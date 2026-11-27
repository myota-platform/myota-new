//! Request-level idempotency: a mutating request made safe to repeat.
//!
//! A caller sends `Idempotency-Key`. The first request with a key runs and its response
//! is stored; a replay with the same key and the same body returns the stored response
//! and does nothing. A replay with the same key and a *different* body is refused,
//! because that is a caller reusing a key for a different operation.
//!
//! ## Why the row is inserted before the handler runs
//!
//! Running the handler and then storing the response has a race: two requests with the
//! same key both miss the lookup and both execute. Inserting a placeholder first makes
//! the second request collide on the primary key, which is a database guarantee rather
//! than a timing assumption. A placeholder never completed (the process died) becomes
//! retryable after [`IN_FLIGHT_TIMEOUT_SECONDS`], so a crash does not wedge a key.

use chrono::{Duration as ChronoDuration, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{store::Store, OpsError, Result};

/// Header name the contract documents.
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

/// How long a claimed-but-unfinished key blocks replays. Longer than any request the edge
/// allows (the gateway's upstream timeout is 10 s), so a normal request never expires its
/// own claim.
pub const IN_FLIGHT_TIMEOUT_SECONDS: i64 = 60;

/// What a caller should do with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    /// The key was free and this request owns it. Run the handler, then
    /// [`Store::save_idempotent_response`].
    Proceed,
    /// The key completed earlier; return the stored response.
    Replay,
}

/// A response stored for replay.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    pub status: i32,
    pub body: serde_json::Value,
}

/// Returns the hash that identifies a request body for idempotency purposes.
///
/// The SHA-256 of the exact bytes. Hashing a re-serialised body would make a semantically
/// identical request with different key order look different, turning a legitimate retry
/// into a conflict.
#[must_use]
pub fn hash_request(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A newtype so the exported name reads as the Go function it mirrors.
#[derive(Debug, Clone, Copy)]
pub struct HashRequest;

impl HashRequest {
    #[must_use]
    pub fn of(body: &[u8]) -> String {
        hash_request(body)
    }
}

impl Store {
    /// Attempts to take ownership of an idempotency key.
    ///
    /// Returns `(Replay, Some(response))` when the key completed with the same body,
    /// `Err(IdempotencyConflict)` when it completed with a different one,
    /// `Err(IdempotencyInFlight)` when another request holds it, and `(Proceed, None)` when
    /// this caller now owns it and must save a response afterwards.
    pub async fn claim_idempotency(
        &self,
        scope: &str,
        key: &str,
        request_hash: &str,
        ttl_seconds: i64,
    ) -> Result<(IdempotencyOutcome, Option<StoredResponse>)> {
        if scope.is_empty() || key.is_empty() {
            return Err(OpsError::Invalid(
                "idempotency needs a scope and a key".into(),
            ));
        }
        let ttl = if ttl_seconds <= 0 {
            86_400
        } else {
            ttl_seconds
        };

        // The placeholder is status 0, which no real response can have, so "claimed but
        // unfinished" is distinguishable from "completed with an empty body".
        let claimed = sqlx::query(
            "INSERT INTO ops.idempotency_record \
             (scope, key, request_hash, response, status, expires_at) \
             VALUES ($1, $2, $3, 'null'::jsonb, 0, $4) \
             ON CONFLICT (scope, key) DO NOTHING",
        )
        .bind(scope)
        .bind(key)
        .bind(request_hash)
        .bind(Utc::now() + ChronoDuration::seconds(ttl))
        .execute(&self.pool)
        .await?;

        if claimed.rows_affected() == 1 {
            return Ok((IdempotencyOutcome::Proceed, None));
        }

        let row = sqlx::query(
            "SELECT request_hash, status, response, created_at \
             FROM ops.idempotency_record WHERE scope = $1 AND key = $2",
        )
        .bind(scope)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            // Deleted between the insert and the read — an expiring record. Treating it as
            // a normal proceed would risk a duplicate.
            return Err(OpsError::IdempotencyInFlight(format!("{scope}/{key}")));
        };

        let stored_hash: String = row.get("request_hash");
        if stored_hash != request_hash {
            return Err(OpsError::IdempotencyConflict(format!("{scope}/{key}")));
        }

        let status: i32 = row.get("status");
        if status == 0 {
            let created_at: chrono::DateTime<Utc> = row.get("created_at");
            if (Utc::now() - created_at).num_seconds() < IN_FLIGHT_TIMEOUT_SECONDS {
                return Err(OpsError::IdempotencyInFlight(format!("{scope}/{key}")));
            }
            // The claimant died. Take the key over rather than blocking forever on a
            // request that will never complete.
            sqlx::query(
                "UPDATE ops.idempotency_record SET created_at = now(), expires_at = $3 \
                 WHERE scope = $1 AND key = $2 AND status = 0",
            )
            .bind(scope)
            .bind(key)
            .bind(Utc::now() + ChronoDuration::seconds(ttl))
            .execute(&self.pool)
            .await?;
            return Ok((IdempotencyOutcome::Proceed, None));
        }

        Ok((
            IdempotencyOutcome::Replay,
            Some(StoredResponse {
                status,
                body: row.get("response"),
            }),
        ))
    }

    /// Records the response for a key this caller claimed.
    pub async fn save_idempotent_response(
        &self,
        scope: &str,
        key: &str,
        status: i32,
        body: &[u8],
    ) -> Result<()> {
        // A non-JSON body is stored as a JSON string so the column stays jsonb.
        let value: serde_json::Value = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => serde_json::Value::String(String::from_utf8_lossy(body).into_owned()),
        };

        let result = sqlx::query(
            "UPDATE ops.idempotency_record SET response = $3, status = $4 \
             WHERE scope = $1 AND key = $2 AND status = 0",
        )
        .bind(scope)
        .bind(key)
        .bind(&value)
        .bind(status)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(OpsError::NoRun(format!(
                "{scope}/{key} is not claimed by this request"
            )));
        }
        Ok(())
    }

    /// Gives a key back when the handler failed in a way the caller may retry.
    ///
    /// Without it a 500 would be stored as the response and every retry would replay the
    /// failure.
    pub async fn release_idempotency(&self, scope: &str, key: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM ops.idempotency_record WHERE scope = $1 AND key = $2 AND status = 0",
        )
        .bind(scope)
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

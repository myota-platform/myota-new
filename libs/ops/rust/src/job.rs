//! Work with competing consumers: a table, not a broker.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::{store::Store, OpsError, Result};

/// One unit of work.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    /// How many times this job has been claimed, including this claim.
    pub attempt: i32,
    pub max_attempts: i32,
    pub available_at: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    pub lease_owner: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

impl Job {
    /// Builds a job with a fresh id and default limits.
    #[must_use]
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: kind.into(),
            payload,
            attempt: 0,
            max_attempts: 8,
            available_at: Utc::now(),
            lease_until: None,
            lease_owner: None,
            scheduled_for: None,
            created_at: Utc::now(),
            last_error: None,
        }
    }

    /// Uses a caller-chosen id. The consumer inserts its job with the *event* id, so
    /// pulling the same event twice inserts once.
    #[must_use]
    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_max_attempts(mut self, attempts: i32) -> Self {
        self.max_attempts = attempts.max(1);
        self
    }

    /// Marks the job as belonging to a scheduled slot, so the partial unique index can
    /// make it fire once per slot rather than once per replica.
    #[must_use]
    pub fn scheduled_for(mut self, slot: DateTime<Utc>) -> Self {
        self.scheduled_for = Some(slot);
        self
    }
}

/// How long to wait before the next attempt.
///
/// Exponential with a cap, and no jitter: one job retries on its own schedule, so there
/// is no herd to spread, and deterministic delays make the tests assertable. The cap
/// matters more than the curve — without it the eighth attempt of a failing import would
/// wait 21 hours, which is indistinguishable from never.
#[derive(Debug, Clone, Copy)]
pub struct Backoff;

impl Backoff {
    /// The delay before attempt `attempt` (1-based).
    #[must_use]
    pub fn delay(attempt: i32) -> ChronoDuration {
        if attempt < 1 {
            return ChronoDuration::zero();
        }
        let shift = (attempt - 1).min(6) as u32;
        let seconds = 2_i64.saturating_mul(1_i64 << shift).min(60);
        ChronoDuration::seconds(seconds)
    }
}

impl Store {
    /// Adds a job and reports whether it was inserted.
    ///
    /// `ON CONFLICT DO NOTHING` makes a duplicate id a no-op rather than an error, which
    /// is what lets a consumer turn a re-delivered event into nothing.
    pub async fn enqueue_job(&self, job: Job) -> Result<(Uuid, bool)> {
        if job.kind.is_empty() {
            return Err(OpsError::Invalid("a job needs a kind".into()));
        }
        let result = sqlx::query(
            "INSERT INTO ops.job (id, kind, payload, max_attempts, available_at, scheduled_for) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(job.id)
        .bind(&job.kind)
        .bind(&job.payload)
        .bind(job.max_attempts)
        .bind(job.available_at)
        .bind(job.scheduled_for)
        .execute(&self.pool)
        .await?;
        Ok((job.id, result.rows_affected() == 1))
    }

    /// Takes up to `count` jobs, leased to this replica.
    ///
    /// The returned jobs are disjoint from any other replica's claim, and each is owned
    /// for the lease duration. A caller that cannot finish in time should extend the
    /// lease rather than let it lapse, because a lapsed lease means another replica starts
    /// the same work.
    pub async fn claim(&self, count: i64) -> Result<Vec<Job>> {
        if count <= 0 {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "UPDATE ops.job \
             SET state = 'leased', lease_owner = $1, lease_until = $2, \
                 attempt = attempt + 1, updated_at = now() \
             WHERE id IN ( \
                 SELECT id FROM ops.job \
                 WHERE (state = 'queued' AND available_at <= now()) \
                    OR (state = 'leased' AND lease_until < now()) \
                 ORDER BY available_at, id \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT $3) \
             RETURNING id, kind, payload, attempt, max_attempts, available_at, \
                       lease_until, lease_owner, scheduled_for, created_at, last_error",
        )
        .bind(&self.owner)
        .bind(self.lease_deadline())
        .bind(count)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Job {
                id: row.get("id"),
                kind: row.get("kind"),
                payload: row.get("payload"),
                attempt: row.get("attempt"),
                max_attempts: row.get("max_attempts"),
                available_at: row.get("available_at"),
                lease_until: row.get("lease_until"),
                lease_owner: row.get("lease_owner"),
                scheduled_for: row.get("scheduled_for"),
                created_at: row.get("created_at"),
                last_error: row.get("last_error"),
            })
            .collect())
    }

    /// Marks a job done. Idempotent: completing an already-done job succeeds.
    pub async fn complete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query(
            "UPDATE ops.job \
             SET state = 'done', lease_owner = NULL, lease_until = NULL, updated_at = now() \
             WHERE id = $1 AND state IN ('queued', 'leased')",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 1 {
            return Ok(());
        }

        let row: Option<(String,)> = sqlx::query_as("SELECT state FROM ops.job WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            None => Err(OpsError::NoRun(format!("no job {id}"))),
            Some((state,)) if state == "done" => Ok(()),
            Some((state,)) => Err(OpsError::NoRun(format!("job {id} is {state}"))),
        }
    }

    /// Records a failed attempt. Returns whether the job is now dead.
    ///
    /// Below the attempt limit the job returns to `queued` with a backoff; at the limit it
    /// becomes `dead` and a dead-letter row is written in the same transaction. One path,
    /// one place a job stops existing.
    pub async fn fail(&self, id: Uuid, reason: &str) -> Result<bool> {
        let delay_worst_case = Backoff::delay(8);

        let mut transaction = self.pool.begin().await?;

        let row: Option<(i32, i32, Value)> = sqlx::query_as(
            "SELECT attempt, max_attempts, payload FROM ops.job WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;

        let Some((attempt, max_attempts, payload)) = row else {
            let _ = transaction.rollback().await;
            return Err(OpsError::NoRun(format!("no job {id}")));
        };

        if attempt >= max_attempts {
            sqlx::query(
                "UPDATE ops.job SET state = 'dead', lease_owner = NULL, lease_until = NULL, \
                 last_error = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(reason)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO ops.dead_letter (id, source_table, ref_id, reason, payload) \
                 VALUES ($1, 'ops.job', $2, $3, $4)",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(reason)
            .bind(&payload)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(true);
        }

        let next = Utc::now() + Backoff::delay(attempt);
        let _ = delay_worst_case;
        sqlx::query(
            "UPDATE ops.job SET state = 'queued', available_at = $2, last_error = $3, \
             lease_owner = NULL, lease_until = NULL, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(next)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(false)
    }

    /// Re-queues jobs whose lease lapsed without a completion or a failure.
    ///
    /// Not required for correctness — `claim` already reclaims expired leases — but it
    /// keeps `state` honest between claims, so a monitoring query that counts `leased`
    /// rows is not counting ghosts.
    pub async fn reap(&self) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE ops.job SET state = 'queued', lease_owner = NULL, lease_until = NULL, \
             updated_at = now() WHERE state = 'leased' AND lease_until < now()",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

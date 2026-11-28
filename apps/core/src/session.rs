//! Sessions: the refresh credential, the row that backs it, and the cookie.
//!
//! A session is the durable half of a sign-in. The access token is a short-lived
//! claim about it; the row is the thing that can be revoked.
//!
//! Three properties the code is shaped around:
//!
//!   * **The refresh token is never stored.** Only its SHA-256 is, so a database
//!     read cannot be turned into a session. The token itself is 32 random bytes
//!     from the OS, encoded base64url, and exists only in the cookie.
//!   * **Idle and absolute expiry are different columns.** `expires_at` is pushed
//!     forward on every rotation; `absolute_expires_at` never moves. A refresh token
//!     that is used every twenty minutes still dies after eight hours.
//!   * **Rotation replaces the token in place.** The row keeps its id, so the
//!     session the console lists is the session it revokes.

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::ClientContext;
use crate::config::Config;
use crate::ADMIN_SESSION_COOKIE;

/// A session as the tables hold it.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// Session id, the `sid` claim.
    pub id: Uuid,
    /// The account.
    pub account_id: Uuid,
    /// `admin` or `participant`.
    pub audience: String,
    /// Idle expiry: pushed forward on rotation.
    pub expires_at: DateTime<Utc>,
    /// Absolute expiry: never moved.
    pub absolute_expires_at: DateTime<Utc>,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
}

/// A freshly minted refresh token: the value for the cookie and the hash for the row.
pub struct RefreshToken {
    /// The token itself. Goes in the cookie and nowhere else.
    pub token: String,
    /// SHA-256 of the token, lowercase hex. Goes in the row.
    pub hash: String,
}

/// Generates a refresh token and the hash to store.
///
/// 32 bytes from the OS, not a UUID: a UUIDv4 carries 122 bits and a refresh token
/// is the one credential that lives for eight hours.
#[must_use]
pub fn new_refresh_token() -> RefreshToken {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
    let hash = hash_refresh_token(&token);
    RefreshToken { token, hash }
}

/// The stored form of a refresh token.
///
/// SHA-256 rather than Argon2: the input is 256 bits of uniform randomness, so there
/// is nothing to guess and a slow hash would only add latency to every refresh.
/// Argon2 exists for passwords because passwords are guessable; this is not one.
#[must_use]
pub fn hash_refresh_token(token: &str) -> String {
    crate::hex_lower(&Sha256::digest(token.as_bytes()))
}

/// Builds the `Set-Cookie` value for an administrator session.
///
/// `__Host-` requires `Secure`, requires no `Domain`, and requires `Path=/`; a
/// browser rejects the cookie otherwise, which turns a header mistake into "login
/// does nothing" rather than a security hole. `HttpOnly` keeps it out of reach of
/// script, and `SameSite=Lax` is what stops a cross-site form from riding the
/// session — the double-submit token the contract also mentions is belt and braces
/// on top of this.
#[must_use]
pub fn session_cookie(token: &str, max_age: Duration) -> String {
    format!(
        "{ADMIN_SESSION_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
        max_age.num_seconds().max(0)
    )
}

/// The `Set-Cookie` value that removes the session cookie.
#[must_use]
pub fn cleared_session_cookie() -> String {
    format!("{ADMIN_SESSION_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// Extracts a cookie from a `Cookie` header.
///
/// A tiny parser rather than a crate: the header is a semicolon-separated list of
/// `name=value` pairs, the name is a constant of this service, and a dependency
/// that also parses the cookie's attributes would parse attributes the browser
/// already validated.
#[must_use]
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim())
    })
}

/// Creates a session and returns it with its refresh token.
///
/// The access-token id is supplied by the caller rather than generated here,
/// because the same value has to be minted into the token and stored on the row:
/// logout revokes the id it read from the token, and a second random id would
/// revoke nothing.
pub async fn create(
    pool: &PgPool,
    config: &Config,
    account_id: Uuid,
    audience: &str,
    client: &ClientContext,
    access_jti: Uuid,
) -> Result<(SessionRow, RefreshToken), sqlx::Error> {
    let refresh = new_refresh_token();
    let session_id = Uuid::new_v4();
    let now = Utc::now();
    let idle_expiry = now + Duration::seconds(config.refresh_idle.as_secs() as i64);
    let absolute_expiry = now + Duration::seconds(config.refresh_absolute.as_secs() as i64);

    sqlx::query(
        "INSERT INTO auth_session
             (id, account_id, audience, refresh_token_hash, access_jti,
              expires_at, absolute_expires_at, user_agent, remote_addr, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::inet, $10)",
    )
    .bind(session_id)
    .bind(account_id)
    .bind(audience)
    .bind(&refresh.hash)
    .bind(access_jti)
    .bind(idle_expiry)
    .bind(absolute_expiry)
    .bind(client.user_agent.as_deref())
    .bind(client.ip.map(|ip| ip.to_string()))
    .bind(now)
    .execute(pool)
    .await?;

    Ok((
        SessionRow {
            id: session_id,
            account_id,
            audience: audience.to_string(),
            expires_at: idle_expiry,
            absolute_expires_at: absolute_expiry,
            created_at: now,
        },
        refresh,
    ))
}

/// Finds an open session by the hash of its refresh token.
///
/// "Open" is three conditions: not revoked, inside the idle window, inside the
/// absolute window. All three are in SQL rather than in Rust so a replica with a
/// skewed clock cannot extend a session the database considers dead.
pub async fn find_open_by_refresh_hash(
    pool: &PgPool,
    refresh_hash: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, account_id, audience, expires_at, absolute_expires_at, created_at
           FROM auth_session
          WHERE refresh_token_hash = $1
            AND revoked_at IS NULL
            AND expires_at > now()
            AND absolute_expires_at > now()",
    )
    .bind(refresh_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(id, account_id, audience, expires_at, absolute_expires_at, created_at)| SessionRow {
            id,
            account_id,
            audience,
            expires_at,
            absolute_expires_at,
            created_at,
        },
    ))
}

/// Finds an open session by id, for a bearer token.
pub async fn find_open_by_id(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, account_id, audience, expires_at, absolute_expires_at, created_at
           FROM auth_session
          WHERE id = $1
            AND revoked_at IS NULL
            AND expires_at > now()
            AND absolute_expires_at > now()",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(id, account_id, audience, expires_at, absolute_expires_at, created_at)| SessionRow {
            id,
            account_id,
            audience,
            expires_at,
            absolute_expires_at,
            created_at,
        },
    ))
}

/// Rotates the refresh token of an open session and returns the new one.
///
/// The `WHERE` clause repeats the liveness conditions, so a rotation racing a
/// revocation loses rather than resurrecting a dead session. A rotation that
/// matches no row returns `None` and the caller answers 401.
pub async fn rotate(
    pool: &PgPool,
    config: &Config,
    session_id: Uuid,
    new_refresh: &RefreshToken,
    new_access_jti: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let row = sqlx::query_as::<_, (DateTime<Utc>,)>(
        "UPDATE auth_session
            SET refresh_token_hash = $2,
                access_jti = $3,
                last_seen_at = now(),
                updated_at = now(),
                expires_at = LEAST(now() + make_interval(secs => $4), absolute_expires_at)
          WHERE id = $1
            AND revoked_at IS NULL
            AND expires_at > now()
            AND absolute_expires_at > now()
      RETURNING expires_at",
    )
    .bind(session_id)
    .bind(&new_refresh.hash)
    .bind(new_access_jti)
    .bind(config.refresh_idle.as_secs() as f64)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(expires_at,)| expires_at))
}

/// Revokes a session.
///
/// Idempotent: revoking twice leaves the first revocation time in place, because
/// "when did this end" is the fact worth keeping.
pub async fn revoke(pool: &PgPool, session_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE auth_session
            SET revoked_at = COALESCE(revoked_at, now()), updated_at = now()
          WHERE id = $1",
    )
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records a token id as revoked until it would have expired anyway.
///
/// `auth_revocation` is a denylist with an expiry: an access token is useless after
/// ten minutes, so the row is too.
pub async fn revoke_token(
    pool: &PgPool,
    jti: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO auth_revocation (jti, expires_at)
         VALUES ($1, $2)
         ON CONFLICT (jti) DO NOTHING",
    )
    .bind(jti)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether a token id has been revoked.
pub async fn is_revoked(pool: &PgPool, jti: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM auth_revocation WHERE jti = $1 AND expires_at > now())",
    )
    .bind(jti)
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_long_and_unique() {
        let first = new_refresh_token();
        let second = new_refresh_token();
        assert_ne!(first.token, second.token);
        assert_eq!(first.token.len(), 43, "32 bytes base64url without padding");
        assert_eq!(first.hash.len(), 64, "sha256 as lowercase hex");
    }

    #[test]
    fn the_hash_is_stable_and_the_token_is_not_stored_in_it() {
        let refresh = new_refresh_token();
        assert_eq!(hash_refresh_token(&refresh.token), refresh.hash);
        assert!(!refresh.hash.contains(&refresh.token));
    }

    #[test]
    fn the_cookie_carries_the_attributes_the_host_prefix_requires() {
        let cookie = session_cookie("abc", Duration::seconds(28800));
        assert!(
            cookie.starts_with("__Host-myota_admin_sid=abc;"),
            "{cookie}"
        );
        assert!(cookie.contains("Path=/"), "{cookie}");
        assert!(cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        assert!(cookie.contains("Max-Age=28800"), "{cookie}");
        assert!(
            !cookie.contains("Domain"),
            "a __Host- cookie must not set Domain: {cookie}"
        );
    }

    #[test]
    fn clearing_the_cookie_expires_it_immediately() {
        let cookie = cleared_session_cookie();
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
        assert!(cookie.contains("__Host-myota_admin_sid=;"), "{cookie}");
    }

    #[test]
    fn a_cookie_header_is_parsed_by_name() {
        let header = "a=1; __Host-myota_admin_sid=token-value; b=2";
        assert_eq!(
            cookie_value(header, ADMIN_SESSION_COOKIE),
            Some("token-value")
        );
        assert_eq!(cookie_value(header, "__Host-myota_sid"), None);
        assert_eq!(
            cookie_value("__Host-myota_admin_sid=x", ADMIN_SESSION_COOKIE),
            Some("x")
        );
        assert_eq!(cookie_value("", ADMIN_SESSION_COOKIE), None);
    }

    #[test]
    fn a_cookie_name_is_matched_exactly_not_by_prefix() {
        // The participant cookie name contains the admin one as a suffix; a prefix
        // match would confuse the two, which is the whole point of separate names.
        let header = "__Host-myota_sid=participant-token";
        assert_eq!(cookie_value(header, ADMIN_SESSION_COOKIE), None);
        assert_eq!(
            cookie_value(header, "__Host-myota_sid"),
            Some("participant-token")
        );
    }
}

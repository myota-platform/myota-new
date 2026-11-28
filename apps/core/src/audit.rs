//! The audit log: `security_event`.
//!
//! Every sign-in outcome, refusal, lockout and enrolment writes a row. The table is
//! append-only and indexed by account and by type, which is what makes two very
//! different questions answerable: "what happened to this account" and "is someone
//! trying passwords against us".
//!
//! ## Why audit failures do not fail the request
//!
//! A failed insert is a database problem, and the sign-in has already happened. The
//! alternatives are worse: refusing a valid sign-in because the *audit* row could
//! not be written turns a logging outage into an authentication outage, and
//! silently dropping the error leaves no trace. So the error is logged at `error`
//! with the event type and the account, which is a signal an operator can act on,
//! and the request proceeds.
//!
//! ## Event types
//!
//! Names are `<subject>.<past-tense fact>` in the same shape as the outbox events,
//! so a reader who knows one naming convention knows both. The set is documented in
//! `docs/architecture/auth-model.md`; `security_event` is an audit table, not the
//! outbox, so it is not part of the event catalogue that `tools/check_events.py`
//! validates.
//!
//! | Type | When |
//! | --- | --- |
//! | `auth.password_verified` | the password was right |
//! | `auth.login_failed` | wrong password, or no such account |
//! | `auth.lockout` | the wrong-password count reached the threshold |
//! | `auth.refused_status` | the account is not active |
//! | `auth.refused_no_admin_role` | the account holds no administrator role |
//! | `auth.refused_no_mfa` | the account has no confirmed second factor |
//! | `auth.challenge_issued` | a second-factor challenge was created |
//! | `auth.challenge_failed` | a wrong second-factor code |
//! | `auth.challenge_burned` | too many wrong codes; the challenge is spent |
//! | `auth.session_issued` | a session was created |
//! | `auth.refresh_failed` | a refresh token that is not a session |
//! | `auth.session_revoked` | sign-out, or the session was revoked |
//! | `auth.mfa_enrolment_started` | a pending TOTP factor was written |
//! | `auth.mfa_enrolled` | the factor was confirmed with a code |

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// The event types above, as constants so a typo is a compile error rather than a
/// row nothing queries for.
pub mod event {
    /// The password was correct.
    pub const PASSWORD_VERIFIED: &str = "auth.password_verified";
    /// Wrong password, or an address with no account.
    pub const LOGIN_FAILED: &str = "auth.login_failed";
    /// The failure count reached the threshold and the account locked.
    pub const LOCKOUT: &str = "auth.lockout";
    /// The account exists and the password was right, but its status forbids entry.
    pub const REFUSED_STATUS: &str = "auth.refused_status";
    /// The password was right and the account holds no administrator role.
    pub const REFUSED_NO_ADMIN_ROLE: &str = "auth.refused_no_admin_role";
    /// The password was right and no second factor is confirmed.
    pub const REFUSED_NO_MFA: &str = "auth.refused_no_mfa";
    /// A second-factor challenge was created.
    pub const CHALLENGE_ISSUED: &str = "auth.challenge_issued";
    /// A second-factor code was wrong.
    pub const CHALLENGE_FAILED: &str = "auth.challenge_failed";
    /// Too many wrong codes; the challenge is spent.
    pub const CHALLENGE_BURNED: &str = "auth.challenge_burned";
    /// A session was created.
    pub const SESSION_ISSUED: &str = "auth.session_issued";
    /// A refresh token that matched no open session.
    pub const REFRESH_FAILED: &str = "auth.refresh_failed";
    /// A session was revoked.
    pub const SESSION_REVOKED: &str = "auth.session_revoked";
    /// A pending TOTP factor was written.
    pub const MFA_ENROLMENT_STARTED: &str = "auth.mfa_enrolment_started";
    /// The factor was confirmed.
    pub const MFA_ENROLLED: &str = "auth.mfa_enrolled";
}

/// Writes one audit row.
///
/// Never returns an error to the caller — see the module comment. The payload is a
/// JSON object with whatever explains the decision; it must never contain a
/// password, a code, a token or a seed.
pub async fn record(pool: &PgPool, account_id: Option<Uuid>, event_type: &str, payload: Value) {
    let result = sqlx::query(
        "INSERT INTO security_event (id, account_id, event_type, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(event_type)
    .bind(payload)
    .execute(pool)
    .await;

    if let Err(error) = result {
        tracing::error!(
            %error,
            event_type,
            account_id = account_id.map(|id| id.to_string()).as_deref().unwrap_or("-"),
            "could not write the security event; the request continues"
        );
    }
}

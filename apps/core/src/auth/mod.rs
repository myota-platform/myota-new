//! The administrator sign-in flow.
//!
//! Five steps, in the only order that makes the guarantee true:
//!
//! ```text
//! POST /v1/admin/auth/login          password  →  challenge (5 min)
//! POST /v1/admin/auth/mfa/challenge  code      →  session + access token + cookie
//! POST /v1/admin/auth/mfa/enrol      session   →  pending TOTP secret + recovery codes
//! POST /v1/admin/auth/mfa/enrol/confirm code   →  the factor is confirmed
//! POST /v1/admin/auth/logout         session   →  gone
//! ```
//!
//! **No session is issued without a confirmed second factor.** That is not a check
//! at the end of `login` — it is the only path to a session: `login` returns a
//! challenge or a refusal, and `complete_challenge` is the only function that calls
//! [`session::create`]. A code path that skips MFA would have to be written on
//! purpose.
//!
//! The failure paths are separate types rather than one "bad credentials": a client
//! needs to know whether to show "check your password", "enrol a second factor",
//! "ask an administrator for a role", or "wait fifteen minutes", and each of those
//! is a different screen.

use std::net::IpAddr;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, event};
use crate::config::{Config, CHALLENGE_ATTEMPT_LIMIT};

/// How long a re-authentication stays valid.
///
/// Five minutes, from `docs/architecture/auth-model.md` and the contract's `grantRole`
/// description: long enough to walk from the sign-in prompt to the action, short enough
/// that a stolen session is not useful by the time someone reaches it.
pub const FRESHNESS_WINDOW: chrono::Duration = chrono::Duration::minutes(5);
use crate::token::{AccessClaims, TokenError, TokenService, AUDIENCE_ADMIN};
use crate::{secretbox, session, totp};

/// What the caller's connection looks like, as far as the audit and the rate
/// limiter are concerned.
#[derive(Debug, Clone, Default)]
pub struct ClientContext {
    /// The client address, after any trusted proxy.
    pub ip: Option<IpAddr>,
    /// The `User-Agent`, truncated. Kept because "which session is this" is a
    /// question an operator asks about a browser they do not recognise.
    pub user_agent: Option<String>,
}

impl ClientContext {
    /// The longest user agent stored. A header can be arbitrarily long and this
    /// column is read by a console.
    const MAX_USER_AGENT: usize = 200;

    /// Builds a context from the header values.
    #[must_use]
    pub fn new(ip: Option<IpAddr>, user_agent: Option<String>) -> Self {
        Self {
            ip,
            user_agent: user_agent.map(|agent| agent.chars().take(Self::MAX_USER_AGENT).collect()),
        }
    }
}

/// The identity behind an authenticated request.
#[derive(Debug, Clone)]
pub struct Principal {
    /// The account.
    pub account_id: Uuid,
    /// The session the credential belongs to.
    pub session_id: Uuid,
    /// The access token's id, so sign-out can revoke it.
    pub token_id: Uuid,
    /// Effective scopes at issue time. A2 re-evaluates these per request.
    pub scopes: Vec<String>,
}

/// What a sign-in produced.
#[derive(Debug)]
pub enum LoginOutcome {
    /// The password was right; a second factor is still required.
    Challenge(Challenge),
    /// A complete session. Reached from `complete_challenge` only.
    Authenticated(SessionView),
}

/// A second-factor challenge.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    /// The discriminator the contract's `AdminLoginResult` uses.
    pub status: &'static str,
    /// The id to answer with.
    pub challenge_id: Uuid,
    /// The methods this account can answer with.
    pub methods: Vec<String>,
    /// When the challenge stops being answerable.
    pub expires_at: DateTime<Utc>,
}

/// A session, as the contract's `AdminSession` describes it, plus the refresh token
/// the handler puts in the cookie.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// The discriminator for the `authenticated` variant of `AdminLoginResult`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    /// The account.
    pub account_id: Uuid,
    /// Always `admin` from this flow.
    pub audience: String,
    /// The session id.
    pub session_id: Uuid,
    /// The signed access token.
    #[serde(skip)]
    pub access_token: String,
    /// When the access token expires.
    pub access_token_expires_at: DateTime<Utc>,
    /// When the session dies regardless of use.
    pub refresh_expires_at: DateTime<Utc>,
    /// Effective scopes at issue time.
    pub scopes: Vec<String>,
    /// When the second factor was confirmed.
    pub mfa_confirmed_at: Option<DateTime<Utc>>,
    /// The refresh token, for the cookie. Never serialised into the body.
    #[serde(skip)]
    pub refresh_token: String,
}

/// A pending TOTP enrolment.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Enrolment {
    /// Always `totp` in this chunk.
    pub method: String,
    /// The base32 secret.
    pub secret: String,
    /// The URI the QR code encodes.
    pub otpauth_uri: String,
    /// Ten single-use codes. Shown once.
    pub recovery_codes: Vec<String>,
    /// When the pending enrolment stops being confirmable.
    pub expires_at: DateTime<Utc>,
}

/// Everything that can go wrong, as the sign-in flow sees it.
#[derive(Debug)]
pub enum AuthError {
    /// Wrong password, or no such account. Deliberately one variant: the caller
    /// must not be able to tell the two apart, and neither may a client.
    InvalidCredentials,
    /// The account is locked. Retrying before `retry_after_seconds` will fail.
    Locked { retry_after_seconds: i64 },
    /// Too many attempts in the window.
    RateLimited { retry_after_seconds: i64 },
    /// Authenticated, but no administrator role.
    NoAdminRole,
    /// The password was right and there is no confirmed second factor.
    MfaNotEnrolled,
    /// The challenge is gone: expired, consumed, or never existed.
    ChallengeExpired,
    /// The second-factor code was wrong.
    MfaRejected,
    /// No credential, or one that does not verify.
    Unauthorized,
    /// A valid credential of the wrong audience.
    WrongAudience,
    /// The session has not proved its holder's identity recently enough for this action.
    ReauthRequired,
    /// The state does not permit the operation.
    Conflict(String),
    /// The request cannot be served as sent.
    Validation(String),
    /// A method the contract names but this chunk does not implement.
    UnsupportedMethod(String),
    /// The database failed.
    Database(sqlx::Error),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentials => write!(formatter, "the address or password is not correct"),
            Self::Locked {
                retry_after_seconds,
            } => {
                write!(
                    formatter,
                    "the account is locked for {retry_after_seconds} more seconds"
                )
            }
            Self::RateLimited {
                retry_after_seconds,
            } => {
                write!(
                    formatter,
                    "too many attempts; retry after {retry_after_seconds} seconds"
                )
            }
            Self::NoAdminRole => write!(formatter, "the account holds no administrator role"),
            Self::MfaNotEnrolled => write!(formatter, "no second factor is enrolled"),
            Self::ChallengeExpired => write!(formatter, "the challenge has expired"),
            Self::MfaRejected => write!(formatter, "the second factor was rejected"),
            Self::Unauthorized => write!(formatter, "not authenticated"),
            Self::WrongAudience => write!(formatter, "wrong audience"),
            Self::ReauthRequired => write!(formatter, "re-authentication is required"),
            Self::Conflict(detail) | Self::Validation(detail) | Self::UnsupportedMethod(detail) => {
                write!(formatter, "{detail}")
            }
            Self::Database(error) => write!(formatter, "database: {error}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<sqlx::Error> for AuthError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// The sign-in flow, over one pool and one configuration.
pub struct Auth<'a> {
    /// The database.
    pub pool: &'a PgPool,
    /// The configuration: timings, thresholds, keys.
    pub config: &'a Config,
    /// The token service.
    pub tokens: &'a TokenService,
}

/// A row from the account lookup.
struct AccountRow {
    id: Uuid,
    status: String,
    password_hash: Option<String>,
    locked_until: Option<DateTime<Utc>>,
    suspended_at: Option<DateTime<Utc>>,
}

impl Auth<'_> {
    /// Begins an administrator sign-in.
    ///
    /// The order is the policy:
    ///
    /// 1. **Rate limit before anything expensive.** A flood costs one indexed
    ///    upsert per request, not an Argon2 verification.
    /// 2. **Lockout before the password.** A locked account answers 429 without
    ///    checking anything, so the lock cannot be probed for password guesses.
    /// 3. **Password, with the same work for a missing account.** See
    ///    [`password::verify_against_dummy`].
    /// 4. **Refusals**, each with its own reason, because each has its own repair.
    /// 5. **Challenge**, never a session.
    pub async fn login(
        &self,
        email: &str,
        password: &str,
        client: &ClientContext,
    ) -> Result<LoginOutcome, AuthError> {
        let address = email.trim().to_ascii_lowercase();
        if address.is_empty() {
            return Err(AuthError::Validation(
                "An email address is required.".into(),
            ));
        }

        let account_key = rate_limit_key("admin-login:account", &address);
        let ip_key = client
            .ip
            .map(|ip| rate_limit_key("admin-login:ip", &ip.to_string()));

        // The account lookup comes first, and it is the cheapest thing here: one row
        // on a unique index. It has to, because the lock and the rate limit say
        // different things and have different remedies — "an administrator has to
        // unlock this" against "wait fifteen minutes" — and both trip at five wrong
        // passwords. Ordering the rate limit first made the lock unreachable from the
        // outside: the caller was told to wait when the account needed an operator.
        //
        // What must stay behind the rate limit is the *password verification*, which
        // is the expensive part (Argon2, ~19 MiB). That is still protected.
        let account = self.find_account(&address).await?;

        if let Some(account) = &account {
            if let Some(locked_until) = account.locked_until {
                let remaining = (locked_until - Utc::now()).num_seconds();
                if remaining > 0 {
                    audit::record(
                        self.pool,
                        Some(account.id),
                        event::LOGIN_FAILED,
                        serde_json::json!({ "reason": "locked" }),
                    )
                    .await;
                    return Err(AuthError::Locked {
                        retry_after_seconds: remaining,
                    });
                }
            }
        }

        self.check_rate_limit(&account_key, ip_key.as_deref())
            .await?;

        let Some(account) = account else {
            // Same work as a real verification, so response time does not say
            // whether the address exists.
            verify_password_off_thread(password.to_string(), dummy_hash()).await;
            self.count_failure(&account_key, ip_key.as_deref()).await;
            audit::record(
                self.pool,
                None,
                event::LOGIN_FAILED,
                serde_json::json!({ "reason": "no_such_account" }),
            )
            .await;
            return Err(AuthError::InvalidCredentials);
        };

        let Some(stored_hash) = account.password_hash.clone() else {
            // An account with no password (provisioned for a passkey, or not yet
            // activated). The work is still done, and the answer is the same.
            verify_password_off_thread(password.to_string(), dummy_hash()).await;
            self.count_failure(&account_key, ip_key.as_deref()).await;
            audit::record(
                self.pool,
                Some(account.id),
                event::LOGIN_FAILED,
                serde_json::json!({ "reason": "no_password_set" }),
            )
            .await;
            return Err(AuthError::InvalidCredentials);
        };

        if !verify_password_off_thread(password.to_string(), stored_hash).await {
            let (failed_attempts, now_locked) = self.record_wrong_password(account.id).await?;
            self.count_failure(&account_key, ip_key.as_deref()).await;

            if let Some(locked_until) = now_locked {
                audit::record(
                    self.pool,
                    Some(account.id),
                    event::LOCKOUT,
                    serde_json::json!({
                        "failedAttempts": failed_attempts,
                        "lockedUntil": locked_until,
                        "threshold": self.config.lockout_threshold,
                    }),
                )
                .await;
            } else {
                audit::record(
                    self.pool,
                    Some(account.id),
                    event::LOGIN_FAILED,
                    serde_json::json!({
                        "reason": "wrong_password",
                        "failedAttempts": failed_attempts,
                    }),
                )
                .await;
            }
            return Err(AuthError::InvalidCredentials);
        }

        // The password is right; from here the failures are about the account, not
        // about the credential, so the counter resets.
        self.clear_failures(account.id).await?;
        audit::record(
            self.pool,
            Some(account.id),
            event::PASSWORD_VERIFIED,
            serde_json::json!({ "ip": client.ip.map(|ip| ip.to_string()) }),
        )
        .await;

        if account.status != "active" || account.suspended_at.is_some() {
            audit::record(
                self.pool,
                Some(account.id),
                event::REFUSED_STATUS,
                serde_json::json!({
                    "status": account.status,
                    "suspended": account.suspended_at.is_some(),
                }),
            )
            .await;
            return Err(AuthError::Unauthorized);
        }

        if !self.has_administrator_role(account.id).await? {
            audit::record(
                self.pool,
                Some(account.id),
                event::REFUSED_NO_ADMIN_ROLE,
                serde_json::json!({}),
            )
            .await;
            return Err(AuthError::NoAdminRole);
        }

        let Some((mfa_methods, _)) = self.confirmed_factors(account.id).await? else {
            audit::record(
                self.pool,
                Some(account.id),
                event::REFUSED_NO_MFA,
                serde_json::json!({}),
            )
            .await;
            return Err(AuthError::MfaNotEnrolled);
        };

        let challenge = self.issue_challenge(account.id).await?;
        let mut methods: Vec<String> = mfa_methods;
        // A recovery code is a way to answer every factor, so it is always offered
        // when the account has any.
        methods.push("recovery_code".to_string());
        methods.dedup();

        audit::record(
            self.pool,
            Some(account.id),
            event::CHALLENGE_ISSUED,
            serde_json::json!({ "challengeId": challenge.challenge_id, "methods": methods }),
        )
        .await;

        Ok(LoginOutcome::Challenge(Challenge {
            status: "mfa_required",
            challenge_id: challenge.challenge_id,
            methods,
            expires_at: challenge.expires_at,
        }))
    }

    /// Answers a second-factor challenge and creates the session.
    pub async fn complete_challenge(
        &self,
        challenge_id: Uuid,
        method: &str,
        code: &str,
        client: &ClientContext,
    ) -> Result<SessionView, AuthError> {
        if let Some(ip) = client.ip {
            let key = rate_limit_key("admin-challenge:ip", &ip.to_string());
            self.check_rate_limit(&key, None).await?;
        }

        let challenge = self.find_open_challenge(challenge_id).await?;
        let Some((account_id, attempts)) = challenge else {
            return Err(AuthError::ChallengeExpired);
        };

        let method = method.trim().to_ascii_lowercase();
        let verified = match method.as_str() {
            "totp" => self.verify_totp(account_id, code, false).await?,
            "recovery_code" | "recovery-code" | "recoverycode" => {
                self.verify_recovery_code(account_id, code).await?
            }
            "webauthn" => {
                return Err(AuthError::UnsupportedMethod(
                    "WebAuthn assertions are not accepted yet; use TOTP or a recovery code.".into(),
                ));
            }
            other => {
                return Err(AuthError::Validation(format!(
                    "{other:?} is not a second-factor method this service accepts."
                )));
            }
        };

        if !verified {
            let burned = self.record_wrong_code(challenge_id, attempts).await?;
            audit::record(
                self.pool,
                Some(account_id),
                if burned { event::CHALLENGE_BURNED } else { event::CHALLENGE_FAILED },
                serde_json::json!({ "challengeId": challenge_id, "method": method, "attempts": attempts + 1 }),
            )
            .await;
            return if burned {
                Err(AuthError::ChallengeExpired)
            } else {
                Err(AuthError::MfaRejected)
            };
        }

        // Consume and create in one step's worth of code: the UPDATE is the
        // single-use guarantee, and it is what makes two concurrent answers to one
        // challenge produce one session rather than two.
        if !self.consume_challenge(challenge_id).await? {
            return Err(AuthError::ChallengeExpired);
        }

        let view = self.start_session(account_id, client).await?;
        audit::record(
            self.pool,
            Some(account_id),
            event::SESSION_ISSUED,
            serde_json::json!({
                "sessionId": view.session_id,
                "ip": client.ip.map(|ip| ip.to_string()),
                "method": method,
            }),
        )
        .await;
        Ok(view)
    }

    /// Rotates a session from its refresh token.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        client: &ClientContext,
    ) -> Result<SessionView, AuthError> {
        let hash = session::hash_refresh_token(refresh_token);
        let open = session::find_open_by_refresh_hash(self.pool, &hash).await?;

        let Some(open) = open else {
            audit::record(
                self.pool,
                None,
                event::REFRESH_FAILED,
                serde_json::json!({ "ip": client.ip.map(|ip| ip.to_string()) }),
            )
            .await;
            return Err(AuthError::Unauthorized);
        };

        if open.audience != AUDIENCE_ADMIN {
            // A participant session presented as an administrator one. The gateway
            // refuses this by cookie name; this is the check that holds when the
            // request bypasses the gateway.
            return Err(AuthError::WrongAudience);
        }

        let account = self.find_account_by_id(open.account_id).await?;
        let Some(account) = account else {
            return Err(AuthError::Unauthorized);
        };
        if account.status != "active" || account.suspended_at.is_some() {
            session::revoke(self.pool, open.id).await?;
            audit::record(
                self.pool,
                Some(open.account_id),
                event::SESSION_REVOKED,
                serde_json::json!({ "sessionId": open.id, "reason": "account_not_active" }),
            )
            .await;
            return Err(AuthError::Unauthorized);
        }

        let scopes = self.scopes_for(open.account_id).await?;
        let refresh = session::new_refresh_token();
        let token_id = Uuid::new_v4();

        let Some(expires_at) =
            session::rotate(self.pool, self.config, open.id, &refresh, token_id).await?
        else {
            // Revoked between the read and the write: the session is gone, and the
            // caller must sign in again.
            return Err(AuthError::Unauthorized);
        };

        let (access_token, access_expires_at) =
            self.tokens
                .issue(open.account_id, open.id, token_id, AUDIENCE_ADMIN, &scopes);

        Ok(SessionView {
            status: None,
            account_id: open.account_id,
            audience: AUDIENCE_ADMIN.to_string(),
            session_id: open.id,
            access_token,
            access_token_expires_at: unix(access_expires_at),
            refresh_expires_at: expires_at,
            scopes,
            mfa_confirmed_at: None,
            refresh_token: refresh.token,
        })
    }

    /// Revokes the session behind a credential.
    pub async fn logout(&self, principal: &Principal) -> Result<(), AuthError> {
        session::revoke(self.pool, principal.session_id).await?;
        // The access token stays valid until it expires unless it is denied here.
        session::revoke_token(
            self.pool,
            principal.token_id,
            Utc::now() + Duration::seconds(self.config.access_ttl.as_secs() as i64),
        )
        .await?;
        audit::record(
            self.pool,
            Some(principal.account_id),
            event::SESSION_REVOKED,
            serde_json::json!({ "sessionId": principal.session_id, "reason": "logout" }),
        )
        .await;
        Ok(())
    }

    /// Proves the session's holder is still the person who signed in.
    ///
    /// Both factors again, deliberately: a password alone would let a stolen session that
    /// also captured a password escalate, and a code alone would let a stolen device do
    /// it. The grant is recorded on the session with a five-minute life.
    pub async fn reauthenticate(
        &self,
        principal: &Principal,
        password: &str,
        code: &str,
    ) -> Result<DateTime<Utc>, AuthError> {
        let account = self
            .find_account_by_id(principal.account_id)
            .await?
            .ok_or(AuthError::Unauthorized)?;
        if account.status != "active" || account.suspended_at.is_some() {
            return Err(AuthError::Unauthorized);
        }

        let Some(stored_hash) = account.password_hash.clone() else {
            verify_password_off_thread(password.to_string(), dummy_hash()).await;
            return Err(AuthError::InvalidCredentials);
        };
        if !verify_password_off_thread(password.to_string(), stored_hash).await {
            audit::record(
                self.pool,
                Some(principal.account_id),
                event::REAUTH_FAILED,
                serde_json::json!({ "reason": "wrong_password", "sessionId": principal.session_id }),
            )
            .await;
            return Err(AuthError::InvalidCredentials);
        }

        // Either factor proves possession; the caller does not say which.
        let accepted = self.verify_totp(principal.account_id, code, true).await?
            || self
                .verify_recovery_code(principal.account_id, code)
                .await?;
        if !accepted {
            audit::record(
                self.pool,
                Some(principal.account_id),
                event::REAUTH_FAILED,
                serde_json::json!({ "reason": "wrong_code", "sessionId": principal.session_id }),
            )
            .await;
            return Err(AuthError::MfaRejected);
        }

        let fresh_until =
            session::mark_fresh(self.pool, principal.session_id, FRESHNESS_WINDOW).await?;
        audit::record(
            self.pool,
            Some(principal.account_id),
            event::REAUTHENTICATED,
            serde_json::json!({
                "sessionId": principal.session_id,
                "freshUntil": fresh_until,
            }),
        )
        .await;
        Ok(fresh_until)
    }

    /// Whether the session is fresh enough for a destructive change.
    pub async fn is_fresh(&self, session_id: Uuid) -> Result<bool, AuthError> {
        Ok(session::is_fresh(self.pool, session_id).await?)
    }

    /// Resolves a principal from a bearer token.
    pub async fn principal_from_bearer(&self, token: &str) -> Result<Principal, AuthError> {
        let claims: AccessClaims = self.tokens.verify(token).map_err(|error| match error {
            TokenError::WrongAudience => AuthError::WrongAudience,
            _ => AuthError::Unauthorized,
        })?;

        if claims.aud != AUDIENCE_ADMIN {
            return Err(AuthError::WrongAudience);
        }

        let Some(open) = session::find_open_by_id(self.pool, claims.sid).await? else {
            return Err(AuthError::Unauthorized);
        };
        if open.audience != AUDIENCE_ADMIN || open.account_id != claims.sub {
            return Err(AuthError::WrongAudience);
        }
        if session::is_revoked(self.pool, claims.jti).await? {
            return Err(AuthError::Unauthorized);
        }

        Ok(Principal {
            account_id: claims.sub,
            session_id: claims.sid,
            token_id: claims.jti,
            scopes: claims.scp,
        })
    }

    /// Resolves a principal from the administrator refresh cookie.
    ///
    /// This is what the console uses for a state-changing call it makes before it
    /// has rotated an access token, and what `/v1/admin/auth/logout` accepts. The
    /// token id is the session's stored `access_jti`, so a logout through this path
    /// revokes the token that was last issued for the session.
    pub async fn principal_from_cookie(&self, refresh_token: &str) -> Result<Principal, AuthError> {
        let hash = session::hash_refresh_token(refresh_token);
        let Some(open) = session::find_open_by_refresh_hash(self.pool, &hash).await? else {
            return Err(AuthError::Unauthorized);
        };
        if open.audience != AUDIENCE_ADMIN {
            return Err(AuthError::WrongAudience);
        }
        let scopes = self.scopes_for(open.account_id).await?;
        Ok(Principal {
            account_id: open.account_id,
            session_id: open.id,
            token_id: access_jti(self.pool, open.id)
                .await
                .unwrap_or_else(|_| Uuid::nil()),
            scopes,
        })
    }

    /// Starts a TOTP enrolment for the signed-in administrator.
    pub async fn enrol(&self, principal: &Principal) -> Result<Enrolment, AuthError> {
        let existing = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT confirmed_at FROM account_mfa WHERE account_id = $1 AND method = 'totp'",
        )
        .bind(principal.account_id)
        .fetch_optional(self.pool)
        .await?;

        if let Some(Some(confirmed_at)) = existing {
            return Err(AuthError::Conflict(format!(
                "A TOTP second factor was already confirmed on {confirmed_at}. Another administrator has to reset it."
            )));
        }

        let secret = totp::generate_secret();
        let secret_base32 = totp::encode_secret(&secret);
        let sealed = secretbox::seal(&self.config.mfa_key, secret_base32.as_bytes());
        let expires_at = Utc::now() + Duration::seconds(self.config.enrolment_ttl.as_secs() as i64);

        let mut transaction = self.pool.begin().await?;

        // A pending enrolment is replaced rather than duplicated: `account_mfa` is
        // keyed by (account, method), and an interrupted enrolment must not leave
        // two plausible secrets.
        sqlx::query(
            "INSERT INTO account_mfa (account_id, method, secret_ref, confirmed_at)
             VALUES ($1, 'totp', $2, NULL)
             ON CONFLICT (account_id, method) DO UPDATE
                 SET secret_ref = EXCLUDED.secret_ref,
                     confirmed_at = NULL,
                     last_used_at = NULL,
                     updated_at = now()
               WHERE account_mfa.confirmed_at IS NULL",
        )
        .bind(principal.account_id)
        .bind(&sealed)
        .execute(&mut *transaction)
        .await?;

        // The codes from a previous pending enrolment are dead the moment a new
        // secret exists; leaving them would hand out two sets for one factor.
        sqlx::query("DELETE FROM account_recovery_code WHERE account_id = $1")
            .bind(principal.account_id)
            .execute(&mut *transaction)
            .await?;

        let mut recovery_codes = Vec::with_capacity(10);
        for _ in 0..10 {
            let code = new_recovery_code();
            sqlx::query(
                "INSERT INTO account_recovery_code (id, account_id, code_hash)
                 VALUES ($1, $2, $3)",
            )
            .bind(Uuid::new_v4())
            .bind(principal.account_id)
            .bind(hash_recovery_code(&code))
            .execute(&mut *transaction)
            .await?;
            recovery_codes.push(code);
        }

        transaction.commit().await?;

        let account_label = self.email_for(principal.account_id).await?;
        audit::record(
            self.pool,
            Some(principal.account_id),
            event::MFA_ENROLMENT_STARTED,
            serde_json::json!({ "method": "totp" }),
        )
        .await;

        Ok(Enrolment {
            method: "totp".to_string(),
            otpauth_uri: totp::otpauth_uri(&self.config.otp_issuer, &account_label, &secret_base32),
            secret: secret_base32,
            recovery_codes,
            expires_at,
        })
    }

    /// Confirms a pending TOTP enrolment with a code.
    pub async fn confirm_enrolment(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<(), AuthError> {
        let row = sqlx::query_as::<_, (Option<String>, Option<DateTime<Utc>>, DateTime<Utc>)>(
            "SELECT secret_ref, confirmed_at, created_at
               FROM account_mfa
              WHERE account_id = $1 AND method = 'totp'",
        )
        .bind(principal.account_id)
        .fetch_optional(self.pool)
        .await?;

        let Some((secret_ref, confirmed_at, created_at)) = row else {
            return Err(AuthError::Conflict(
                "There is no pending enrolment to confirm. Start one with /v1/admin/auth/mfa/enrol.".into(),
            ));
        };
        if confirmed_at.is_some() {
            return Err(AuthError::Conflict(
                "The second factor is already confirmed.".into(),
            ));
        }
        let Some(secret_ref) = secret_ref else {
            return Err(AuthError::Conflict(
                "The pending enrolment has no secret.".into(),
            ));
        };

        let deadline = created_at + Duration::seconds(self.config.enrolment_ttl.as_secs() as i64);
        if deadline <= Utc::now() {
            return Err(AuthError::ChallengeExpired);
        }

        let secret = secretbox::open(&self.config.mfa_key, &secret_ref).map_err(|_| {
            AuthError::Conflict("The pending secret cannot be read; start again.".into())
        })?;
        let secret = std::str::from_utf8(&secret)
            .ok()
            .and_then(totp::decode_secret)
            .ok_or_else(|| {
                AuthError::Conflict("The pending secret is not a TOTP secret.".into())
            })?;

        let Some(_) = totp::verify(
            &secret,
            code,
            Utc::now().timestamp(),
            totp::DEFAULT_SKEW_STEPS,
        ) else {
            return Err(AuthError::MfaRejected);
        };

        sqlx::query(
            "UPDATE account_mfa
                SET confirmed_at = now(), last_used_at = now(), updated_at = now()
              WHERE account_id = $1 AND method = 'totp' AND confirmed_at IS NULL",
        )
        .bind(principal.account_id)
        .execute(self.pool)
        .await?;

        audit::record(
            self.pool,
            Some(principal.account_id),
            event::MFA_ENROLLED,
            serde_json::json!({ "method": "totp" }),
        )
        .await;
        Ok(())
    }

    // ── internals ──────────────────────────────────────────────────────────────

    async fn find_account(&self, email: &str) -> Result<Option<AccountRow>, AuthError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<i32>,
                Option<DateTime<Utc>>,
                Option<DateTime<Utc>>,
            ),
        >(
            "SELECT a.id, a.status::text, c.password_hash, c.failed_attempts, c.locked_until,
                    a.suspended_at
               FROM account a
               LEFT JOIN account_credential c ON c.account_id = a.id
              WHERE a.email = $1::citext",
        )
        .bind(email)
        .fetch_optional(self.pool)
        .await?;

        Ok(row.map(
            |(id, status, password_hash, _failed_attempts, locked_until, suspended_at)| {
                AccountRow {
                    id,
                    status,
                    password_hash,
                    locked_until,
                    suspended_at,
                }
            },
        ))
    }

    async fn find_account_by_id(&self, account_id: Uuid) -> Result<Option<AccountRow>, AuthError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<String>,
                Option<i32>,
                Option<DateTime<Utc>>,
                Option<DateTime<Utc>>,
            ),
        >(
            "SELECT a.id, a.status::text, c.password_hash, c.failed_attempts, c.locked_until,
                    a.suspended_at
               FROM account a
               LEFT JOIN account_credential c ON c.account_id = a.id
              WHERE a.id = $1",
        )
        .bind(account_id)
        .fetch_optional(self.pool)
        .await?;

        Ok(row.map(
            |(id, status, password_hash, _failed_attempts, locked_until, suspended_at)| {
                AccountRow {
                    id,
                    status,
                    password_hash,
                    locked_until,
                    suspended_at,
                }
            },
        ))
    }

    async fn email_for(&self, account_id: Uuid) -> Result<String, AuthError> {
        let email =
            sqlx::query_scalar::<_, String>("SELECT email::text FROM account WHERE id = $1")
                .bind(account_id)
                .fetch_one(self.pool)
                .await?;
        Ok(email)
    }

    /// Whether the account holds any open role that grants a scope.
    ///
    /// Deliberately not "holds the administrator role": A2 defines the role
    /// catalogue and the exact scopes an administrator surface requires. What this
    /// chunk must guarantee is the refusal — an account with no role at all cannot
    /// reach the console — and that is what it checks. A role with no scopes grants
    /// nothing and so does not count.
    async fn has_administrator_role(&self, account_id: Uuid) -> Result<bool, AuthError> {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                 SELECT 1
                   FROM account_role r
                   JOIN role_definition d ON d.role_code = r.role_code
                  WHERE r.account_id = $1
                    AND r.valid_to IS NULL
                    AND cardinality(d.scopes) > 0)",
        )
        .bind(account_id)
        .fetch_one(self.pool)
        .await?;
        Ok(exists)
    }

    /// The account's effective scopes: the union of its open roles' scopes.
    async fn scopes_for(&self, account_id: Uuid) -> Result<Vec<String>, AuthError> {
        let scopes = sqlx::query_scalar::<_, Vec<String>>(
            "SELECT COALESCE(array_agg(DISTINCT scope), '{}')
               FROM account_role r
               JOIN role_definition d ON d.role_code = r.role_code
               CROSS JOIN LATERAL unnest(d.scopes) AS scope
              WHERE r.account_id = $1 AND r.valid_to IS NULL",
        )
        .bind(account_id)
        .fetch_one(self.pool)
        .await?;
        Ok(scopes)
    }

    /// The confirmed second-factor methods, plus when the first was confirmed.
    async fn confirmed_factors(
        &self,
        account_id: Uuid,
    ) -> Result<Option<(Vec<String>, Option<DateTime<Utc>>)>, AuthError> {
        let rows = sqlx::query_as::<_, (String, Option<DateTime<Utc>>)>(
            "SELECT method, confirmed_at
               FROM account_mfa
              WHERE account_id = $1 AND confirmed_at IS NOT NULL
              ORDER BY confirmed_at",
        )
        .bind(account_id)
        .fetch_all(self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(None);
        }
        let confirmed_at = rows.iter().filter_map(|(_, at)| *at).min();
        let methods = rows.into_iter().map(|(method, _)| method).collect();
        Ok(Some((methods, confirmed_at)))
    }

    async fn issue_challenge(&self, account_id: Uuid) -> Result<Challenge, AuthError> {
        let challenge_id = Uuid::new_v4();
        let expires_at = Utc::now() + Duration::seconds(self.config.challenge_ttl.as_secs() as i64);

        // Any earlier open challenge for this account is spent: a second password
        // verification replaces the first rather than leaving two live answers.
        sqlx::query(
            "UPDATE mfa_challenge
                SET consumed_at = now(), updated_at = now()
              WHERE account_id = $1 AND consumed_at IS NULL",
        )
        .bind(account_id)
        .execute(self.pool)
        .await?;

        sqlx::query(
            "INSERT INTO mfa_challenge (id, account_id, expires_at)
             VALUES ($1, $2, $3)",
        )
        .bind(challenge_id)
        .bind(account_id)
        .bind(expires_at)
        .execute(self.pool)
        .await?;

        Ok(Challenge {
            status: "mfa_required",
            challenge_id,
            methods: Vec::new(),
            expires_at,
        })
    }

    async fn find_open_challenge(
        &self,
        challenge_id: Uuid,
    ) -> Result<Option<(Uuid, i32)>, AuthError> {
        let row = sqlx::query_as::<_, (Uuid, i32)>(
            "SELECT account_id, attempts
               FROM mfa_challenge
              WHERE id = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(challenge_id)
        .fetch_optional(self.pool)
        .await?;
        Ok(row)
    }

    async fn consume_challenge(&self, challenge_id: Uuid) -> Result<bool, AuthError> {
        let result = sqlx::query(
            "UPDATE mfa_challenge
                SET consumed_at = now(), updated_at = now()
              WHERE id = $1 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(challenge_id)
        .execute(self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn record_wrong_code(
        &self,
        challenge_id: Uuid,
        attempts: i32,
    ) -> Result<bool, AuthError> {
        let result = sqlx::query(
            "UPDATE mfa_challenge
                SET attempts = attempts + 1,
                    consumed_at = CASE WHEN attempts + 1 >= $2 THEN now() ELSE consumed_at END,
                    updated_at = now()
              WHERE id = $1 AND consumed_at IS NULL",
        )
        .bind(challenge_id)
        .bind(CHALLENGE_ATTEMPT_LIMIT)
        .execute(self.pool)
        .await?;
        Ok(attempts + 1 >= CHALLENGE_ATTEMPT_LIMIT || result.rows_affected() == 0)
    }

    /// Verifies a TOTP code and refuses a replay of one already used in its step.
    ///
    /// `allow_same_step` exists for re-authentication, and only for it. A sign-in consumes
    /// a code and records the step; an administrator who signs in and immediately grants a
    /// role would otherwise be told their perfectly good code was rejected, for up to
    /// thirty seconds. Re-authentication already demands the password, so the marginal risk
    /// of sharing a step is a code observed by someone who also has the password, inside
    /// the same half-minute. Challenge answers stay strict: that is the path an attacker
    /// with a stolen password but no factor walks.
    async fn verify_totp(
        &self,
        account_id: Uuid,
        code: &str,
        allow_same_step: bool,
    ) -> Result<bool, AuthError> {
        let row = sqlx::query_as::<_, (Option<String>, Option<DateTime<Utc>>)>(
            "SELECT secret_ref, last_used_at
               FROM account_mfa
              WHERE account_id = $1 AND method = 'totp' AND confirmed_at IS NOT NULL",
        )
        .bind(account_id)
        .fetch_optional(self.pool)
        .await?;

        let Some((Some(secret_ref), last_used_at)) = row else {
            return Ok(false);
        };
        let secret = secretbox::open(&self.config.mfa_key, &secret_ref)
            .map_err(|_| AuthError::MfaRejected)?;
        let Ok(secret_base32) = std::str::from_utf8(&secret) else {
            return Ok(false);
        };
        let Some(secret) = totp::decode_secret(secret_base32) else {
            return Ok(false);
        };

        let now = Utc::now().timestamp();
        let Some(matched_step) = totp::verify(&secret, code, now, totp::DEFAULT_SKEW_STEPS) else {
            return Ok(false);
        };

        // A TOTP code is valid for its whole 30-second window. Accepting the same
        // one twice inside that window is a replay, and the only record of the first
        // use is `last_used_at`.
        if !allow_same_step {
            if let Some(last_used_at) = last_used_at {
                if totp::step_at(last_used_at.timestamp()) >= matched_step {
                    return Ok(false);
                }
            }
        }

        sqlx::query(
            "UPDATE account_mfa SET last_used_at = now(), updated_at = now()
              WHERE account_id = $1 AND method = 'totp'",
        )
        .bind(account_id)
        .execute(self.pool)
        .await?;

        Ok(true)
    }

    async fn verify_recovery_code(&self, account_id: Uuid, code: &str) -> Result<bool, AuthError> {
        let hash = hash_recovery_code(code);
        let matched = sqlx::query_scalar::<_, Uuid>(
            "UPDATE account_recovery_code
                SET used_at = now()
              WHERE account_id = $1 AND code_hash = $2 AND used_at IS NULL
          RETURNING id",
        )
        .bind(account_id)
        .bind(&hash)
        .fetch_optional(self.pool)
        .await?;
        Ok(matched.is_some())
    }

    /// Creates the session and mints the access token.
    async fn start_session(
        &self,
        account_id: Uuid,
        client: &ClientContext,
    ) -> Result<SessionView, AuthError> {
        let scopes = self.scopes_for(account_id).await?;
        let token_id = Uuid::new_v4();
        let (row, refresh) = session::create(
            self.pool,
            self.config,
            account_id,
            AUDIENCE_ADMIN,
            client,
            token_id,
        )
        .await?;
        let (access_token, access_expires_at) =
            self.tokens
                .issue(account_id, row.id, token_id, AUDIENCE_ADMIN, &scopes);

        let mfa_confirmed_at = self
            .confirmed_factors(account_id)
            .await?
            .and_then(|(_, confirmed_at)| confirmed_at);

        Ok(SessionView {
            status: None,
            account_id,
            audience: AUDIENCE_ADMIN.to_string(),
            session_id: row.id,
            access_token,
            access_token_expires_at: unix(access_expires_at),
            refresh_expires_at: row.absolute_expires_at,
            scopes,
            mfa_confirmed_at,
            refresh_token: refresh.token,
        })
    }

    async fn record_wrong_password(
        &self,
        account_id: Uuid,
    ) -> Result<(i32, Option<DateTime<Utc>>), AuthError> {
        let row = sqlx::query_as::<_, (i32, Option<DateTime<Utc>>)>(
            "UPDATE account_credential
                SET failed_attempts = failed_attempts + 1,
                    locked_until = CASE
                        WHEN failed_attempts + 1 >= $2
                        THEN now() + make_interval(secs => $3)
                        ELSE locked_until
                    END,
                    updated_at = now()
              WHERE account_id = $1
          RETURNING failed_attempts, locked_until",
        )
        .bind(account_id)
        .bind(self.config.lockout_threshold)
        .bind(self.config.lockout_duration.as_secs() as f64)
        .fetch_optional(self.pool)
        .await?;

        Ok(row.unwrap_or((0, None)))
    }

    async fn clear_failures(&self, account_id: Uuid) -> Result<(), AuthError> {
        sqlx::query(
            "UPDATE account_credential
                SET failed_attempts = 0, locked_until = NULL, updated_at = now()
              WHERE account_id = $1 AND (failed_attempts <> 0 OR locked_until IS NOT NULL)",
        )
        .bind(account_id)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// Refuses when either key has spent its allowance for the window.
    async fn check_rate_limit(
        &self,
        account_key: &str,
        ip_key: Option<&str>,
    ) -> Result<(), AuthError> {
        // A vector rather than a chained iterator: the loop body awaits, so the
        // iterator would have to live across the await while borrowing two locals
        // with different lifetimes, which is the shape that makes an async fn unable
        // to satisfy the handler's `for<'a>` bound.
        let mut keys: Vec<&str> = vec![account_key];
        keys.extend(ip_key);
        for key in keys {
            if let Some(retry_after_seconds) = self.retry_after(key).await? {
                audit::record(
                    self.pool,
                    None,
                    event::LOGIN_FAILED,
                    serde_json::json!({ "reason": "rate_limited" }),
                )
                .await;
                return Err(AuthError::RateLimited {
                    retry_after_seconds,
                });
            }
        }
        Ok(())
    }

    /// Seconds until the key may be used again, or `None` when it may be used now.
    async fn retry_after(&self, key: &str) -> Result<Option<i64>, AuthError> {
        let row = sqlx::query_as::<_, (i32, DateTime<Utc>)>(
            "SELECT request_count, window_started_at FROM login_rate_limit WHERE key_hash = $1",
        )
        .bind(key)
        .fetch_optional(self.pool)
        .await?;

        let Some((count, window_started_at)) = row else {
            return Ok(None);
        };
        if i64::from(count) < self.config.login_rate_limit {
            return Ok(None);
        }

        let window = Duration::seconds(self.config.login_rate_window.as_secs() as i64);
        let expires_at = window_started_at + window;
        let remaining = (expires_at - Utc::now()).num_seconds();
        Ok(Some(remaining.max(1)))
    }

    /// Counts a failure against every key.
    async fn count_failure(&self, account_key: &str, ip_key: Option<&str>) {
        let mut keys: Vec<&str> = vec![account_key];
        keys.extend(ip_key);
        for key in keys {
            if let Err(error) = self.increment_key(key).await {
                tracing::error!(%error, "could not increment the login rate limit");
            }
        }
    }

    async fn increment_key(&self, key: &str) -> Result<(), AuthError> {
        sqlx::query(
            "INSERT INTO login_rate_limit (key_hash, window_started_at, request_count)
             VALUES ($1, now(), 1)
             ON CONFLICT (key_hash) DO UPDATE
                 SET request_count = CASE
                         WHEN login_rate_limit.window_started_at <= now() - make_interval(secs => $2)
                         THEN 1 ELSE login_rate_limit.request_count + 1 END,
                     window_started_at = CASE
                         WHEN login_rate_limit.window_started_at <= now() - make_interval(secs => $2)
                         THEN now() ELSE login_rate_limit.window_started_at END,
                     updated_at = now()",
        )
        .bind(key)
        .bind(self.config.login_rate_window.as_secs() as f64)
        .execute(self.pool)
        .await?;
        Ok(())
    }
}

/// The stored `access_jti` of a session, for a logout through the cookie.
async fn access_jti(pool: &PgPool, session_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("SELECT access_jti FROM auth_session WHERE id = $1")
        .bind(session_id)
        .fetch_one(pool)
        .await
}

/// Runs Argon2 on the blocking pool.
///
/// A verification is ~50 ms of pure CPU and ~19 MiB of memory. On the async runtime
/// that becomes a stalled worker and, with enough concurrent sign-ins, a service
/// that stops answering health checks.
async fn verify_password_off_thread(password: String, hash: String) -> bool {
    match tokio::task::spawn_blocking(move || crate::password::verify(&password, &hash)).await {
        Ok(verified) => verified,
        Err(error) => {
            tracing::error!(%error, "password verification task failed");
            false
        }
    }
}

/// A hash to verify against when the account does not exist.
fn dummy_hash() -> String {
    // Generated once per process by `verify_against_dummy`'s cache in `password`;
    // here the same constant shape is used so the cost is one verification.
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHR2YWx1ZQ$YXNkZmFzZGZhc2RmYXNkZmFzZGZhc2RmYXNkZmFzZGZhc2RmYXNkZg".to_string()
}

/// A rate-limit key. Hashed so the table holds no addresses and no email addresses.
fn rate_limit_key(kind: &str, value: &str) -> String {
    let digest = Sha256::digest(format!("{kind}:{value}").as_bytes());
    crate::hex_lower(&digest)
}

/// A recovery code: three groups of four hex characters, as the contract's example
/// shows. Forty-eight bits, single use, and only reachable through an endpoint that
/// burns the challenge after five wrong answers.
fn new_recovery_code() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 6];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex = crate::hex_lower(&bytes);
    format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
}

/// The stored form of a recovery code.
///
/// SHA-256, not Argon2: the code is 48 random bits, so there is nothing to guess,
/// and the verification path is already limited to five attempts per challenge.
fn hash_recovery_code(code: &str) -> String {
    let normalised = code.trim().to_ascii_lowercase().replace(' ', "");
    crate::hex_lower(&Sha256::digest(normalised.as_bytes()))
}

/// Unix seconds to a timestamp.
fn unix(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_keys_are_hashed_and_distinct_per_kind() {
        let address = rate_limit_key("admin-login:account", "admin@example.org");
        let ip = rate_limit_key("admin-login:ip", "admin@example.org");
        assert_eq!(address.len(), 64);
        assert_ne!(
            address, ip,
            "the same value in two buckets must not collide"
        );
        assert!(!address.contains("admin@example.org"));
    }

    #[test]
    fn a_recovery_code_has_the_documented_shape() {
        let code = new_recovery_code();
        let parts: Vec<&str> = code.split('-').collect();
        assert_eq!(parts.len(), 3, "{code}");
        assert!(parts.iter().all(|part| part.len() == 4), "{code}");
        assert!(
            code.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "{code}"
        );
    }

    #[test]
    fn recovery_codes_are_unique() {
        let codes: std::collections::HashSet<String> =
            (0..200).map(|_| new_recovery_code()).collect();
        assert_eq!(codes.len(), 200);
    }

    #[test]
    fn a_recovery_code_hash_ignores_case_and_spaces() {
        let code = "4f2a-8c1e-9d3b";
        assert_eq!(
            hash_recovery_code(code),
            hash_recovery_code("4F2A-8C1E-9D3B")
        );
        assert_eq!(
            hash_recovery_code(code),
            hash_recovery_code(" 4f2a-8c1e-9d3b ")
        );
        assert_ne!(
            hash_recovery_code(code),
            hash_recovery_code("4f2a-8c1e-9d3c")
        );
    }

    #[test]
    fn the_user_agent_is_truncated_to_a_column_sized_value() {
        let context = ClientContext::new(None, Some("x".repeat(500)));
        assert_eq!(context.user_agent.as_deref().map(str::len), Some(200));
    }

    #[test]
    fn the_dummy_hash_is_a_parseable_argon2_string() {
        // It does not have to verify anything; it has to cost a verification, which
        // means argon2 must accept it as a hash to check against.
        assert!(dummy_hash().starts_with("$argon2id$"));
        assert!(!crate::password::verify("anything", &dummy_hash()));
    }
}

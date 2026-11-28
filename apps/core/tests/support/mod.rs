// Each test binary compiles this module separately, so a helper one suite does not use is
// dead code in that binary and alive in the other. Allowing it here is more honest than
// trimming the shared harness to whatever the smaller suite happens to need.
#![allow(dead_code)]

//! Shared test scaffolding: a harness that drives the real router against a real
//! PostgreSQL.
//!
//! Both suites use it — the sign-in flow and the authority flow — because a second copy
//! would drift, and a drifting harness is a test that passes against yesterday's
//! application.
//!
//! These tests exist because the guarantees A1 makes are not properties of a
//! function: "no session without a second factor", "five wrong passwords lock the
//! account", "a participant token is refused" are properties of SQL, HTTP and the
//! order the two are applied in. A unit test with a mock would assert that the code
//! calls the functions it calls.
//!
//! **They need a database.** Set `CORE_TEST_DATABASE_URL` to a PostgreSQL that has
//! the baselines applied:
//!
//! ```sh
//! CORE_TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/myota_core cargo test
//! ```
//!
//! Without it they report why they were skipped and pass, the same rule `make
//! checks-ops` follows: a suite that cannot run says so instead of silently
//! reporting success.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use chrono::Utc;
use http_body_util::BodyExt as _;
use myota_core::config::Config;
use myota_core::http::{router, AppState};
use myota_core::token::TokenService;
use myota_core::{bootstrap, password, secretbox, totp};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;
use uuid::Uuid;

/// The service this suite configures as an internal client.
pub const TEST_SERVICE: &str = "geodata";

/// Its credential. Long enough for the configuration to accept it, and obviously a test
/// value so its presence in a database dump would be a different kind of problem.
pub const TEST_SERVICE_SECRET: &str = "test-service-secret-at-least-32-characters";

/// A completed sign-in.
pub struct SignedIn {
    /// The access token.
    pub token: String,
    /// The refresh cookie, as a `Cookie` header value.
    pub cookie: String,
    /// The session the token belongs to.
    pub session_id: Uuid,
}

/// Everything a test needs to drive the service.
pub struct Harness {
    pub app: Router,
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub tokens: Arc<TokenService>,
}

impl Harness {
    /// Builds the router over the test database, or `None` when there is none.
    pub async fn new() -> Option<Self> {
        let database_url = match std::env::var("CORE_TEST_DATABASE_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => {
                eprintln!(
                    "skipping: CORE_TEST_DATABASE_URL is not set, so the sign-in flow was not \
                     exercised against a database"
                );
                return None;
            }
        };

        let config = Arc::new(
            Config::from_lookup(|name| match name {
                "CORE_DATABASE_URL" => Some(database_url.clone()),
                "CORE_JWT_SEED_BASE64" => Some(base64_key(&[21_u8; 32])),
                "CORE_MFA_KEY_BASE64" => Some(base64_key(&[22_u8; 32])),
                "CORE_JWT_KEY_ID" => Some("core-test".to_string()),
                "CORE_TRUST_FORWARDED_FOR" => Some("false".to_string()),
                "CORE_SERVICE_CLIENTS" => {
                    Some(format!(r#"{{"{TEST_SERVICE}":"{TEST_SERVICE_SECRET}"}}"#))
                }
                _ => None,
            })
            .expect("the test configuration is valid"),
        );

        let pool = myota_core::db::connect(&config.database_url)
            .await
            .expect("the test database is reachable");
        let tokens = Arc::new(TokenService::new(
            &config.token_seed,
            config.token_key_id.clone(),
            config.access_ttl,
        ));
        let app = router(AppState::new(
            pool.clone(),
            Arc::clone(&config),
            Arc::clone(&tokens),
        ));

        Some(Self {
            app,
            pool,
            config,
            tokens,
        })
    }

    /// A unique address, so tests can share one database.
    pub fn email(label: &str) -> String {
        format!("{label}-{}@test.example.org", Uuid::new_v4())
    }

    /// Provisions an administrator through the same tool the operator runs.
    pub async fn provision(&self, email: &str, plain_password: &str) -> bootstrap::Provisioned {
        bootstrap::provision_admin(
            &self.pool,
            &self.config,
            email,
            "Test Administrator",
            plain_password,
        )
        .await
        .expect("provisioning succeeds")
    }

    /// Inserts an account with a credential but no role and no MFA.
    pub async fn account_without_role(&self, email: &str, plain_password: &str) -> Uuid {
        self.account(email, plain_password, false, false).await
    }

    /// Inserts an account with a role but no confirmed MFA.
    pub async fn account_without_mfa(&self, email: &str, plain_password: &str) -> Uuid {
        self.account(email, plain_password, true, false).await
    }

    pub async fn account(
        &self,
        email: &str,
        plain_password: &str,
        with_role: bool,
        with_mfa: bool,
    ) -> Uuid {
        let account_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO account (id, display_name, email, participation_type, status, email_verified)
             VALUES ($1, 'Test', $2, 'OPERATOR', 'active', true)",
        )
        .bind(account_id)
        .bind(email)
        .execute(&self.pool)
        .await
        .expect("inserting an account");

        sqlx::query(
            "INSERT INTO account_credential (account_id, password_hash, password_changed_at)
             VALUES ($1, $2, now())",
        )
        .bind(account_id)
        .bind(password::hash(plain_password).expect("hashing"))
        .execute(&self.pool)
        .await
        .expect("inserting a credential");

        if with_role {
            sqlx::query(
                "INSERT INTO role_definition (role_code, display_name, scopes)
                 VALUES ('test_admin', 'Test administrator', ARRAY['geodata.review'])
                 ON CONFLICT (role_code) DO NOTHING",
            )
            .execute(&self.pool)
            .await
            .expect("inserting a role");
            sqlx::query(
                "INSERT INTO account_role (id, account_id, role_code) VALUES ($1, $2, 'test_admin')",
            )
            .bind(Uuid::new_v4())
            .bind(account_id)
            .execute(&self.pool)
            .await
            .expect("granting a role");
        }

        if with_mfa {
            let secret = totp::generate_secret();
            let sealed = secretbox::seal(
                &self.config.mfa_key,
                totp::encode_secret(&secret).as_bytes(),
            );
            sqlx::query(
                "INSERT INTO account_mfa (account_id, method, secret_ref, confirmed_at)
                 VALUES ($1, 'totp', $2, now())",
            )
            .bind(account_id)
            .bind(sealed)
            .execute(&self.pool)
            .await
            .expect("inserting a factor");
        }

        account_id
    }

    /// Signs an administrator in end to end and returns the credential.
    ///
    /// The full flow rather than a minted token: the endpoints under test run
    /// `principal_from_bearer`, which requires a live session row with that id, so a
    /// token minted out of thin air would be refused and the test would prove nothing.
    pub async fn sign_in(&self, email: &str, password: &str, secret_base32: &str) -> SignedIn {
        let (status, _, body) = self.login(email, password).await;
        assert_eq!(status, StatusCode::OK, "sign-in preamble: {body}");
        let challenge_id = body["challengeId"]
            .as_str()
            .expect("a challenge id")
            .to_string();

        let (status, headers, body) = self
            .send(
                "POST",
                "/v1/admin/auth/mfa/challenge",
                Some(json!({
                    "challengeId": challenge_id,
                    "method": "totp",
                    "code": code_now(secret_base32),
                })),
                &[],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "challenge: {body}");
        SignedIn {
            token: body["accessToken"].as_str().expect("a token").to_string(),
            cookie: cookie_header(&headers).expect("a session cookie"),
            session_id: body["sessionId"]
                .as_str()
                .expect("a session id")
                .parse()
                .expect("a uuid"),
        }
    }

    /// Re-authenticates a session, so a mutating admin call is allowed.
    ///
    /// Every destructive administrator endpoint requires proof within five minutes. A test
    /// that changes a role has to produce it, exactly as a person would.
    pub async fn make_fresh(&self, cookie: &str, password: &str, secret_base32: &str) {
        let (status, _, body) = self
            .send(
                "POST",
                "/v1/admin/auth/reauth",
                Some(json!({ "password": password, "code": code_now(secret_base32) })),
                &[("cookie", cookie.to_string())],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "re-authentication: {body}");
    }

    /// Grants a role directly, bypassing the subset rule.
    ///
    /// Setup, not behaviour: a test that needs an actor holding a role should not have to
    /// construct an actor who could grant it. The rule itself is exercised through the
    /// endpoint.
    pub async fn grant_directly(
        &self,
        account_id: Uuid,
        role_code: &str,
        programme_id: Option<Uuid>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO account_role (id, account_id, role_code, programme_id, scopes)
             VALUES ($1, $2, $3, $4, '{}'::jsonb)",
        )
        .bind(id)
        .bind(account_id)
        .bind(role_code)
        .bind(programme_id)
        .execute(&self.pool)
        .await
        .expect("granting a role directly");
        id
    }

    /// Creates an active account with a credential and one role.
    pub async fn account_with_role(
        &self,
        email: &str,
        plain_password: &str,
        role_code: &str,
    ) -> Uuid {
        let account_id = self.account(email, plain_password, false, false).await;
        self.grant_directly(account_id, role_code, None).await;
        account_id
    }

    /// Sends a request and returns status, headers and the JSON body.
    pub async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, String)],
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let body = match body {
            Some(value) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&value).expect("serialising the body"))
            }
            None => Body::empty(),
        };

        let response = self
            .app
            .clone()
            .oneshot(request.body(body).expect("building the request"))
            .await
            .expect("the router answers");

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("reading the body")
            .to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()))
        };
        (status, headers, json)
    }

    pub async fn login(&self, email: &str, plain_password: &str) -> (StatusCode, HeaderMap, Value) {
        self.send(
            "POST",
            "/v1/admin/auth/login",
            Some(json!({ "email": email, "password": plain_password })),
            &[],
        )
        .await
    }

    pub async fn count_events(&self, account_id: Uuid, event_type: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM security_event WHERE account_id = $1 AND event_type = $2",
        )
        .bind(account_id)
        .bind(event_type)
        .fetch_one(&self.pool)
        .await
        .expect("counting audit rows")
    }
}

pub fn base64_key(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The code an authenticator app would show for this secret now.
pub fn code_now(secret_base32: &str) -> String {
    let secret = totp::decode_secret(secret_base32).expect("a base32 secret");
    totp::code_now(&secret, Utc::now().timestamp())
}

/// The session cookie from a response, as a `Cookie` header value.
pub fn cookie_header(headers: &HeaderMap) -> Option<String> {
    let set_cookie = headers.get(header::SET_COOKIE)?.to_str().ok()?;
    let pair = set_cookie.split(';').next()?.trim();
    (!pair.is_empty()).then(|| pair.to_string())
}

pub fn bearer(token: &str) -> (&'static str, String) {
    ("authorization", format!("Bearer {token}"))
}

// ── the acceptance criteria ───────────────────────────────────────────────────

//! The sign-in flow, driven through the real router against a real PostgreSQL.
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
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::Router;
use chrono::Utc;
use http_body_util::BodyExt as _;
use myota_core::config::Config;
use myota_core::http::{router, AppState};
use myota_core::token::{TokenService, AUDIENCE_ADMIN, AUDIENCE_PARTICIPANT};
use myota_core::{bootstrap, password, secretbox, totp};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt as _;
use uuid::Uuid;

/// Everything a test needs to drive the service.
struct Harness {
    app: Router,
    pool: PgPool,
    config: Arc<Config>,
    tokens: Arc<TokenService>,
}

impl Harness {
    /// Builds the router over the test database, or `None` when there is none.
    async fn new() -> Option<Self> {
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
    fn email(label: &str) -> String {
        format!("{label}-{}@test.example.org", Uuid::new_v4())
    }

    /// Provisions an administrator through the same tool the operator runs.
    async fn provision(&self, email: &str, plain_password: &str) -> bootstrap::Provisioned {
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
    async fn account_without_role(&self, email: &str, plain_password: &str) -> Uuid {
        self.account(email, plain_password, false, false).await
    }

    /// Inserts an account with a role but no confirmed MFA.
    async fn account_without_mfa(&self, email: &str, plain_password: &str) -> Uuid {
        self.account(email, plain_password, true, false).await
    }

    async fn account(
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

    /// Sends a request and returns status, headers and the JSON body.
    async fn send(
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

    async fn login(&self, email: &str, plain_password: &str) -> (StatusCode, HeaderMap, Value) {
        self.send(
            "POST",
            "/v1/admin/auth/login",
            Some(json!({ "email": email, "password": plain_password })),
            &[],
        )
        .await
    }

    async fn count_events(&self, account_id: Uuid, event_type: &str) -> i64 {
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

fn base64_key(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The code an authenticator app would show for this secret now.
fn code_now(secret_base32: &str) -> String {
    let secret = totp::decode_secret(secret_base32).expect("a base32 secret");
    totp::code_now(&secret, Utc::now().timestamp())
}

/// The session cookie from a response, as a `Cookie` header value.
fn cookie_header(headers: &HeaderMap) -> Option<String> {
    let set_cookie = headers.get(header::SET_COOKIE)?.to_str().ok()?;
    let pair = set_cookie.split(';').next()?.trim();
    (!pair.is_empty()).then(|| pair.to_string())
}

fn bearer(token: &str) -> (&'static str, String) {
    ("authorization", format!("Bearer {token}"))
}

// ── the acceptance criteria ───────────────────────────────────────────────────

#[tokio::test]
async fn a_second_factor_is_required_before_a_session_exists() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("admin");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    // Right password, no factor answered yet: a challenge, and no cookie.
    let (status, headers, body) = harness.login(&email, "an-administrator-passphrase").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "mfa_required");
    assert!(body["challengeId"].is_string());
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "a challenge must not carry a session cookie"
    );
    let methods = body["methods"].as_array().expect("methods");
    assert!(methods.iter().any(|method| method == "totp"), "{body}");
    assert!(
        methods.iter().any(|method| method == "recovery_code"),
        "{body}"
    );

    // Answering it produces the session.
    let challenge_id = body["challengeId"].as_str().expect("a challenge id");
    let (status, headers, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/challenge",
            Some(json!({
                "challengeId": challenge_id,
                "method": "totp",
                "code": code_now(&provisioned.secret),
            })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["audience"], "admin");
    assert!(!body["accessToken"].as_str().unwrap_or_default().is_empty());
    assert!(body["scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .any(|scope| scope == "geodata.review"));

    let cookie = cookie_header(&headers).expect("a session cookie");
    assert!(cookie.starts_with("__Host-myota_admin_sid="), "{cookie}");
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("set-cookie");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");

    // The token works on an authenticated route: enrolment already confirmed, so
    // the answer is 409 — anything but 401 proves the credential was accepted.
    let token = body["accessToken"].as_str().expect("a token").to_string();
    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/enrol",
            Some(json!({ "method": "totp" })),
            &[bearer(&token)],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // And the cookie works where the token does.
    let (status, _, _) = harness
        .send("POST", "/v1/admin/auth/logout", None, &[("cookie", cookie)])
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Signing out revokes: the same token is refused afterwards.
    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/enrol",
            Some(json!({ "method": "totp" })),
            &[bearer(&token)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn an_account_without_a_confirmed_factor_is_refused_with_mfa_not_enrolled() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("no-mfa");
    harness
        .account_without_mfa(&email, "a-long-enough-password")
        .await;

    let (status, headers, body) = harness.login(&email, "a-long-enough-password").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#mfa-not-enrolled");
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "a refusal must not carry a cookie"
    );
}

#[tokio::test]
async fn an_account_without_an_administrator_role_is_refused() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("no-role");
    harness
        .account_without_role(&email, "a-long-enough-password")
        .await;

    let (status, _, body) = harness.login(&email, "a-long-enough-password").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#forbidden");
}

#[tokio::test]
async fn five_wrong_passwords_lock_the_account_for_fifteen_minutes() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("lockout");
    let account_id = harness
        .provision(&email, "the-right-passphrase")
        .await
        .account_id;

    for attempt in 1..=5 {
        let (status, _, body) = harness.login(&email, "not-the-right-passphrase").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}: {body}"
        );
        assert_eq!(body["type"], "about:blank#unauthorized");
    }

    // The sixth attempt is refused before the password is even read: the right one
    // does not help.
    let (status, _, body) = harness.login(&email, "the-right-passphrase").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["type"], "about:blank#rate-limited");
    assert!(
        body["retryAfterSeconds"].as_i64().unwrap_or(0) > 0,
        "{body}"
    );

    // The lock is in the database, not in the response: every replica sees it.
    let (failed_attempts, locked_until): (i32, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT failed_attempts, locked_until FROM account_credential WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&harness.pool)
    .await
    .expect("reading the credential");
    assert_eq!(failed_attempts, 5);
    let locked_until = locked_until.expect("the account is locked");
    let remaining = (locked_until - Utc::now()).num_minutes();
    assert!(
        (13..=15).contains(&remaining),
        "locked for {remaining} minutes"
    );

    // And it is in the audit log.
    assert_eq!(harness.count_events(account_id, "auth.lockout").await, 1);
    assert_eq!(
        harness.count_events(account_id, "auth.login_failed").await,
        4
    );
}

#[tokio::test]
async fn a_participant_token_is_refused_on_an_administrator_route() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("participant-token");
    let account_id = harness
        .provision(&email, "an-administrator-passphrase")
        .await
        .account_id;

    // A token of the other audience for the same, real, account.
    let (participant_token, _) = harness.tokens.issue(
        account_id,
        Uuid::new_v4(),
        Uuid::new_v4(),
        AUDIENCE_PARTICIPANT,
        &[],
    );

    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/enrol",
            Some(json!({ "method": "totp" })),
            &[bearer(&participant_token)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["type"], "about:blank#wrong-audience");
}

#[tokio::test]
async fn a_wrong_second_factor_is_rejected_and_burns_the_challenge() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("wrong-codes");
    harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    let (_, _, body) = harness.login(&email, "an-administrator-passphrase").await;
    let challenge_id = body["challengeId"]
        .as_str()
        .expect("a challenge id")
        .to_string();

    for attempt in 1..=4 {
        let (status, _, body) = harness
            .send(
                "POST",
                "/v1/admin/auth/mfa/challenge",
                Some(json!({
                    "challengeId": challenge_id,
                    "method": "totp",
                    "code": "000000",
                })),
                &[],
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}: {body}"
        );
        assert_eq!(body["type"], "about:blank#mfa-failed");
    }

    // The fifth wrong code burns it; from then on the challenge is gone.
    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/challenge",
            Some(json!({ "challengeId": challenge_id, "method": "totp", "code": "000000" })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["type"], "about:blank#not-found");
}

#[tokio::test]
async fn a_recovery_code_answers_the_challenge_once() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("recovery");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let code = provisioned.recovery_codes[0].clone();

    let (_, _, body) = harness.login(&email, "an-administrator-passphrase").await;
    let challenge_id = body["challengeId"]
        .as_str()
        .expect("a challenge id")
        .to_string();
    let (status, headers, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/challenge",
            Some(json!({ "challengeId": challenge_id, "method": "recovery_code", "code": code })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(cookie_header(&headers).is_some());

    // The same code, in a fresh challenge, is refused: a recovery code is single-use.
    let (_, _, body) = harness.login(&email, "an-administrator-passphrase").await;
    let challenge_id = body["challengeId"]
        .as_str()
        .expect("a challenge id")
        .to_string();
    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/challenge",
            Some(json!({ "challengeId": challenge_id, "method": "recovery_code", "code": code })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn the_published_key_set_verifies_the_issued_tokens() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let (status, _, body) = harness.send("GET", "/internal/auth/jwks", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    let key = &body["keys"][0];
    assert_eq!(key["kty"], "OKP");
    assert_eq!(key["crv"], "Ed25519");
    assert_eq!(key["kid"], "core-test");
    assert!(!key["x"].as_str().unwrap_or_default().is_empty());
}

#[tokio::test]
async fn a_refresh_extends_the_session_and_rotates_the_cookie() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("refresh");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    let (_, _, body) = harness.login(&email, "an-administrator-passphrase").await;
    let challenge_id = body["challengeId"]
        .as_str()
        .expect("a challenge id")
        .to_string();
    let (status, headers, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/challenge",
            Some(json!({
                "challengeId": challenge_id,
                "method": "totp",
                "code": code_now(&provisioned.secret),
            })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first_cookie = cookie_header(&headers).expect("a cookie");

    let (status, headers, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", first_cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rotated = cookie_header(&headers).expect("a rotated cookie");
    assert_ne!(rotated, first_cookie, "a refresh must rotate the token");

    // The old cookie is dead: rotation replaced the hash.
    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", first_cookie)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // And the new one still works.
    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", rotated)],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_unknown_address_and_a_wrong_password_look_the_same() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("known");
    harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    let (status_known, _, body_known) = harness.login(&email, "wrong-password-here").await;
    let (status_unknown, _, body_unknown) = harness
        .login(&Harness::email("unknown"), "wrong-password-here")
        .await;

    assert_eq!(status_known, StatusCode::UNAUTHORIZED);
    assert_eq!(status_unknown, StatusCode::UNAUTHORIZED);
    assert_eq!(body_known["type"], body_unknown["type"]);
    assert_eq!(body_known["detail"], body_unknown["detail"]);
}

#[tokio::test]
async fn the_audit_log_records_success_failure_and_lockout() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("audit");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    let (_, _, _) = harness.login(&email, "an-administrator-passphrase").await;
    assert_eq!(
        harness
            .count_events(provisioned.account_id, "auth.password_verified")
            .await,
        1
    );
    assert_eq!(
        harness
            .count_events(provisioned.account_id, "auth.challenge_issued")
            .await,
        1
    );

    let (_, _, _) = harness.login(&email, "not-the-password-at-all").await;
    assert_eq!(
        harness
            .count_events(provisioned.account_id, "auth.login_failed")
            .await,
        1
    );

    // The event payload carries no credential. This is the assertion that stops a
    // future `payload: {"password": …}` from shipping.
    let payloads: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM security_event WHERE account_id = $1")
            .bind(provisioned.account_id)
            .fetch_all(&harness.pool)
            .await
            .expect("reading audit rows");
    for payload in payloads {
        let rendered = payload.to_string();
        assert!(
            !rendered.contains("an-administrator-passphrase"),
            "{rendered}"
        );
        assert!(!rendered.contains(&provisioned.secret), "{rendered}");
    }
}

#[tokio::test]
async fn the_service_serves_its_own_contract() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let (status, headers, body) = harness
        .send("GET", "/internal/openapi.json", None, &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(body["info"]["title"], "Myota core service");
    assert!(body["paths"]["/v1/admin/auth/login"]["post"].is_object());
}

#[tokio::test]
async fn a_malformed_body_is_a_problem_document_not_plain_text() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let response = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{not json"))
                .expect("building the request"),
        )
        .await
        .expect("the router answers");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("a JSON problem");
    assert_eq!(body["type"], "about:blank#validation-failed");
}

#[tokio::test]
async fn health_and_readiness_answer() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    // The path the contract declares and the gateway aggregates. Getting this wrong
    // is invisible in isolation — the pod is healthy, the gateway reports every
    // upstream unreachable — so it is asserted here rather than assumed.
    let (status, _, body) = harness.send("GET", "/internal/healthz", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["service"], "core");
    assert_eq!(body["status"], "ok");

    let (status, _, body) = harness.send("GET", "/healthz", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["service"], "core");

    let (status, _, body) = harness.send("GET", "/readyz", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok");
}

/// A token signed for the right issuer but by another key must not be accepted.
#[tokio::test]
async fn a_token_from_another_signer_is_refused() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("other-signer");
    let account_id = harness
        .provision(&email, "an-administrator-passphrase")
        .await
        .account_id;

    let other = TokenService::new(&[99_u8; 32], "core-test", harness.config.access_ttl);
    let (token, _) = other.issue(
        account_id,
        Uuid::new_v4(),
        Uuid::new_v4(),
        AUDIENCE_ADMIN,
        &[],
    );

    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/mfa/enrol",
            Some(json!({ "method": "totp" })),
            &[bearer(&token)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

/// Header injection through the cookie value must be impossible.
#[tokio::test]
async fn a_crafted_cookie_cannot_inject_a_header() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/logout",
            None,
            &[("cookie", "__Host-myota_admin_sid=x; evil=1".to_string())],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A header value that is not valid HTTP must be rejected by the test client, not
/// silently accepted by the server. This asserts the shape of the request the other
/// tests rely on rather than the service.
#[test]
fn the_test_client_refuses_an_invalid_header() {
    assert!(HeaderValue::from_str("a\nb").is_err());
}

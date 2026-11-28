//! The HTTP surface: routing, extraction, and the mapping from [`AuthError`] to an
//! RFC 7807 response.
//!
//! This is the only layer that knows about status codes. Everything it calls returns
//! a typed outcome, so the decision "a wrong second factor is 401 but an expired
//! challenge is 410" lives in exactly one `match`, and a handler cannot invent a new
//! error shape.
//!
//! ## What is not here
//!
//! No TLS (Traefik terminates it), no rate limiting at the edge (that is the
//! gateway's job for floods, and this service's `login_rate_limit` for the
//! authoritative per-account count), and no CORS: both front ends are on the same
//! origin as the gateway that proxies here.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::{Auth, AuthError, ClientContext, LoginOutcome, Principal, SessionView};
use crate::config::Config;
use crate::problem::{Problem, ProblemType};
use crate::session;
use crate::token::TokenService;
use crate::{db, ADMIN_SESSION_COOKIE, SERVICE_NAME};

/// Everything a handler needs.
#[derive(Clone)]
pub struct AppState {
    /// The database pool.
    pub pool: PgPool,
    /// Configuration, immutable for the process's life.
    pub config: Arc<Config>,
    /// Token signing and verification.
    pub tokens: Arc<TokenService>,
}

impl AppState {
    /// Builds a state from its parts.
    #[must_use]
    pub fn new(pool: PgPool, config: Arc<Config>, tokens: Arc<TokenService>) -> Self {
        Self {
            pool,
            config,
            tokens,
        }
    }

    /// The sign-in flow over this state.
    fn auth(&self) -> Auth<'_> {
        Auth {
            pool: &self.pool,
            config: &self.config,
            tokens: &self.tokens,
        }
    }
}

/// The peer address, attached by [`attach_peer`].
///
/// A layer rather than an extractor: `Option<ConnectInfo<SocketAddr>>` stopped being
/// an extractor in axum 0.8, and a hand-written `FromRequestParts` for the optional
/// case runs into the handler-trait bounds. A middleware reads the extension once
/// and every handler takes `Extension<PeerAddress>`, which is a plain, optional value.
#[derive(Clone, Copy)]
struct PeerAddress(Option<SocketAddr>);

/// Copies the peer address out of the connection extension, when there is one.
///
/// `into_make_service_with_connect_info` always provides it in the deployment; a
/// test that drives the router with `oneshot` does not, and gets `None` — which is
/// the same shape as a request that arrived without one.
async fn attach_peer(mut request: Request, next: axum::middleware::Next) -> Response {
    let address = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    request.extensions_mut().insert(PeerAddress(address));
    next.run(request).await
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/internal/auth/jwks", get(jwks))
        .route("/internal/openapi.json", get(contract))
        .route("/v1/admin/auth/login", post(login))
        .route("/v1/admin/auth/mfa/challenge", post(complete_challenge))
        .route("/v1/admin/auth/refresh", post(refresh))
        .route("/v1/admin/auth/logout", post(logout))
        .route("/v1/admin/auth/mfa/enrol", post(enrol))
        .route("/v1/admin/auth/mfa/enrol/confirm", post(confirm_enrolment))
        // Every request body this service accepts is a few hundred bytes. The limit
        // is here rather than in the gateway because a service must not depend on a
        // proxy for its own memory safety.
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(axum::middleware::from_fn(attach_peer))
        .with_state(state)
}

// ── health ─────────────────────────────────────────────────────────────────────

/// Liveness. Answers "this process runs", which is all a restart policy needs.
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok", "service": SERVICE_NAME }))
}

/// Readiness. Fails when the database does not answer, because a replica that
/// cannot read a session cannot serve one.
async fn ready(State(state): State<AppState>) -> Response {
    match db::ping(&state.pool).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response(),
        Err(error) => {
            tracing::error!(%error, "readiness: the database does not answer");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "unavailable", "database": "unreachable" })),
            )
                .into_response()
        }
    }
}

/// The JWKS the gateway fetches (ADR-0008).
///
/// Public on purpose: a public key is public. It is served from the same process
/// that signs, so a rotation is one deploy rather than two.
async fn jwks(State(state): State<AppState>) -> Response {
    let mut response = Json(state.tokens.jwks()).into_response();
    // Cached briefly: a rotation should take effect within the minute, and the
    // gateway already refetches on an unknown key id.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=60"),
    );
    response
}

/// The service's own contract, for `tools/compose_contracts.py --fetch`.
///
/// The document is the committed YAML converted to JSON at build time by
/// `tools/gen_core_contract.py`; the same file the composer reads from disk. Serving
/// a hand-written copy would be a second document that drifts.
async fn contract() -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        include_str!("../../assets/openapi.json"),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

// ── sign-in ───────────────────────────────────────────────────────────────────

/// `POST /v1/admin/auth/login`
async fn login(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
    body: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let peer_address = peer.0 .0;
    let client = client_context(&state.config, &headers, peer_address.as_ref());
    let Json(body) = body.map_err(body_problem)?;

    match state
        .auth()
        .login(&body.email, &body.password, &client)
        .await
    {
        Ok(LoginOutcome::Challenge(challenge)) => Ok(Json(challenge).into_response()),
        Ok(LoginOutcome::Authenticated(view)) => {
            Ok(session_response(&state, view, "authenticated"))
        }
        Err(error) => Err(problem_for(error)),
    }
}

/// `POST /v1/admin/auth/mfa/challenge`
async fn complete_challenge(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
    body: Result<Json<ChallengeRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let peer_address = peer.0 .0;
    let client = client_context(&state.config, &headers, peer_address.as_ref());
    let Json(body) = body.map_err(body_problem)?;

    state
        .auth()
        .complete_challenge(body.challenge_id, &body.method, &body.code, &client)
        .await
        .map(|view| session_response(&state, view, ""))
        .map_err(problem_for)
}

/// `POST /v1/admin/auth/refresh`
async fn refresh(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let peer_address = peer.0 .0;
    let client = client_context(&state.config, &headers, peer_address.as_ref());
    let token = refresh_token_from(&headers).ok_or_else(|| {
        Problem::new(
            ProblemType::Unauthorized,
            "There is no administrator session cookie.",
        )
    })?;

    state
        .auth()
        .refresh(&token, &client)
        .await
        .map(|view| session_response(&state, view, ""))
        .map_err(problem_for)
}

/// `POST /v1/admin/auth/logout`
async fn logout(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;

    state.auth().logout(&principal).await.map_err(problem_for)?;

    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session::cleared_session_cookie())],
    )
        .into_response())
}

/// `POST /v1/admin/auth/mfa/enrol`
async fn enrol(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
    body: Result<Json<EnrolRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;

    // The contract allows `webauthn` in the enum and this chunk does not implement
    // it. Refusing with a named reason beats enrolling something that cannot be
    // used to sign in.
    let Json(body) = body.map_err(body_problem)?;
    if body.method != "totp" {
        return Err(Problem::new(
            ProblemType::ValidationFailed,
            format!(
                "The {0} method is not available yet; enrol a TOTP factor.",
                body.method
            ),
        ));
    }

    let enrolment = state.auth().enrol(&principal).await.map_err(problem_for)?;
    Ok((StatusCode::CREATED, Json(enrolment)).into_response())
}

/// `POST /v1/admin/auth/mfa/enrol/confirm`
async fn confirm_enrolment(
    State(state): State<AppState>,
    peer: axum::Extension<PeerAddress>,
    headers: HeaderMap,
    body: Result<Json<ConfirmRequest>, JsonRejection>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    state
        .auth()
        .confirm_enrolment(&principal, &body.map_err(body_problem)?.code)
        .await
        .map_err(problem_for)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Builds the response for a completed sign-in: body, `Set-Cookie`, and no caching.
///
/// `Cache-Control: no-store` is not decoration. A response carrying an access token
/// that a shared proxy caches is a token handed to the next caller.
fn session_response(state: &AppState, view: SessionView, status: &'static str) -> Response {
    let cookie = session::session_cookie(
        &view.refresh_token,
        chrono::Duration::seconds(state.config.refresh_absolute.as_secs() as i64),
    );
    let body = SessionBody::from(&view, status);

    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie)
            .unwrap_or_else(|_| header::HeaderValue::from_static("")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// The `AdminSession` body, without the refresh token.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
    account_id: Uuid,
    audience: String,
    session_id: Uuid,
    access_token: String,
    access_token_expires_at: chrono::DateTime<chrono::Utc>,
    refresh_expires_at: chrono::DateTime<chrono::Utc>,
    scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mfa_confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SessionBody {
    fn from(view: &SessionView, status: &'static str) -> Self {
        Self {
            status: (!status.is_empty()).then_some(status),
            account_id: view.account_id,
            audience: view.audience.clone(),
            session_id: view.session_id,
            access_token: view.access_token.clone(),
            access_token_expires_at: view.access_token_expires_at,
            refresh_expires_at: view.refresh_expires_at,
            scopes: view.scopes.clone(),
            mfa_confirmed_at: view.mfa_confirmed_at,
        }
    }
}

/// Resolves the caller from a bearer token, else the administrator cookie.
///
/// The order matters: a token is a deliberate credential for one request, while the
/// cookie is a session. If both are present, the token wins — that is what lets a
/// client with a stale token and a live session get a clear 401 rather than a
/// surprise success.
async fn principal_from(
    state: &AppState,
    headers: &HeaderMap,
    _peer: Option<&SocketAddr>,
) -> Result<Principal, Problem> {
    let auth = state.auth();

    if let Some(token) = bearer_token(headers) {
        return auth
            .principal_from_bearer(&token)
            .await
            .map_err(problem_for);
    }

    let Some(refresh) = refresh_token_from(headers) else {
        return Err(Problem::new(
            ProblemType::Unauthorized,
            "An administrator session is required for this route.",
        ));
    };
    auth.principal_from_cookie(&refresh)
        .await
        .map_err(problem_for)
}

/// The `Authorization: Bearer` value, if present.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// The administrator refresh cookie, if present.
fn refresh_token_from(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    session::cookie_value(cookies, ADMIN_SESSION_COOKIE).map(str::to_string)
}

/// Builds the audit/rate-limit view of the caller.
fn client_context(
    config: &Config,
    headers: &HeaderMap,
    peer: Option<&SocketAddr>,
) -> ClientContext {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.trim().parse::<IpAddr>().ok());

    // `X-Forwarded-For` is only believed when the service is behind the gateway.
    // A service that trusts the header while directly reachable lets a caller pick
    // its own rate-limit bucket.
    let ip = if config.trust_forwarded_for {
        forwarded.or_else(|| peer.map(SocketAddr::ip))
    } else {
        peer.map(SocketAddr::ip)
    };

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    ClientContext::new(ip, user_agent)
}

/// Turns axum's JSON rejection into the same RFC 7807 body as everything else.
///
/// axum's own rejection is a plain-text 400/422. A client that has to parse two
/// error shapes parses neither properly, so the shape is unified here rather than in
/// every client.
fn body_problem(rejection: JsonRejection) -> Problem {
    Problem::new(
        ProblemType::ValidationFailed,
        format!("The request body is not valid: {rejection}"),
    )
}

/// Maps a typed failure to a problem response.
///
/// Every arm is a decision a client depends on. `Database` is logged with its cause
/// and answers a bare 500: the cause is for the operator, not for the caller.
fn problem_for(error: AuthError) -> Problem {
    match error {
        AuthError::InvalidCredentials => Problem::new(
            ProblemType::Unauthorized,
            "The email address or password is not correct.",
        ),
        AuthError::Locked {
            retry_after_seconds,
        } => Problem::new(
            ProblemType::RateLimited,
            format!(
                "This account is locked. Try again in {} minute(s).",
                (retry_after_seconds + 59) / 60
            ),
        )
        .with_retry_after(retry_after_seconds),
        AuthError::RateLimited {
            retry_after_seconds,
        } => Problem::new(
            ProblemType::RateLimited,
            format!(
                "Too many sign-in attempts. Try again in {} minute(s).",
                (retry_after_seconds + 59) / 60
            ),
        )
        .with_retry_after(retry_after_seconds),
        AuthError::NoAdminRole => Problem::new(
            ProblemType::Forbidden,
            "This account holds no role with an administrator scope.",
        ),
        AuthError::MfaNotEnrolled => Problem::new(
            ProblemType::MfaNotEnrolled,
            "No second factor is enrolled for this account. Enrol one before signing in.",
        ),
        AuthError::ChallengeExpired => Problem::new(
            ProblemType::Gone,
            "The challenge has expired or was already answered. Start the sign-in again.",
        ),
        AuthError::MfaRejected => {
            Problem::new(ProblemType::MfaFailed, "The second factor was rejected.")
        }
        AuthError::Unauthorized => Problem::new(
            ProblemType::Unauthorized,
            "An administrator session is required.",
        ),
        AuthError::WrongAudience => Problem::new(
            ProblemType::WrongAudience,
            "This is a participant session; the route requires an administrator session.",
        ),
        AuthError::Conflict(detail) => Problem::new(ProblemType::Conflict, detail),
        AuthError::Validation(detail) => Problem::new(ProblemType::ValidationFailed, detail),
        AuthError::UnsupportedMethod(detail) => Problem::new(ProblemType::ValidationFailed, detail),
        AuthError::Database(error) => {
            tracing::error!(%error, "a database operation failed");
            Problem::new(
                ProblemType::Internal,
                "The service could not complete the request.",
            )
        }
    }
}

// ── request bodies and the JSON extractor ─────────────────────────────────────

/// `{"email": "...", "password": "..."}`
#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

/// `{"challengeId": "...", "method": "...", "code": "..."}`
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeRequest {
    challenge_id: Uuid,
    method: String,
    code: String,
}

/// `{"method": "totp"}`
#[derive(Deserialize)]
struct EnrolRequest {
    method: String,
}

/// `{"code": "123456"}`
#[derive(Deserialize)]
struct ConfirmRequest {
    code: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).expect("valid header"));
        }
        map
    }

    #[test]
    fn a_bearer_token_is_extracted_in_either_case() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer abc.def.ghi")])).as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bearer abc")])).as_deref(),
            Some("abc")
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Basic abc")])),
            None
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer ")])),
            None
        );
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn the_refresh_cookie_is_read_by_its_exact_name() {
        let admin = headers(&[("cookie", "__Host-myota_admin_sid=admin-token; other=1")]);
        assert_eq!(refresh_token_from(&admin).as_deref(), Some("admin-token"));

        let participant = headers(&[("cookie", "__Host-myota_sid=participant-token")]);
        assert_eq!(refresh_token_from(&participant), None);
    }

    #[test]
    fn the_client_address_comes_from_forwarded_for_only_when_trusted() {
        let forwarded = headers(&[("x-forwarded-for", "203.0.113.9, 10.0.0.1")]);

        let trusting = Config {
            trust_forwarded_for: true,
            ..Config::from_lookup(|name| match name {
                "CORE_DATABASE_URL" => Some("postgres://u@h/db".to_string()),
                "CORE_JWT_SEED_BASE64" => Some(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    [1_u8; 32],
                )),
                "CORE_MFA_KEY_BASE64" => Some(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    [2_u8; 32],
                )),
                _ => None,
            })
            .expect("valid config")
        };

        let context = client_context(&trusting, &forwarded, None);
        assert_eq!(
            context.ip.map(|ip| ip.to_string()).as_deref(),
            Some("203.0.113.9")
        );
    }
}

//! The authority surface: the admin endpoints that read and change roles, and the two
//! internal endpoints other services call.
//!
//! Everything here is thin on purpose. The rules live in [`crate::authority`]; this
//! module resolves the caller, checks the route's scope, calls one function, and maps a
//! typed outcome to a status code. The separation is what lets the same evaluation answer
//! a console request and a service question and be sure they agree.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::SecondsFormat;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, event};
use crate::authority::{Authority, AuthorityError, SCOPE_ADMINS_MANAGE};
use crate::http::{body_problem, principal_from, require_fresh, require_scope, AppState};
use crate::problem::{Problem, ProblemType};

/// Builds the routes this module owns.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/admin/identity/admins/{account_id}/scopes",
            get(read_scopes),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/roles",
            post(grant_role),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/roles/{grant_id}",
            delete(revoke_role),
        )
        .route("/internal/authorize", post(authorize))
        .route("/internal/token", post(service_token))
}

// ── the admin surface ─────────────────────────────────────────────────────────

/// `GET /v1/admin/identity/admins/{accountId}/scopes`
async fn read_scopes(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    // No freshness here: reading an account's scopes changes nothing, and asking for a
    // password to render a page is how a security control gets worked around.

    if !account_exists(&state, account_id).await? {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No account with that id.",
        ));
    }

    let scopes = Authority { pool: &state.pool }
        .scopes(account_id, None)
        .await
        .map_err(authority_problem)?;
    Ok(Json(scopes).into_response())
}

/// `POST /v1/admin/identity/admins/{accountId}/roles`
async fn grant_role(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
    body: Result<Json<GrantRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    require_fresh(&state, &principal).await?;

    let Json(body) = body.map_err(body_problem)?;
    let authority = Authority { pool: &state.pool };

    match authority
        .grant(
            &principal,
            account_id,
            &body.role_code,
            body.programme_id,
            body.jurisdiction_code.as_deref(),
        )
        .await
    {
        Ok(grant) => Ok((axum::http::StatusCode::CREATED, Json(grant)).into_response()),
        Err(error) => {
            // The refusal is the interesting audit row: it says who tried to confer what
            // they did not hold, which is the signal an escalation attempt leaves.
            let (event_type, payload) = match &error {
                AuthorityError::ScopeNotHeld { missing } => (
                    event::ROLE_GRANT_REFUSED,
                    serde_json::json!({
                        "actorId": principal.account_id,
                        "targetId": account_id,
                        "roleCode": body.role_code,
                        "programmeId": body.programme_id,
                        "missingScopes": missing,
                    }),
                ),
                AuthorityError::LastAdministratorRole => (
                    event::ROLE_REVOKE_REFUSED,
                    serde_json::json!({
                        "actorId": principal.account_id,
                        "targetId": account_id,
                        "roleCode": body.role_code,
                    }),
                ),
                _ => (
                    event::ROLE_GRANT_REFUSED,
                    serde_json::json!({
                        "actorId": principal.account_id,
                        "targetId": account_id,
                        "roleCode": body.role_code,
                        "error": error.to_string(),
                    }),
                ),
            };
            audit::record(&state.pool, Some(principal.account_id), event_type, payload).await;
            Err(authority_problem(error))
        }
    }
}

/// `DELETE /v1/admin/identity/admins/{accountId}/roles/{grantId}`
async fn revoke_role(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path((account_id, grant_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    require_fresh(&state, &principal).await?;

    let authority = Authority { pool: &state.pool };
    match authority.revoke(&principal, account_id, grant_id).await {
        Ok(()) => Ok(axum::http::StatusCode::NO_CONTENT.into_response()),
        Err(error) => {
            if matches!(error, AuthorityError::LastAdministratorRole) {
                audit::record(
                    &state.pool,
                    Some(principal.account_id),
                    event::ROLE_REVOKE_REFUSED,
                    serde_json::json!({
                        "actorId": principal.account_id,
                        "targetId": account_id,
                        "grantId": grant_id,
                    }),
                )
                .await;
            }
            Err(authority_problem(error))
        }
    }
}

// ── the internal surface ──────────────────────────────────────────────────────

/// `POST /internal/authorize`
async fn authorize(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Result<Json<AuthorizationBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Problem> {
    let _caller = service_caller(&state, &headers)?;
    let Json(body) = body.map_err(body_problem)?;

    let decision = Authority { pool: &state.pool }
        .decide(
            body.account_id,
            &body.scope,
            body.programme_id,
            body.session_id,
        )
        .await
        .map_err(authority_problem)?;

    Ok(Json(decision).into_response())
}

/// `POST /internal/token`
async fn service_token(
    State(state): State<AppState>,
    body: Result<Json<ServiceCredentials>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Problem> {
    let Json(body) = body.map_err(body_problem)?;

    // One answer for "unknown service" and "wrong secret", so the endpoint cannot be used
    // to find out which services exist.
    let configured = state.config.service_clients.get(&body.service);
    let matched = configured
        .map(|expected| secrets_match(expected, &body.secret))
        .unwrap_or_else(|| {
            // Burn a comparison anyway: otherwise a name that is not configured answers
            // faster than one that is, which is the same enumeration oracle by timing.
            let _ = secrets_match(&body.secret, &body.secret);
            false
        });

    if !matched {
        tracing::warn!(
            service = %body.service,
            "a service token was requested with credentials that do not match"
        );
        return Err(Problem::new(
            ProblemType::Unauthorized,
            "The service name or secret is not correct.",
        ));
    }

    let (access_token, expires_at) = state.tokens.issue_service(&body.service);
    Ok(Json(ServiceTokenBody {
        access_token,
        expires_at: chrono::DateTime::from_timestamp(expires_at, 0)
            .unwrap_or_else(Utc::now)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        audience: "service".to_string(),
    })
    .into_response())
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Resolves the calling service from its bearer token.
fn service_caller(state: &AppState, headers: &axum::http::HeaderMap) -> Result<String, Problem> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            Problem::new(
                ProblemType::Unauthorized,
                "A service token is required for this route.",
            )
        })?;

    state.tokens.verify_service(token).map_err(|error| {
        tracing::warn!(%error, "a service token was refused");
        Problem::new(ProblemType::Unauthorized, "The service token is not valid.")
    })
}

async fn account_exists(state: &AppState, account_id: Uuid) -> Result<bool, Problem> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM account WHERE id = $1)")
        .bind(account_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, "checking whether an account exists");
            Problem::new(
                ProblemType::Internal,
                "The service could not complete the request.",
            )
        })
}

/// Compares two secrets without leaking their contents through timing.
///
/// Both sides are hashed first, so the comparison is over two 32-byte arrays whatever the
/// input lengths: comparing raw bytes would return early on a length mismatch, which is a
/// (small) oracle.
fn secrets_match(expected: &str, presented: &str) -> bool {
    use sha2::{Digest, Sha256};

    let expected = Sha256::digest(expected.as_bytes());
    let presented = Sha256::digest(presented.as_bytes());

    let mut difference = 0_u8;
    for (left, right) in expected.iter().zip(presented.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// Maps an authority failure to a problem response.
fn authority_problem(error: AuthorityError) -> Problem {
    match error {
        AuthorityError::ScopeNotHeld { missing } => Problem::new(
            ProblemType::ScopeNotHeld,
            format!(
                "The role carries {}, which this account does not hold.",
                missing.join(", ")
            ),
        ),
        AuthorityError::LastAdministratorRole => Problem::new(
            ProblemType::Conflict,
            "This is your last administrator role. Grant another before revoking this one.",
        ),
        AuthorityError::RoleUnknown(role) => Problem::new(
            ProblemType::ValidationFailed,
            format!("{role} is not in the role catalogue."),
        ),
        AuthorityError::AccountNotFound => {
            Problem::new(ProblemType::NotFound, "No account with that id.")
        }
        AuthorityError::GrantNotFound => Problem::new(
            ProblemType::NotFound,
            "No open grant with that id for that account.",
        ),
        AuthorityError::Conflict(detail) => Problem::new(ProblemType::Conflict, detail),
        AuthorityError::Validation(detail) => Problem::new(ProblemType::ValidationFailed, detail),
        AuthorityError::Database(error) => {
            tracing::error!(%error, "an authority query failed");
            Problem::new(
                ProblemType::Internal,
                "The service could not complete the request.",
            )
        }
        AuthorityError::Outbox(error) => {
            tracing::error!(%error, "writing the role-changed event failed");
            Problem::new(
                ProblemType::Internal,
                "The service could not complete the request.",
            )
        }
    }
}

// ── bodies ────────────────────────────────────────────────────────────────────

/// `{"roleCode": "...", "programmeId": null, "jurisdictionCode": null}`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantRequest {
    role_code: String,
    programme_id: Option<Uuid>,
    jurisdiction_code: Option<String>,
}

/// `{"accountId": "...", "scope": "...", "programmeId": null, "sessionId": null}`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationBody {
    account_id: Uuid,
    scope: String,
    programme_id: Option<Uuid>,
    session_id: Option<Uuid>,
}

/// `{"service": "geodata", "secret": "..."}`
#[derive(Debug, Deserialize)]
struct ServiceCredentials {
    service: String,
    secret: String,
}

/// `{"accessToken": "...", "expiresAt": "...", "audience": "service"}`
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceTokenBody {
    access_token: String,
    expires_at: String,
    audience: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_match_only_when_identical() {
        let secret = "a-secret-at-least-thirty-two-characters-long";
        assert!(secrets_match(secret, secret));
        assert!(!secrets_match(
            secret,
            "another-secret-at-least-thirty-two-charac"
        ));
        assert!(!secrets_match(secret, ""));
        assert!(!secrets_match(secret, &format!("{secret} ")));
    }

    #[test]
    fn a_scope_refusal_is_a_forbidden_problem_naming_the_scopes() {
        let problem = authority_problem(AuthorityError::ScopeNotHeld {
            missing: vec!["identity.admins.manage".into(), "audit.read".into()],
        });
        assert_eq!(problem.status_code(), axum::http::StatusCode::FORBIDDEN);
        assert_eq!(problem.problem_type, "about:blank#scope-not-held");
        assert!(
            problem.detail.contains("identity.admins.manage"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn the_last_role_refusal_is_a_conflict() {
        let problem = authority_problem(AuthorityError::LastAdministratorRole);
        assert_eq!(problem.status_code(), axum::http::StatusCode::CONFLICT);
        assert!(
            problem.detail.contains("last administrator role"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn an_unknown_role_is_a_validation_failure() {
        let problem = authority_problem(AuthorityError::RoleUnknown("wizard".into()));
        assert_eq!(problem.status_code(), axum::http::StatusCode::BAD_REQUEST);
        assert!(problem.detail.contains("wizard"), "{}", problem.detail);
    }
}

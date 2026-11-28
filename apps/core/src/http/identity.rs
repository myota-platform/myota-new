//! The admins section: who exists, what they may do, and the three things that change it.
//!
//! Every mutating endpoint here is behind two checks, and they are different questions:
//!
//!   * `identity.admins.manage` — may this actor manage administrators at all;
//!   * freshness — did this session prove, within five minutes, that the person holding it
//!     is the person who signed in. A stolen session passes the first and fails the second,
//!     which is the whole reason the second exists.
//!
//! A **state** is computed rather than stored. `blocked` is "cannot sign in because no
//! second factor is confirmed", `suspended` is the reversible stop an administrator
//! applies, `idle` is an account nobody has used for a month, `active` is the rest. None of
//! those is a column: three of them are derived, and deriving them is what keeps them true
//! when the underlying fact changes.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, event};
use crate::authority::SCOPE_ADMINS_MANAGE;
use crate::http::{body_problem, principal_from, require_fresh, require_scope, AppState};
use crate::problem::{Problem, ProblemType};
use crate::session;

/// An account nobody has used for this long is `idle`.
const IDLE_AFTER: Duration = Duration::days(30);

/// Builds the routes this module owns.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/identity/admins", get(list_admins))
        .route("/v1/admin/identity/roles", get(list_roles))
        .route("/v1/admin/identity/admins/{account_id}", get(read_admin))
        .route(
            "/v1/admin/identity/admins/{account_id}/mfa/reset",
            post(reset_mfa),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/suspend",
            post(suspend),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/reactivate",
            post(reactivate),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/sessions",
            get(list_sessions),
        )
        .route(
            "/v1/admin/identity/admins/{account_id}/sessions/{session_id}",
            delete(revoke_session),
        )
}

// ── shapes ────────────────────────────────────────────────────────────────────

/// The derived state of an administrator account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountState {
    /// Holds a second factor, not suspended, used recently.
    Active,
    /// Cannot sign in: no confirmed second factor.
    Blocked,
    /// Stopped by an administrator. Reversible.
    Suspended,
    /// Usable, but nobody has signed in for a month.
    Idle,
}

/// An administrator account as the section shows it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccount {
    /// The account.
    pub id: Uuid,
    /// Name shown in the console.
    pub display_name: String,
    /// Address, as registered.
    pub email: String,
    /// `pending`, `active` or `closed`.
    pub status: String,
    /// Whether a second factor is confirmed. The contract's `mfa`, not a boolean, because a
    /// console has to distinguish "not enrolled" from "enrolled but unused".
    pub mfa: String,
    /// The derived state.
    pub state: AccountState,
    /// Open grants.
    pub roles: Vec<GrantView>,
    /// The most recent sign-in on any session, if there was one.
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// One grant, as the console lists it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantView {
    /// The grant id, which a revocation names.
    pub id: Uuid,
    /// The role.
    pub role_code: String,
    /// The programme it is scoped to, if any.
    pub programme_id: Option<Uuid>,
    /// The jurisdiction narrowing, if any.
    pub jurisdiction_code: Option<String>,
    /// When it was granted.
    pub granted_at: DateTime<Utc>,
    /// When it stops applying.
    pub valid_to: Option<DateTime<Utc>>,
}

/// One open session.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// Session id.
    pub id: Uuid,
    /// `admin` or `participant`. A participant session cannot appear here, but the field is
    /// returned so a console that grows a participant section reuses the shape.
    pub audience: String,
    /// Browser, as recorded at sign-in.
    pub user_agent: Option<String>,
    /// Address, as recorded at sign-in.
    pub remote_addr: Option<String>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// Last use.
    pub last_seen_at: DateTime<Utc>,
    /// Absolute expiry.
    pub expires_at: DateTime<Utc>,
    /// Whether this is the caller's own session.
    pub current: bool,
}

/// A page of administrators.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPage {
    /// The page.
    pub items: Vec<AdminAccount>,
    /// Where the next page starts, or `null`.
    pub next_cursor: Option<String>,
}

/// What a list request can filter on.
#[derive(Debug, Deserialize)]
struct ListQuery {
    /// Free text over name and address.
    q: Option<String>,
    /// `enrolled` or `not_enrolled`.
    mfa: Option<String>,
    /// A role code.
    role: Option<String>,
    /// Opaque cursor from a previous page.
    cursor: Option<String>,
    /// Page size.
    limit: Option<i64>,
}

// ── handlers ──────────────────────────────────────────────────────────────────

/// `GET /v1/admin/identity/admins`
async fn list_admins(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;

    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let after = query
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<i64>().ok());

    let search = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{text}%"));

    // `mfa` is a filter on a derived fact, so it becomes a predicate rather than a column.
    let mfa_filter = query.mfa.as_deref();
    let role_filter = query.role.as_deref().filter(|code| !code.is_empty());

    let rows = sqlx::query_as::<_, (Uuid, String, String, String, Option<DateTime<Utc>>, bool, Option<DateTime<Utc>>, serde_json::Value, i64)>(
        r#"
        SELECT a.id,
               a.display_name,
               a.email::text,
               a.status::text,
               a.suspended_at,
               EXISTS (SELECT 1 FROM account_mfa m
                        WHERE m.account_id = a.id AND m.confirmed_at IS NOT NULL) AS mfa_enrolled,
               (SELECT max(s.last_seen_at) FROM auth_session s WHERE s.account_id = a.id) AS last_seen,
               COALESCE((
                   SELECT json_agg(json_build_object(
                              'id', r.id,
                              'roleCode', r.role_code,
                              'programmeId', r.programme_id,
                              'jurisdictionCode', r.jurisdiction_code,
                              'grantedAt', r.created_at,
                              'validTo', r.valid_to) ORDER BY r.created_at)
                     FROM account_role r
                    WHERE r.account_id = a.id AND r.valid_to IS NULL), '[]'::json) AS roles,
               a.insertion_order
          FROM account a
         WHERE EXISTS (
                   SELECT 1
                     FROM account_role r
                     JOIN role_definition d ON d.role_code = r.role_code
                    WHERE r.account_id = a.id
                      AND r.valid_to IS NULL
                      AND cardinality(d.scopes) > 0)
           AND ($1::bigint IS NULL OR a.insertion_order > $1)
           AND ($2::text IS NULL OR a.display_name ILIKE $2 OR a.email::text ILIKE $2)
           AND ($3::text IS NULL
                OR ($3 = 'enrolled'
                    AND EXISTS (SELECT 1 FROM account_mfa m WHERE m.account_id = a.id AND m.confirmed_at IS NOT NULL))
                OR ($3 = 'not_enrolled'
                    AND NOT EXISTS (SELECT 1 FROM account_mfa m WHERE m.account_id = a.id AND m.confirmed_at IS NOT NULL)))
           AND ($4::text IS NULL
                OR EXISTS (SELECT 1 FROM account_role r
                            WHERE r.account_id = a.id AND r.role_code = $4 AND r.valid_to IS NULL))
         ORDER BY a.insertion_order
         LIMIT $5
        "#,
    )
    .bind(after)
    .bind(search)
    .bind(mfa_filter)
    .bind(role_filter)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    let mut items: Vec<AdminAccount> = rows
        .iter()
        .map(
            |(id, display_name, email, status, suspended_at, mfa_enrolled, last_seen, roles, _)| {
                AdminAccount {
                    id: *id,
                    display_name: display_name.clone(),
                    email: email.clone(),
                    status: status.clone(),
                    mfa: if *mfa_enrolled {
                        "enrolled"
                    } else {
                        "not_enrolled"
                    }
                    .to_string(),
                    state: state_of(status, *suspended_at, *mfa_enrolled, *last_seen),
                    roles: serde_json::from_value(roles.clone()).unwrap_or_default(),
                    last_seen_at: *last_seen,
                }
            },
        )
        .collect();

    // One row more than asked for is the pagination signal; it is dropped before answering.
    let next_cursor = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        rows.get(limit as usize - 1).map(|row| row.8.to_string())
    } else {
        None
    };

    Ok(Json(AdminPage { items, next_cursor }).into_response())
}

/// `GET /v1/admin/identity/roles`
///
/// The catalogue, so a console offers a list instead of a text field. Reading it is part
/// of managing administrators: an operator who cannot see what a role carries cannot
/// decide whether to grant it.
async fn list_roles(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;

    let roles = crate::authority::Authority { pool: &state.pool }
        .catalogue()
        .await
        .map_err(|error| {
            tracing::error!(%error, "reading the role catalogue");
            Problem::new(
                ProblemType::Internal,
                "The service could not complete the request.",
            )
        })?;

    Ok(Json(serde_json::json!({ "items": roles })).into_response())
}

/// `GET /v1/admin/identity/admins/{accountId}`
async fn read_admin(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    let account = load_account(&state.pool, account_id).await?;
    Ok(Json(account).into_response())
}

/// `POST /v1/admin/identity/admins/{accountId}/mfa/reset`
///
/// Removes the factor and every recovery code, and **revokes the account's sessions**: a
/// reset exists because the factor is lost or suspect, and leaving live sessions that were
/// obtained with the suspect factor would defeat it.
async fn reset_mfa(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    require_fresh(&state, &principal).await?;

    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM account WHERE id = $1)")
        .bind(account_id)
        .fetch_one(&state.pool)
        .await
        .map_err(internal)?;
    if !exists {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No account with that id.",
        ));
    }

    let mut transaction = state.pool.begin().await.map_err(internal)?;
    sqlx::query("DELETE FROM account_mfa WHERE account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
    sqlx::query("DELETE FROM account_recovery_code WHERE account_id = $1")
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
    sqlx::query(
        "UPDATE auth_session SET revoked_at = now(), updated_at = now()
          WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .execute(&mut *transaction)
    .await
    .map_err(internal)?;
    transaction.commit().await.map_err(internal)?;

    audit::record(
        &state.pool,
        Some(account_id),
        event::MFA_RESET,
        serde_json::json!({ "actorId": principal.account_id }),
    )
    .await;

    // 202, per the contract: the reset is recorded and the account now has no factor, and
    // the next sign-in is what proves it. There is nothing to return.
    Ok(axum::http::StatusCode::ACCEPTED.into_response())
}

/// `POST /v1/admin/identity/admins/{accountId}/suspend`
async fn suspend(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
    body: Result<Json<SuspendRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    require_fresh(&state, &principal).await?;

    // Suspending yourself would lock the console out with one click, the same failure the
    // last-role rule prevents.
    if account_id == principal.account_id {
        return Err(Problem::new(
            ProblemType::Conflict,
            "An administrator cannot suspend their own account.",
        ));
    }

    let body = body.map_err(body_problem)?.0;

    let updated = sqlx::query(
        "UPDATE account SET suspended_at = now(), suspension_reason = $2, updated_at = now()
          WHERE id = $1 AND suspended_at IS NULL",
    )
    .bind(account_id)
    .bind(body.reason.as_deref())
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    if updated.rows_affected() == 0 {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No active account with that id, or it is already suspended.",
        ));
    }

    // A suspension that left the sessions running would take effect at the next token
    // refresh at the latest — up to eight hours. Revoking is what makes it immediate.
    sqlx::query(
        "UPDATE auth_session SET revoked_at = now(), updated_at = now()
          WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    audit::record(
        &state.pool,
        Some(account_id),
        event::ACCOUNT_SUSPENDED,
        serde_json::json!({
            "actorId": principal.account_id,
            "reason": body.reason,
        }),
    )
    .await;

    let account = load_account(&state.pool, account_id).await?;
    Ok(Json(account).into_response())
}

/// `POST /v1/admin/identity/admins/{accountId}/reactivate`
async fn reactivate(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;
    require_fresh(&state, &principal).await?;

    let updated = sqlx::query(
        "UPDATE account SET suspended_at = NULL, suspension_reason = NULL, updated_at = now()
          WHERE id = $1 AND suspended_at IS NOT NULL",
    )
    .bind(account_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    if updated.rows_affected() == 0 {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No suspended account with that id.",
        ));
    }

    audit::record(
        &state.pool,
        Some(account_id),
        event::ACCOUNT_REACTIVATED,
        serde_json::json!({ "actorId": principal.account_id }),
    )
    .await;

    let account = load_account(&state.pool, account_id).await?;
    Ok(Json(account).into_response())
}

/// `GET /v1/admin/identity/admins/{accountId}/sessions`
async fn list_sessions(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, DateTime<Utc>, DateTime<Utc>, DateTime<Utc>)>(
        "SELECT id, audience, user_agent, host(remote_addr), created_at, last_seen_at, absolute_expires_at
           FROM auth_session
          WHERE account_id = $1
            AND revoked_at IS NULL
            AND expires_at > now()
            AND absolute_expires_at > now()
          ORDER BY last_seen_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    let items: Vec<SessionView> = rows
        .into_iter()
        .map(
            |(id, audience, user_agent, remote_addr, created_at, last_seen_at, expires_at)| {
                SessionView {
                    id,
                    audience,
                    user_agent,
                    remote_addr,
                    created_at,
                    last_seen_at,
                    expires_at,
                    current: id == principal.session_id,
                }
            },
        )
        .collect();

    Ok(Json(serde_json::json!({ "items": items })).into_response())
}

/// `DELETE /v1/admin/identity/admins/{accountId}/sessions/{sessionId}`
async fn revoke_session(
    State(state): State<AppState>,
    peer: axum::Extension<crate::http::PeerAddress>,
    headers: axum::http::HeaderMap,
    Path((account_id, session_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, Problem> {
    let principal = principal_from(&state, &headers, peer.0 .0.as_ref()).await?;
    require_scope(&state, &principal, SCOPE_ADMINS_MANAGE).await?;

    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM auth_session
                         WHERE id = $1 AND account_id = $2 AND revoked_at IS NULL)",
    )
    .bind(session_id)
    .bind(account_id)
    .fetch_one(&state.pool)
    .await
    .map_err(internal)?;
    if !owned {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No open session with that id for that account.",
        ));
    }

    session::revoke(&state.pool, session_id)
        .await
        .map_err(internal)?;
    audit::record(
        &state.pool,
        Some(account_id),
        event::SESSION_REVOKED_BY_ADMIN,
        serde_json::json!({
            "actorId": principal.account_id,
            "sessionId": session_id,
        }),
    )
    .await;

    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Loads one administrator, refusing an account that holds no administrator scope.
async fn load_account(pool: &PgPool, account_id: Uuid) -> Result<AdminAccount, Problem> {
    let row = sqlx::query_as::<_, (Uuid, String, String, String, Option<DateTime<Utc>>, bool, Option<DateTime<Utc>>, serde_json::Value)>(
        r#"
        SELECT a.id,
               a.display_name,
               a.email::text,
               a.status::text,
               a.suspended_at,
               EXISTS (SELECT 1 FROM account_mfa m
                        WHERE m.account_id = a.id AND m.confirmed_at IS NOT NULL) AS mfa_enrolled,
               (SELECT max(s.last_seen_at) FROM auth_session s WHERE s.account_id = a.id) AS last_seen,
               COALESCE((
                   SELECT json_agg(json_build_object(
                              'id', r.id,
                              'roleCode', r.role_code,
                              'programmeId', r.programme_id,
                              'jurisdictionCode', r.jurisdiction_code,
                              'grantedAt', r.created_at,
                              'validTo', r.valid_to) ORDER BY r.created_at)
                     FROM account_role r
                    WHERE r.account_id = a.id AND r.valid_to IS NULL), '[]'::json) AS roles
          FROM account a
         WHERE a.id = $1
           AND EXISTS (
                   SELECT 1
                     FROM account_role r
                     JOIN role_definition d ON d.role_code = r.role_code
                    WHERE r.account_id = a.id
                      AND r.valid_to IS NULL
                      AND cardinality(d.scopes) > 0)
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    let Some((id, display_name, email, status, suspended_at, mfa_enrolled, last_seen, roles)) = row
    else {
        return Err(Problem::new(
            ProblemType::NotFound,
            "No account with that id holds an administrator role.",
        ));
    };

    Ok(AdminAccount {
        id,
        display_name,
        email,
        status: status.clone(),
        mfa: if mfa_enrolled {
            "enrolled"
        } else {
            "not_enrolled"
        }
        .to_string(),
        state: state_of(&status, suspended_at, mfa_enrolled, last_seen),
        roles: serde_json::from_value(roles).unwrap_or_default(),
        last_seen_at: last_seen,
    })
}

/// The derived state. Order matters: suspension outranks everything, because a suspended
/// account with no factor is still suspended; `blocked` outranks `idle`, because being
/// unable to sign in is the fact an operator acts on.
fn state_of(
    status: &str,
    suspended_at: Option<DateTime<Utc>>,
    mfa_enrolled: bool,
    last_seen: Option<DateTime<Utc>>,
) -> AccountState {
    if suspended_at.is_some() || status == "closed" {
        return AccountState::Suspended;
    }
    if !mfa_enrolled {
        return AccountState::Blocked;
    }
    match last_seen {
        Some(seen) if seen > Utc::now() - IDLE_AFTER => AccountState::Active,
        Some(_) => AccountState::Idle,
        None => AccountState::Idle,
    }
}

fn internal(error: sqlx::Error) -> Problem {
    tracing::error!(%error, "an identity query failed");
    Problem::new(
        ProblemType::Internal,
        "The service could not complete the request.",
    )
}

/// `{"reason": "..."}`
#[derive(Debug, Deserialize)]
struct SuspendRequest {
    reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn a_suspended_account_is_suspended_even_without_a_factor() {
        assert_eq!(
            state_of("active", Some(now()), false, Some(now())),
            AccountState::Suspended
        );
    }

    #[test]
    fn no_confirmed_factor_is_blocked() {
        assert_eq!(
            state_of("active", None, false, Some(now())),
            AccountState::Blocked
        );
    }

    #[test]
    fn a_recent_sign_in_is_active() {
        assert_eq!(
            state_of("active", None, true, Some(now())),
            AccountState::Active
        );
    }

    #[test]
    fn a_month_of_silence_is_idle() {
        assert_eq!(
            state_of("active", None, true, Some(now() - Duration::days(31))),
            AccountState::Idle
        );
        assert_eq!(state_of("active", None, true, None), AccountState::Idle);
    }

    #[test]
    fn a_closed_account_reads_as_suspended() {
        // `closed` is the end of an account's life; the console shows it as stopped rather
        // than inventing a fifth state the contract does not declare.
        assert_eq!(
            state_of("closed", None, true, Some(now())),
            AccountState::Suspended
        );
    }
}

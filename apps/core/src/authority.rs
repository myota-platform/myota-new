//! Authority: roles, scopes, and the one place a decision is made.
//!
//! ## Why this is a module and not a query in each handler
//!
//! "May this account do X to programme Y?" is asked from three directions — the admin
//! endpoints, `/internal/authorize` on behalf of `geodata` and `activity`, and the
//! checks that guard a role change. If each wrote its own SQL they would eventually
//! disagree, and the disagreements would be in the direction that grants rather than
//! refuses. There is one evaluation, [`Authority::scopes`], and everything else reads it.
//!
//! ## The two rules that make granting safe
//!
//! 1. **`granted ⊆ actor_scopes`.** Evaluated here, at write time, against the actor's
//!    scopes *for the programme being granted*. A holder of `identity.admins.manage` who
//!    can grant `identity.admins.manage` to anyone has root whatever the scope is
//!    called, so the check is not "may you grant roles" but "may you grant *these*
//!    scopes for *this* programme".
//! 2. **You cannot revoke your own last administrator role.** Otherwise one click locks
//!    the console out, and the recovery is a database console.
//!
//! ## Why decisions are computed from the database, not from the token
//!
//! An access token carries the scopes it was minted with — a snapshot, up to ten minutes
//! old. Answering `/internal/authorize` from that snapshot would give a revoked
//! administrator their authority back for the token's lifetime, which is the one thing a
//! privilege change must not allow. Every decision re-reads the open grants, and a
//! revocation is therefore effective on the next call.
//!
//! ## The event
//!
//! A grant or revocation writes `core.role.changed.v1` into `ops.outbox_event` **in the
//! same transaction**, carrying the affected open session ids. Consumers that cache a
//! decision drop it; the outbox is what makes that possible without a message broker
//! (ADR-0004).

use chrono::{DateTime, SecondsFormat, Utc};
use myota_ops::Event as OutboxEvent;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, event};
use crate::auth::Principal;

/// The scope that permits managing administrator accounts.
///
/// The one scope with no delegation: you may grant it only if you hold it, so it
/// propagates along a chain of trust rather than being handed out by a support role.
pub const SCOPE_ADMINS_MANAGE: &str = "identity.admins.manage";

/// The scope that permits managing participant accounts.
pub const SCOPE_PARTICIPANTS_MANAGE: &str = "identity.participants.manage";

/// One open grant.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    /// The grant's identity, which is what a revocation names.
    pub id: Uuid,
    /// The role, which carries the scopes.
    pub role_code: String,
    /// `None` when the grant is not scoped to a programme.
    pub programme_id: Option<Uuid>,
    /// Optional jurisdiction narrowing, for a review role that covers one region.
    pub jurisdiction_code: Option<String>,
    /// When it was granted.
    pub granted_at: DateTime<Utc>,
    /// When it stops applying; `None` while open.
    pub valid_to: Option<DateTime<Utc>>,
}

/// An account's effective authority.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountScopes {
    /// The account the scopes belong to.
    pub account_id: Uuid,
    /// Deduplicated union across the open grants, sorted.
    pub scopes: Vec<String>,
    /// The grants those scopes came from.
    pub roles: Vec<Grant>,
}

/// The answer to "may this account do X to programme Y?".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    /// Whether the action is permitted.
    pub allowed: bool,
    /// The account's scopes for the requested programme, sorted.
    pub effective_scopes: Vec<String>,
    /// Why not, when the answer is no.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Refusal>,
}

/// Why a decision went the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The account is real and active, and does not hold the scope.
    ScopeNotHeld,
    /// The session the caller named is not open: revoked, expired, or not this account's.
    SessionRevoked,
    /// The account is not `active`.
    AccountNotActive,
    /// No such account. A refusal rather than an error: the question was answered.
    AccountNotFound,
}

/// Everything that can go wrong, as this module sees it.
#[derive(Debug)]
pub enum AuthorityError {
    /// The actor tried to grant scopes they do not hold for that programme.
    ScopeNotHeld { missing: Vec<String> },
    /// The actor tried to remove their own last administrator role.
    LastAdministratorRole,
    /// The role code is not in the catalogue.
    RoleUnknown(String),
    /// The account is not in the database.
    AccountNotFound,
    /// The grant is not in the database, or not that account's.
    GrantNotFound,
    /// The grant already exists, or the account cannot hold it.
    Conflict(String),
    /// The request cannot be served as sent.
    Validation(String),
    /// The database failed.
    Database(sqlx::Error),
    /// The outbox write failed. Its own variant because it is the one write whose
    /// failure must roll the transaction back rather than be logged and ignored.
    Outbox(String),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScopeNotHeld { missing } => {
                write!(formatter, "scopes not held: {}", missing.join(", "))
            }
            Self::LastAdministratorRole => write!(formatter, "that is the actor's last role"),
            Self::RoleUnknown(role) => write!(formatter, "{role} is not in the role catalogue"),
            Self::AccountNotFound => write!(formatter, "no such account"),
            Self::GrantNotFound => write!(formatter, "no such grant"),
            Self::Conflict(detail) | Self::Validation(detail) => write!(formatter, "{detail}"),
            Self::Database(error) => write!(formatter, "database: {error}"),
            Self::Outbox(error) => write!(formatter, "outbox: {error}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

impl From<sqlx::Error> for AuthorityError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// The authority engine, over one pool.
pub struct Authority<'a> {
    /// The database.
    pub pool: &'a PgPool,
}

impl Authority<'_> {
    /// The account's effective scopes, optionally narrowed to one programme.
    ///
    /// The programme filter is what makes a programme-scoped grant mean something: a
    /// grant for programme A answers a question about programme A, and does not answer
    /// the same question about programme B. An unscoped grant answers every programme.
    pub async fn scopes(
        &self,
        account_id: Uuid,
        programme_id: Option<Uuid>,
    ) -> Result<AccountScopes, AuthorityError> {
        let roles = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<Uuid>,
                Option<String>,
                DateTime<Utc>,
                Option<DateTime<Utc>>,
            ),
        >(
            "SELECT id, role_code, programme_id, jurisdiction_code, created_at, valid_to
               FROM account_role
              WHERE account_id = $1 AND valid_to IS NULL
              ORDER BY created_at",
        )
        .bind(account_id)
        .fetch_all(self.pool)
        .await?
        .into_iter()
        .map(
            |(id, role_code, programme_id, jurisdiction_code, granted_at, valid_to)| Grant {
                id,
                role_code,
                programme_id,
                jurisdiction_code,
                granted_at,
                valid_to,
            },
        )
        .collect();

        let scopes = sqlx::query_scalar::<_, Vec<String>>(
            "SELECT COALESCE(array_agg(DISTINCT scope ORDER BY scope), '{}')
               FROM account_role r
               JOIN role_definition d ON d.role_code = r.role_code
               CROSS JOIN LATERAL unnest(d.scopes) AS scope
              WHERE r.account_id = $1
                AND r.valid_to IS NULL
                AND ($2::uuid IS NULL OR r.programme_id IS NULL OR r.programme_id = $2)",
        )
        .bind(account_id)
        .bind(programme_id)
        .fetch_one(self.pool)
        .await?;

        Ok(AccountScopes {
            account_id,
            scopes,
            roles,
        })
    }

    /// Answers whether `account_id` may perform `scope`, for `programme_id`.
    ///
    /// `session_id` is optional and, when given, decides first: a revoked session is a
    /// refusal even for an account that holds every scope. That is what makes signing
    /// out mean something inside an access token's ten-minute lifetime.
    pub async fn decide(
        &self,
        account_id: Uuid,
        scope: &str,
        programme_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> Result<Decision, AuthorityError> {
        let Some(status) =
            sqlx::query_scalar::<_, String>("SELECT status::text FROM account WHERE id = $1")
                .bind(account_id)
                .fetch_optional(self.pool)
                .await?
        else {
            return Ok(Decision {
                allowed: false,
                effective_scopes: Vec::new(),
                reason: Some(Refusal::AccountNotFound),
            });
        };

        if status != "active" {
            return Ok(Decision {
                allowed: false,
                effective_scopes: Vec::new(),
                reason: Some(Refusal::AccountNotActive),
            });
        }

        if let Some(session_id) = session_id {
            let open = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                     SELECT 1 FROM auth_session
                      WHERE id = $1 AND account_id = $2
                        AND revoked_at IS NULL
                        AND expires_at > now()
                        AND absolute_expires_at > now())",
            )
            .bind(session_id)
            .bind(account_id)
            .fetch_one(self.pool)
            .await?;
            if !open {
                return Ok(Decision {
                    allowed: false,
                    effective_scopes: Vec::new(),
                    reason: Some(Refusal::SessionRevoked),
                });
            }
        }

        let scopes = self.scopes(account_id, programme_id).await?.scopes;
        let allowed = scopes.iter().any(|held| held == scope);
        Ok(Decision {
            allowed,
            effective_scopes: scopes,
            reason: (!allowed).then_some(Refusal::ScopeNotHeld),
        })
    }

    /// Grants `role_code` to `account_id`, enforcing `granted ⊆ actor_scopes`.
    ///
    /// The subset check is redundant with the caller's own `require_scope` guard, and it
    /// is here anyway: the guard says "you may manage administrators", and this says
    /// "you may confer *these* scopes", which is the rule that stops the manage scope
    /// from being root.
    pub async fn grant(
        &self,
        actor: &Principal,
        account_id: Uuid,
        role_code: &str,
        programme_id: Option<Uuid>,
        jurisdiction_code: Option<&str>,
    ) -> Result<Grant, AuthorityError> {
        let role_code = role_code.trim();
        if role_code.is_empty() {
            return Err(AuthorityError::Validation("A roleCode is required.".into()));
        }

        let role_scopes = sqlx::query_scalar::<_, Vec<String>>(
            "SELECT scopes FROM role_definition WHERE role_code = $1",
        )
        .bind(role_code)
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| AuthorityError::RoleUnknown(role_code.to_string()))?;

        // The actor's authority *for the programme being granted*. A reviewer scoped to
        // one programme cannot use that grant to place the same scope on another.
        let actor_scopes = self.scopes(actor.account_id, programme_id).await?.scopes;
        let missing: Vec<String> = role_scopes
            .iter()
            .filter(|scope| !actor_scopes.iter().any(|held| held == *scope))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(AuthorityError::ScopeNotHeld { missing });
        }

        let status =
            sqlx::query_scalar::<_, String>("SELECT status::text FROM account WHERE id = $1")
                .bind(account_id)
                .fetch_optional(self.pool)
                .await?
                .ok_or(AuthorityError::AccountNotFound)?;
        if status != "active" {
            return Err(AuthorityError::Conflict(
                "The account is not active, so a role granted now would never be usable.".into(),
            ));
        }

        let already: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM account_role
                  WHERE account_id = $1 AND role_code = $2 AND valid_to IS NULL
                    AND programme_id IS NOT DISTINCT FROM $3::uuid)",
        )
        .bind(account_id)
        .bind(role_code)
        .bind(programme_id)
        .fetch_one(self.pool)
        .await?;
        if already {
            return Err(AuthorityError::Conflict(format!(
                "{role_code} is already granted to this account for that programme."
            )));
        }

        let grant_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;

        let granted_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO account_role (id, account_id, role_code, programme_id, jurisdiction_code, scopes)
             VALUES ($1, $2, $3, $4, $5, '{}'::jsonb)
         RETURNING created_at",
        )
        .bind(grant_id)
        .bind(account_id)
        .bind(role_code)
        .bind(programme_id)
        .bind(jurisdiction_code)
        .fetch_one(&mut *transaction)
        .await?;

        publish_role_change(
            &mut transaction,
            account_id,
            "granted",
            role_code,
            programme_id,
        )
        .await?;

        transaction.commit().await?;

        audit::record(
            self.pool,
            Some(account_id),
            event::ROLE_GRANTED,
            json!({
                "actorId": actor.account_id,
                "roleCode": role_code,
                "programmeId": programme_id,
                "jurisdictionCode": jurisdiction_code,
                "grantId": grant_id,
            }),
        )
        .await;

        Ok(Grant {
            id: grant_id,
            role_code: role_code.to_string(),
            programme_id,
            jurisdiction_code: jurisdiction_code.map(str::to_string),
            granted_at,
            valid_to: None,
        })
    }

    /// Revokes one grant, refusing when that would strip the actor of their last
    /// administrator role.
    ///
    /// The row is closed (`valid_to = now()`) rather than deleted: "when did this end"
    /// is a fact an audit asks for, and `account_role` carries no history of its own.
    pub async fn revoke(
        &self,
        actor: &Principal,
        account_id: Uuid,
        grant_id: Uuid,
    ) -> Result<(), AuthorityError> {
        let grant = sqlx::query_as::<_, (String, Option<Uuid>)>(
            "SELECT role_code, programme_id
               FROM account_role
              WHERE id = $1 AND account_id = $2 AND valid_to IS NULL",
        )
        .bind(grant_id)
        .bind(account_id)
        .fetch_optional(self.pool)
        .await?
        .ok_or(AuthorityError::GrantNotFound)?;
        let (role_code, programme_id) = grant;

        // The self-lockout guard. It counts *scopes*, not roles: an account holding two
        // roles that carry no scopes has no administrator authority either, so revoking
        // one of them is not the dangerous case.
        if account_id == actor.account_id {
            let remaining = sqlx::query_scalar::<_, i64>(
                "SELECT count(*)
                   FROM account_role r
                   JOIN role_definition d ON d.role_code = r.role_code
                  WHERE r.account_id = $1
                    AND r.valid_to IS NULL
                    AND r.id <> $2
                    AND cardinality(d.scopes) > 0",
            )
            .bind(account_id)
            .bind(grant_id)
            .fetch_one(self.pool)
            .await?;
            if remaining == 0 {
                return Err(AuthorityError::LastAdministratorRole);
            }
        }

        let mut transaction = self.pool.begin().await?;

        sqlx::query(
            "UPDATE account_role SET valid_to = now(), updated_at = now()
              WHERE id = $1 AND account_id = $2 AND valid_to IS NULL",
        )
        .bind(grant_id)
        .bind(account_id)
        .execute(&mut *transaction)
        .await?;

        publish_role_change(
            &mut transaction,
            account_id,
            "revoked",
            &role_code,
            programme_id,
        )
        .await?;

        transaction.commit().await?;

        audit::record(
            self.pool,
            Some(account_id),
            event::ROLE_REVOKED,
            json!({
                "actorId": actor.account_id,
                "roleCode": role_code,
                "programmeId": programme_id,
                "grantId": grant_id,
            }),
        )
        .await;

        Ok(())
    }

    /// Whether the account holds `scope` anywhere, reading the database.
    ///
    /// The authoritative form of "does the actor have the scope this route needs". The
    /// token's `scp` claim is deliberately not used: it is a snapshot, and a revoked
    /// role would keep working until the token expired.
    pub async fn holds(&self, account_id: Uuid, scope: &str) -> Result<bool, AuthorityError> {
        let held = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                 SELECT 1
                   FROM account_role r
                   JOIN role_definition d ON d.role_code = r.role_code
                  WHERE r.account_id = $1
                    AND r.valid_to IS NULL
                    AND $2 = ANY (d.scopes))",
        )
        .bind(account_id)
        .bind(scope)
        .fetch_one(self.pool)
        .await?;
        Ok(held)
    }

    /// The role catalogue, for an interface that needs to say what can be granted.
    pub async fn catalogue(&self) -> Result<Vec<RoleDefinition>, AuthorityError> {
        let roles = sqlx::query_as::<_, (String, String, Vec<String>, bool)>(
            "SELECT role_code, display_name, scopes, is_system
               FROM role_definition
              ORDER BY role_code",
        )
        .fetch_all(self.pool)
        .await?
        .into_iter()
        .map(
            |(role_code, display_name, scopes, is_system)| RoleDefinition {
                role_code,
                display_name,
                scopes,
                is_system,
            },
        )
        .collect();
        Ok(roles)
    }
}

/// One catalogue entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleDefinition {
    /// The code a grant names.
    pub role_code: String,
    /// What to call it in an interface.
    pub display_name: String,
    /// The scopes it carries.
    pub scopes: Vec<String>,
    /// Catalogue roles cannot be deleted by a console.
    pub is_system: bool,
}

/// Writes `core.role.changed.v1` inside the caller's transaction.
///
/// Same transaction on purpose: the event and the change commit together or not at all,
/// which is the whole of the transactional outbox (ADR-0004). The `sessionIds` are the
/// target account's **open** sessions — the caches that have to be dropped are theirs.
async fn publish_role_change(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    change: &str,
    role_code: &str,
    programme_id: Option<Uuid>,
) -> Result<(), AuthorityError> {
    let session_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM auth_session
          WHERE account_id = $1
            AND revoked_at IS NULL
            AND expires_at > now()
            AND absolute_expires_at > now()",
    )
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await?;

    let event_id = Uuid::new_v4();
    let envelope = json!({
        "specversion": "1.0",
        "id": event_id,
        "source": "/core",
        "type": "core.role.changed.v1",
        "subject": format!("account:{account_id}"),
        "time": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "datacontenttype": "application/json",
        "data": {
            "accountId": account_id,
            "change": change,
            "roleCode": role_code,
            "programmeId": programme_id,
            "sessionIds": session_ids,
        },
    });

    // `&mut *transaction` reborrows the transaction as a connection; the extra `*` the
    // compiler allows is what clippy objects to, and it is right.
    myota_ops::enqueue(
        &mut *transaction,
        &OutboxEvent {
            id: event_id,
            event_type: "core.role.changed.v1".to_string(),
            subject: format!("account:{account_id}"),
            payload: envelope,
            time: Some(Utc::now()),
        },
    )
    .await
    .map_err(|error| AuthorityError::Outbox(error.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_serialises_as_snake_case() {
        let decision = Decision {
            allowed: false,
            effective_scopes: vec![],
            reason: Some(Refusal::SessionRevoked),
        };
        let rendered = serde_json::to_value(&decision).expect("serialisable");
        assert_eq!(rendered["allowed"], false);
        assert_eq!(rendered["reason"], "session_revoked");
    }

    #[test]
    fn an_allowed_decision_omits_the_reason() {
        let decision = Decision {
            allowed: true,
            effective_scopes: vec!["geodata.review".into()],
            reason: None,
        };
        let rendered = serde_json::to_value(&decision).expect("serialisable");
        assert!(rendered.get("reason").is_none(), "{rendered}");
        assert_eq!(rendered["effectiveScopes"][0], "geodata.review");
    }

    #[test]
    fn a_grant_serialises_with_camel_case_keys() {
        let grant = Grant {
            id: Uuid::nil(),
            role_code: "programme_reviewer".into(),
            programme_id: None,
            jurisdiction_code: None,
            granted_at: Utc::now(),
            valid_to: None,
        };
        let rendered = serde_json::to_value(&grant).expect("serialisable");
        assert_eq!(rendered["roleCode"], "programme_reviewer");
        assert!(rendered.get("programmeId").is_some());
    }
}

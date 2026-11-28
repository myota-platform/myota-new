//! Roles, scopes and the single authorization decision, driven through the real router
//! against a real PostgreSQL.
//!
//! The properties here are the ones that make delegation safe, and none of them is a
//! property of a function: "you cannot confer a scope you do not hold", "a revocation is
//! effective on the next call", "a revoked session is refused even for an account that
//! holds every scope" are properties of SQL, of the HTTP surface, and of the order the
//! two are applied in.
//!
//! **They need a database.** See `tests/support/mod.rs`; without `CORE_TEST_DATABASE_URL`
//! they report why they were skipped and pass.

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::{bearer, Harness, TEST_SERVICE, TEST_SERVICE_SECRET};
use uuid::Uuid;

/// Obtains a service token from the exchange, as a real caller would.
async fn service_token(harness: &Harness) -> String {
    let (status, _, body) = harness
        .send(
            "POST",
            "/internal/token",
            Some(json!({ "service": TEST_SERVICE, "secret": TEST_SERVICE_SECRET })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["audience"], "service");
    body["accessToken"].as_str().expect("a token").to_string()
}

/// Asks the internal endpoint one question.
async fn authorize(harness: &Harness, token: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, body) = harness
        .send("POST", "/internal/authorize", Some(body), &[bearer(token)])
        .await;
    (status, body)
}

// ── the internal surface ──────────────────────────────────────────────────────

#[tokio::test]
async fn authorize_requires_a_service_token() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("no-service-token");
    let account_id = harness
        .account_with_role(&email, "an-administrator-passphrase", "platform_admin")
        .await;

    let question = json!({ "accountId": account_id, "scope": "geodata.review" });

    // No credential at all.
    let (status, _, body) = harness
        .send("POST", "/internal/authorize", Some(question.clone()), &[])
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["type"], "about:blank#unauthorized");

    // An administrator's session token is not a service credential: right shape, wrong
    // audience.
    let (admin_token, _) = harness.tokens.issue(
        account_id,
        Uuid::new_v4(),
        Uuid::new_v4(),
        myota_core::token::AUDIENCE_ADMIN,
        &[],
    );
    let (status, _, body) = harness
        .send(
            "POST",
            "/internal/authorize",
            Some(question),
            &[bearer(&admin_token)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["type"], "about:blank#unauthorized");
}

#[tokio::test]
async fn the_service_token_exchange_refuses_a_wrong_secret() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    for body in [
        json!({ "service": TEST_SERVICE, "secret": "the-wrong-secret-at-least-32-characters" }),
        json!({ "service": "not-a-service", "secret": TEST_SERVICE_SECRET }),
    ] {
        let (status, _, response) = harness
            .send("POST", "/internal/token", Some(body.clone()), &[])
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body} → {response}");
        // One message for both, so the endpoint cannot enumerate the services that exist.
        assert_eq!(
            response["detail"],
            "The service name or secret is not correct."
        );
    }
}

#[tokio::test]
async fn authorize_answers_a_scope_question_from_the_database() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let token = service_token(&harness).await;

    let email = Harness::email("decide");
    let account_id = harness
        .account_with_role(&email, "an-administrator-passphrase", "programme_reviewer")
        .await;

    // Held.
    let (status, body) = authorize(
        &harness,
        &token,
        json!({ "accountId": account_id, "scope": "geodata.review" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["allowed"], true);
    assert!(body["effectiveScopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .any(|scope| scope == "geodata.review"));

    // Not held: a refusal, not an error — the question was answerable.
    let (status, body) = authorize(
        &harness,
        &token,
        json!({ "accountId": account_id, "scope": "identity.admins.manage" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["allowed"], false);
    assert_eq!(body["reason"], "scope_not_held");
}

#[tokio::test]
async fn an_unknown_account_is_a_refusal_with_a_reason() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let token = service_token(&harness).await;

    let (status, body) = authorize(
        &harness,
        &token,
        json!({ "accountId": Uuid::new_v4(), "scope": "geodata.review" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an unknown account is an answer, not a 404: {body}"
    );
    assert_eq!(body["allowed"], false);
    assert_eq!(body["reason"], "account_not_found");
}

#[tokio::test]
async fn a_programme_scoped_grant_answers_only_that_programme() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let token = service_token(&harness).await;

    let programme_a = create_programme(&harness, "a").await;
    let programme_b = create_programme(&harness, "b").await;

    let email = Harness::email("programme-scoped");
    let account_id = harness
        .account_with_role(&email, "an-administrator-passphrase", "support")
        .await;
    harness
        .grant_directly(account_id, "programme_reviewer", Some(programme_a))
        .await;

    let (_, body) = authorize(
        &harness,
        &token,
        json!({ "accountId": account_id, "scope": "geodata.review", "programmeId": programme_a }),
    )
    .await;
    assert_eq!(body["allowed"], true, "{body}");

    let (_, body) = authorize(
        &harness,
        &token,
        json!({ "accountId": account_id, "scope": "geodata.review", "programmeId": programme_b }),
    )
    .await;
    assert_eq!(
        body["allowed"], false,
        "a grant for A must not answer for B: {body}"
    );
}

#[tokio::test]
async fn a_revoked_session_is_refused_even_though_the_account_holds_the_scope() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let token = service_token(&harness).await;

    let email = Harness::email("revoked-session");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;

    let question = json!({
        "accountId": provisioned.account_id,
        "scope": "geodata.review",
        "sessionId": session.session_id,
    });

    let (_, body) = authorize(&harness, &token, question.clone()).await;
    assert_eq!(body["allowed"], true, "{body}");

    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/logout",
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = authorize(&harness, &token, question).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["allowed"], false);
    assert_eq!(body["reason"], "session_revoked");
    assert_eq!(
        body["effectiveScopes"],
        json!([]),
        "a refused decision reports no scopes: {body}"
    );
}

#[tokio::test]
async fn revoking_a_role_takes_effect_on_the_next_decision() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let token = service_token(&harness).await;

    let target_email = Harness::email("revokee");
    let target = harness
        .account_with_role(
            &target_email,
            "an-administrator-passphrase",
            "programme_reviewer",
        )
        .await;
    let grant_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM account_role WHERE account_id = $1 AND role_code = 'programme_reviewer'",
    )
    .bind(target)
    .fetch_one(&harness.pool)
    .await
    .expect("the setup grant");

    let question = json!({ "accountId": target, "scope": "geodata.review" });
    let (_, body) = authorize(&harness, &token, question.clone()).await;
    assert_eq!(body["allowed"], true, "{body}");

    // Revoke as an administrator who is not the target, so the self-lockout guard is not
    // what answers.
    let admin_email = Harness::email("revoking-admin");
    let admin = harness
        .provision(&admin_email, "an-administrator-passphrase")
        .await;
    let admin_session = harness
        .sign_in(&admin_email, "an-administrator-passphrase", &admin.secret)
        .await;
    harness
        .make_fresh(
            &admin_session.cookie,
            "an-administrator-passphrase",
            &admin.secret,
        )
        .await;

    let (status, _, body) = harness
        .send(
            "DELETE",
            &format!("/v1/admin/identity/admins/{target}/roles/{grant_id}"),
            None,
            &[("cookie", admin_session.cookie)],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (_, body) = authorize(&harness, &token, question).await;
    assert_eq!(
        body["allowed"], false,
        "the revocation must be immediate: {body}"
    );
    assert_eq!(body["reason"], "scope_not_held");
}

// ── the admin surface ─────────────────────────────────────────────────────────

#[tokio::test]
async fn granting_a_role_that_carries_a_scope_the_actor_lacks_is_refused() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    // A catalogue role nobody holds. Inserted here rather than seeded, because the point
    // is that a role can exist without any actor being able to confer it.
    sqlx::query(
        "INSERT INTO role_definition (role_code, display_name, scopes)
         VALUES ('site_owner', 'Site owner', ARRAY['root.everything'])
         ON CONFLICT (role_code) DO NOTHING",
    )
    .execute(&harness.pool)
    .await
    .expect("a role nobody holds");

    let actor_email = Harness::email("grantor");
    let actor = harness
        .provision(&actor_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&actor_email, "an-administrator-passphrase", &actor.secret)
        .await;
    harness
        .make_fresh(
            &session.cookie,
            "an-administrator-passphrase",
            &actor.secret,
        )
        .await;

    let target_email = Harness::email("grantee");
    let target = harness
        .account_with_role(&target_email, "an-administrator-passphrase", "support")
        .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/roles"),
            Some(json!({ "roleCode": "site_owner" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#scope-not-held");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("root.everything"),
        "the refusal must name what was missing: {body}"
    );

    // And nothing was granted.
    let granted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account_role WHERE account_id = $1 AND role_code = 'site_owner')",
    )
    .bind(target)
    .fetch_one(&harness.pool)
    .await
    .expect("checking the grant");
    assert!(!granted, "a refused grant must not exist");

    // The attempt is in the audit log, which is the row an escalation leaves.
    let refusals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM security_event WHERE account_id = $1 AND event_type = 'authority.role_grant_refused'",
    )
    .bind(actor.account_id)
    .fetch_one(&harness.pool)
    .await
    .expect("counting audit rows");
    assert_eq!(refusals, 1);
}

#[tokio::test]
async fn an_actor_can_grant_scopes_they_hold_and_the_scopes_endpoint_reports_them() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let actor_email = Harness::email("grant-allowed");
    let actor = harness
        .provision(&actor_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&actor_email, "an-administrator-passphrase", &actor.secret)
        .await;
    harness
        .make_fresh(
            &session.cookie,
            "an-administrator-passphrase",
            &actor.secret,
        )
        .await;

    let target_email = Harness::email("grantee-allowed");
    let target = harness
        .account_with_role(&target_email, "an-administrator-passphrase", "support")
        .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/roles"),
            Some(json!({ "roleCode": "programme_reviewer" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["roleCode"], "programme_reviewer");
    assert!(body["id"].is_string());

    // The target's scopes now include the new role's, alongside the one it already had.
    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins/{target}/scopes"),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let scopes: Vec<&str> = body["scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .map(|scope| scope.as_str().unwrap_or_default())
        .collect();
    assert!(scopes.contains(&"geodata.review"), "{scopes:?}");
    assert!(
        scopes.contains(&"identity.participants.manage"),
        "{scopes:?}"
    );
    assert_eq!(body["roles"].as_array().expect("roles").len(), 2);

    // A second identical grant is a conflict rather than a duplicate row.
    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/roles"),
            Some(json!({ "roleCode": "programme_reviewer" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn the_scopes_endpoint_requires_the_manage_scope() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    // An administrator with a role that carries `geodata.review` only. Provisioning gives
    // them platform_admin, so the grant is replaced with a narrower one.
    let email = Harness::email("narrow-admin");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    sqlx::query("DELETE FROM account_role WHERE account_id = $1")
        .bind(provisioned.account_id)
        .execute(&harness.pool)
        .await
        .expect("narrowing the actor");
    harness
        .grant_directly(provisioned.account_id, "programme_reviewer", None)
        .await;

    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;

    let (status, _, body) = harness
        .send(
            "GET",
            &format!(
                "/v1/admin/identity/admins/{}/scopes",
                provisioned.account_id
            ),
            None,
            &[("cookie", session.cookie)],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#forbidden");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("identity.admins.manage"),
        "{body}"
    );
}

#[tokio::test]
async fn an_actor_cannot_revoke_their_own_last_administrator_role() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let email = Harness::email("last-role");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;
    harness
        .make_fresh(
            &session.cookie,
            "an-administrator-passphrase",
            &provisioned.secret,
        )
        .await;

    let grant_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM account_role WHERE account_id = $1 AND valid_to IS NULL",
    )
    .bind(provisioned.account_id)
    .fetch_one(&harness.pool)
    .await
    .expect("the provisioned grant");

    let (status, _, body) = harness
        .send(
            "DELETE",
            &format!(
                "/v1/admin/identity/admins/{}/roles/{grant_id}",
                provisioned.account_id
            ),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("last administrator role"),
        "{body}"
    );

    // The grant is still open, so the console is not locked out.
    let still_open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account_role WHERE id = $1 AND valid_to IS NULL)",
    )
    .bind(grant_id)
    .fetch_one(&harness.pool)
    .await
    .expect("checking the grant");
    assert!(still_open);
}

#[tokio::test]
async fn a_grant_publishes_the_role_changed_event_with_the_open_sessions() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let actor_email = Harness::email("event-actor");
    let actor = harness
        .provision(&actor_email, "an-administrator-passphrase")
        .await;
    let actor_session = harness
        .sign_in(&actor_email, "an-administrator-passphrase", &actor.secret)
        .await;
    harness
        .make_fresh(
            &actor_session.cookie,
            "an-administrator-passphrase",
            &actor.secret,
        )
        .await;

    // The target has a live session, so the event has something to invalidate.
    let target_email = Harness::email("event-target");
    let target = harness
        .provision(&target_email, "an-administrator-passphrase")
        .await;
    let target_session = harness
        .sign_in(&target_email, "an-administrator-passphrase", &target.secret)
        .await;

    sqlx::query("DELETE FROM account_role WHERE account_id = $1")
        .bind(target.account_id)
        .execute(&harness.pool)
        .await
        .expect("giving the target room for one more role");

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{}/roles", target.account_id),
            Some(json!({ "roleCode": "programme_reviewer" })),
            &[("cookie", actor_session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM ops.outbox_event
          WHERE type = 'core.role.changed.v1'
          ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("the outbox row");

    assert_eq!(payload["type"], "core.role.changed.v1");
    assert_eq!(payload["source"], "/core");
    assert_eq!(payload["data"]["change"], "granted");
    assert_eq!(payload["data"]["roleCode"], "programme_reviewer");
    assert_eq!(
        payload["data"]["accountId"].as_str().unwrap_or_default(),
        target.account_id.to_string()
    );
    let sessions: Vec<&str> = payload["data"]["sessionIds"]
        .as_array()
        .expect("sessionIds")
        .iter()
        .map(|id| id.as_str().unwrap_or_default())
        .collect();
    assert!(
        sessions.contains(&target_session.session_id.to_string().as_str()),
        "the target's open session must be named so its cache can be dropped: {sessions:?}"
    );
}

#[tokio::test]
async fn the_role_catalogue_is_seeded_in_every_environment() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let roles = myota_core::authority::Authority {
        pool: &harness.pool,
    }
    .catalogue()
    .await
    .expect("the catalogue");
    let codes: Vec<&str> = roles.iter().map(|role| role.role_code.as_str()).collect();
    for expected in [
        "platform_admin",
        "programme_admin",
        "programme_reviewer",
        "support",
        "auditor",
    ] {
        assert!(
            codes.contains(&expected),
            "{expected} missing from {codes:?}"
        );
    }

    let platform_admin = roles
        .iter()
        .find(|role| role.role_code == "platform_admin")
        .expect("platform_admin");
    assert!(platform_admin.is_system);
    assert!(platform_admin
        .scopes
        .iter()
        .any(|scope| scope == myota_core::authority::SCOPE_ADMINS_MANAGE));
}

/// Inserts a programme, which grants refer to by id.
async fn create_programme(harness: &Harness, label: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programme (id, slug, name, status)
         VALUES ($1, $2, $3, 'draft')",
    )
    .bind(id)
    .bind(format!("test-{label}-{}", Uuid::new_v4()))
    .bind(format!("Test programme {label}"))
    .execute(&harness.pool)
    .await
    .expect("creating a programme");
    id
}

/// Provisioning grants from the catalogue; it must not define it.
///
/// The first version of `provision-admin` upserted `platform_admin` with its own scope
/// list. Running it once therefore *removed* a scope the baseline had added, and the
/// next administrator provisioned had less authority than the catalogue said. Found on
/// the deployed environment, where the missing scope was `audit.read`.
#[tokio::test]
async fn provisioning_does_not_redefine_the_role_catalogue() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let before: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(scopes) FROM role_definition WHERE role_code = 'platform_admin' ORDER BY 1",
    )
    .fetch_all(&harness.pool)
    .await
    .expect("the seeded scopes");

    let email = Harness::email("provisioned");
    harness
        .provision(&email, "an-administrator-passphrase")
        .await;

    let after: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(scopes) FROM role_definition WHERE role_code = 'platform_admin' ORDER BY 1",
    )
    .fetch_all(&harness.pool)
    .await
    .expect("the scopes after provisioning");

    assert_eq!(
        before, after,
        "provisioning changed the catalogue:\nbefore {before:?}\nafter  {after:?}"
    );
    assert!(
        after.iter().any(|scope| scope == "audit.read"),
        "the baseline seeds audit.read: {after:?}"
    );
}

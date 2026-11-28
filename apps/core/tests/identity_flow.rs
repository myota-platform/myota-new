//! The admins section: list, states, re-authentication, MFA reset, suspension, sessions.
//!
//! The properties here are the ones that make the section safe to exist. A role change
//! without freshness is escalation from a stolen session; a suspension that leaves sessions
//! running is a suspension that takes effect in eight hours; an MFA reset that leaves them
//! running is a reset that does nothing.
//!
//! **They need a database.** See `tests/support/mod.rs`.

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::{bearer, code_now, Harness};
use uuid::Uuid;

/// Re-authenticates a signed-in administrator, returning the freshness grant.
async fn reauth(
    harness: &Harness,
    cookie: &str,
    password: &str,
    secret: &str,
) -> (StatusCode, Value) {
    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/reauth",
            Some(json!({ "password": password, "code": code_now(secret) })),
            &[("cookie", cookie.to_string())],
        )
        .await;
    (status, body)
}

async fn list(harness: &Harness, cookie: &str, query: &str) -> (StatusCode, Value) {
    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins{query}"),
            None,
            &[("cookie", cookie.to_string())],
        )
        .await;
    (status, body)
}

#[tokio::test]
async fn the_list_shows_administrators_with_their_mfa_and_state() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("list-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;

    // An administrator with no confirmed factor: created raw, so `blocked`.
    let blocked_email = Harness::email("list-blocked");
    let blocked = harness
        .account_with_role(
            &blocked_email,
            "an-administrator-passphrase",
            "platform_admin",
        )
        .await;

    // Filtered by the unique address rather than paging the whole table: the suite shares
    // one database, and "is this account in the first hundred" is a property of the test
    // run's history, not of the endpoint.
    let (status, body) = list(
        &harness,
        &session.cookie,
        &format!("?limit=100&q={blocked_email}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "one address, one row: {body}");

    let blocked_row = &items[0];
    assert_eq!(blocked_row["id"], blocked.to_string());
    assert_eq!(blocked_row["mfa"], "not_enrolled");
    assert_eq!(blocked_row["state"], "blocked");
    assert_eq!(blocked_row["roles"][0]["roleCode"], "platform_admin");

    let (_, body) = list(
        &harness,
        &session.cookie,
        &format!("?limit=100&q={acting_email}"),
    )
    .await;
    let acting_row = &body["items"].as_array().expect("items")[0];
    assert_eq!(acting_row["mfa"], "enrolled");
    assert!(
        acting_row["state"] == "active" || acting_row["state"] == "idle",
        "state was {}",
        acting_row["state"]
    );

    // A participant with no role is not an administrator and must not appear.
    let participant_email = Harness::email("list-participant");
    harness
        .account_without_role(&participant_email, "a-participant-passphrase")
        .await;
    let (_, body) = list(
        &harness,
        &session.cookie,
        &format!("?limit=100&q={participant_email}"),
    )
    .await;
    assert_eq!(
        body["items"].as_array().expect("items").len(),
        0,
        "a non-administrator appeared in the admins list: {body}"
    );
}

/// The page reports where the next one starts, and a cursor that means nothing is ignored
/// rather than refused: a stale link should show the first page, not an error.
#[tokio::test]
async fn the_list_pages_and_tolerates_a_stale_cursor() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let acting_email = Harness::email("page-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;

    for index in 0..3 {
        let email = Harness::email(&format!("page-target-{index}"));
        harness
            .account_with_role(&email, "an-administrator-passphrase", "support")
            .await;
    }

    let (_, first) = list(&harness, &session.cookie, "?limit=1").await;
    assert_eq!(first["items"].as_array().expect("items").len(), 1);
    let cursor = first["nextCursor"]
        .as_str()
        .expect("a next cursor")
        .to_string();

    let (status, second) = list(
        &harness,
        &session.cookie,
        &format!("?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_ne!(
        first["items"][0]["id"], second["items"][0]["id"],
        "the second page repeated the first row"
    );

    let (status, nonsense) = list(&harness, &session.cookie, "?limit=1&cursor=not-a-number").await;
    assert_eq!(status, StatusCode::OK, "{nonsense}");
    assert_eq!(nonsense["items"].as_array().expect("items").len(), 1);
}

#[tokio::test]
async fn the_list_filters_by_mfa_state_and_role() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let acting_email = Harness::email("filter-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;

    let blocked_email = Harness::email("filter-blocked");
    harness
        .account_with_role(&blocked_email, "an-administrator-passphrase", "support")
        .await;

    let (_, body) = list(&harness, &session.cookie, "?mfa=not_enrolled&limit=100").await;
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty());
    assert!(
        items.iter().all(|item| item["mfa"] == "not_enrolled"),
        "the mfa filter let an enrolled account through: {body}"
    );

    let (_, body) = list(&harness, &session.cookie, "?role=support&limit=100").await;
    let items = body["items"].as_array().expect("items");
    assert!(items.iter().all(|item| item["roles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["roleCode"] == "support")));
}

#[tokio::test]
async fn a_role_grant_without_recent_re_authentication_is_refused() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("fresh-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;

    let target_email = Harness::email("fresh-target");
    let target = harness
        .account_with_role(&target_email, "an-administrator-passphrase", "support")
        .await;

    let grant = json!({ "roleCode": "programme_reviewer" });

    // No freshness yet: the session is valid and holds the scope, and is refused anyway.
    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/roles"),
            Some(grant.clone()),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#reauth-required");

    // Re-authenticate, and the same request succeeds.
    let (status, body) = reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &acting.secret,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["freshUntil"].is_string());

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/roles"),
            Some(grant),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn re_authentication_needs_both_factors() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("reauth-both");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;

    let (status, body) = reauth(
        &harness,
        &session.cookie,
        "the-wrong-password",
        &provisioned.secret,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _, body) = harness
        .send(
            "POST",
            "/v1/admin/auth/reauth",
            Some(json!({ "password": "an-administrator-passphrase", "code": "000000" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["type"], "about:blank#mfa-failed");
}

#[tokio::test]
async fn resetting_a_second_factor_revokes_the_account_s_sessions() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("reset-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;
    reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &acting.secret,
    )
    .await;

    let target_email = Harness::email("reset-target");
    let target = harness
        .provision(&target_email, "an-administrator-passphrase")
        .await;
    let target_session = harness
        .sign_in(&target_email, "an-administrator-passphrase", &target.secret)
        .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{}/mfa/reset", target.account_id),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // The factor is gone and the recovery codes with it.
    let factors: i64 = sqlx::query_scalar("SELECT count(*) FROM account_mfa WHERE account_id = $1")
        .bind(target.account_id)
        .fetch_one(&harness.pool)
        .await
        .expect("counting factors");
    assert_eq!(factors, 0);
    let codes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_recovery_code WHERE account_id = $1")
            .bind(target.account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("counting codes");
    assert_eq!(codes, 0);

    // And the target's session is dead.
    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", target_session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Sign-in now refuses with `#mfa-not-enrolled`, which is the state the console shows.
    let (status, _, body) = harness
        .login(&target_email, "an-administrator-passphrase")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "about:blank#mfa-not-enrolled");
}

#[tokio::test]
async fn suspension_blocks_sign_in_and_revokes_sessions() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("suspend-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;
    reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &acting.secret,
    )
    .await;

    let target_email = Harness::email("suspend-target");
    let target = harness
        .provision(&target_email, "an-administrator-passphrase")
        .await;
    let target_session = harness
        .sign_in(&target_email, "an-administrator-passphrase", &target.secret)
        .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{}/suspend", target.account_id),
            Some(json!({ "reason": "credential suspected" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "suspended");

    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", target_session.cookie)],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the suspended session survived"
    );

    let (status, _, body) = harness
        .login(&target_email, "an-administrator-passphrase")
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // Reactivate: the account can sign in again.
    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{}/reactivate", target.account_id),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["state"], "suspended");

    let (status, _, body) = harness
        .login(&target_email, "an-administrator-passphrase")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "mfa_required");
}

#[tokio::test]
async fn an_administrator_cannot_suspend_themselves() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("self-suspend");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;
    reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &provisioned.secret,
    )
    .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!(
                "/v1/admin/identity/admins/{}/suspend",
                provisioned.account_id
            ),
            Some(json!({})),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn an_administrator_can_revoke_a_session_and_notices_it_is_gone() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("session-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;

    let target_email = Harness::email("session-target");
    let target = harness
        .provision(&target_email, "an-administrator-passphrase")
        .await;
    let target_session = harness
        .sign_in(&target_email, "an-administrator-passphrase", &target.secret)
        .await;

    // The list marks which is the caller's own.
    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins/{}/sessions", target.account_id),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("sessions");
    assert_eq!(items.len(), 1, "{body}");
    assert_eq!(items[0]["current"], false);
    assert!(items[0]["userAgent"].is_string() || items[0]["userAgent"].is_null());

    let (status, _, body) = harness
        .send(
            "DELETE",
            &format!(
                "/v1/admin/identity/admins/{}/sessions/{}",
                target.account_id, target_session.session_id
            ),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _, _) = harness
        .send(
            "POST",
            "/v1/admin/auth/refresh",
            None,
            &[("cookie", target_session.cookie)],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reading_one_administrator_shows_the_state_and_the_grants() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("read-one");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;

    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins/{}", provisioned.account_id),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mfa"], "enrolled");
    assert_eq!(body["roles"][0]["roleCode"], "platform_admin");
    assert!(body["state"].is_string());

    // An account with no administrator role is a 404 here, not a participant record.
    let participant_email = Harness::email("read-participant");
    let participant = harness
        .account_without_role(&participant_email, "a-participant-passphrase")
        .await;
    let (status, _, _) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins/{participant}"),
            None,
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_section_requires_the_manage_scope_even_for_a_list() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    // An administrator whose only role carries `geodata.review`.
    let email = Harness::email("narrow-list");
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

    let (status, body) = list(&harness, &session.cookie, "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// A session token alone is enough to read the section, and not enough to change it without
/// a fresh proof. This is the pair the console's re-auth prompt exists for.
#[tokio::test]
async fn a_bearer_token_is_accepted_for_reads() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("bearer-read");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;

    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/admin/identity/admins/{}", provisioned.account_id),
            None,
            &[bearer(&session.token)],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The audit trail names the actor for every change, which is what makes the section
/// accountable rather than merely available.
#[tokio::test]
async fn every_change_is_audited_with_its_actor() {
    let Some(harness) = Harness::new().await else {
        return;
    };

    let acting_email = Harness::email("audit-actor");
    let acting = harness
        .provision(&acting_email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&acting_email, "an-administrator-passphrase", &acting.secret)
        .await;
    reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &acting.secret,
    )
    .await;

    let target_email = Harness::email("audit-target");
    let target = harness
        .account_with_role(&target_email, "an-administrator-passphrase", "support")
        .await;

    harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{target}/suspend"),
            Some(json!({ "reason": "audit probe" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;

    let (event_type, payload): (String, Value) = sqlx::query_as(
        "SELECT event_type, payload FROM security_event
          WHERE account_id = $1 AND event_type = 'authority.account_suspended'
          ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind(target)
    .fetch_one(&harness.pool)
    .await
    .expect("the suspension event");

    assert_eq!(event_type, "authority.account_suspended");
    assert_eq!(
        payload["actorId"].as_str().unwrap_or_default(),
        acting.account_id.to_string()
    );
    assert_eq!(payload["reason"], "audit probe");
}

/// A grant to an account that does not exist is a 404, not a 500 and not a silent success.
#[tokio::test]
async fn granting_to_an_unknown_account_is_a_not_found() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let email = Harness::email("grant-unknown");
    let provisioned = harness
        .provision(&email, "an-administrator-passphrase")
        .await;
    let session = harness
        .sign_in(&email, "an-administrator-passphrase", &provisioned.secret)
        .await;
    reauth(
        &harness,
        &session.cookie,
        "an-administrator-passphrase",
        &provisioned.secret,
    )
    .await;

    let (status, _, body) = harness
        .send(
            "POST",
            &format!("/v1/admin/identity/admins/{}/roles", Uuid::new_v4()),
            Some(json!({ "roleCode": "programme_reviewer" })),
            &[("cookie", session.cookie.clone())],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

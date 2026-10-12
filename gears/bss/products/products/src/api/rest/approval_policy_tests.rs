#![allow(clippy::expect_used, clippy::unwrap_used)]
//! P-D-205: the approval policy is read with a content `ETag` and written under `If-Match`.
use super::router;
use crate::test_support::{
    body_json, denying_enforcer, get, problem_code, raw_i64, request, resolved_usage_types,
    rest_app, rest_app_on_db, test_db, violation_for,
};
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

const POLICY: &str = "/bss-products/v1/approval-policy";

async fn read(app: &axum::Router, tenant: Uuid) -> (String, Value) {
    let r = get(app, tenant, POLICY).await;
    assert_eq!(r.status(), StatusCode::OK);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    (tag, body_json(r).await)
}

async fn put(
    app: &axum::Router,
    tenant: Uuid,
    body: Value,
    tag: Option<&str>,
) -> axum::response::Response {
    request(app, tenant, Method::PUT, POLICY, Some(body), tag).await
}

/// The GET answers a strong decimal tag over the policy's content; the PUT refuses a missing or
/// malformed `If-Match` (400) and a stale one (409 `STALE_REVISION`), and answers the new tag.
#[tokio::test]
async fn the_policy_write_asserts_the_policy_it_read() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (tag, before) = read(&app, tenant).await;
    let body = tag
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .expect("a strong tag is quoted");
    assert!(
        !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()),
        "{tag}"
    );
    assert_eq!(
        read(&app, tenant).await.0,
        tag,
        "an unchanged policy keeps its tag"
    );

    let r = put(&app, tenant, json!({"quorum":0}), None).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(violation_for(&body_json(r).await, "If-Match").is_some());
    for malformed in ["*", "W/\"1\"", "1", "\"abc\"", "\"1\", \"2\""] {
        let r = put(&app, tenant, json!({"quorum":0}), Some(malformed)).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{malformed}");
        assert!(
            violation_for(&body_json(r).await, "If-Match").is_some(),
            "{malformed}"
        );
    }
    let r = put(&app, tenant, json!({"quorum":0}), Some("\"7\"")).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");
    assert_eq!(read(&app, tenant).await, (tag.clone(), before.clone()));
    assert_eq!(
        raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        0,
        "no refused write is audited"
    );

    let r = put(&app, tenant, json!({"quorum":0}), Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    let written = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_ne!(written, tag);
    assert_eq!(body_json(r).await["default_quorum"], 0);
    assert_eq!(
        read(&app, tenant).await.0,
        written,
        "the write answers the tag a read returns"
    );

    // The tag read before the write is stale now, for any kind: a lost update is refused.
    let r = put(
        &app,
        tenant,
        json!({"kind":"sku_publish","quorum":2}),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");
    let r = put(
        &app,
        tenant,
        json!({"kind":"sku_publish","quorum":2}),
        Some(&written),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let (last, policy) = read(&app, tenant).await;
    assert_ne!(last, written, "an override moves the tag too");
    assert_eq!(policy["overrides"]["sku_publish"], 2);
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'approval_policy.write'"
        )
        .await,
        2
    );
}

/// Two writers holding the same tag: exactly one wins, the other is told its read is stale.
#[tokio::test]
async fn two_writers_holding_one_tag_do_not_both_win() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, router).await;
    let (tag, _) = read(&app, tenant).await;
    let (a, b) = tokio::join!(
        put(&app, tenant, json!({"quorum":0}), Some(&tag)),
        put(
            &app,
            tenant,
            json!({"kind":"sku_retire","quorum":3}),
            Some(&tag)
        ),
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
}

/// Authorization is judged before the precondition: a caller without the settings grant is 403
/// on a PUT that also lacks `If-Match` and carries a malformed body.
#[tokio::test]
async fn authorization_is_judged_before_the_precondition() {
    let tenant = Uuid::new_v4();
    let (db, _, _, _dsn) = test_db().await;
    let (_, state) = rest_app_on_db(tenant, router, resolved_usage_types(), "test", db).await;
    let denied = router(state, &toolkit::api::OpenApiRegistryImpl::new())
        .layer(axum::Extension(denying_enforcer()));
    let r = put(&denied, tenant, json!({"quorum":"many"}), None).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = get(&denied, tenant, POLICY).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

async fn delete(
    app: &axum::Router,
    tenant: Uuid,
    kind: &str,
    tag: Option<&str>,
) -> axum::response::Response {
    request(
        app,
        tenant,
        Method::DELETE,
        &format!("{POLICY}/{kind}"),
        None,
        tag,
    )
    .await
}

/// P-D-216 (the twin of pricing's D-435): `DELETE /approval-policy/{kind}` removes a kind's
/// override so the kind follows the default again, at the policy the caller read (If-Match); the
/// default cannot be deleted (400 `POLICY_DEFAULT_REQUIRED`); a kind without an override is 404.
#[tokio::test]
async fn an_override_is_reset_to_the_default_under_if_match() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (tag, _) = read(&app, tenant).await;
    let r = put(
        &app,
        tenant,
        json!({"kind":"sku_retire","quorum":3}),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let (tag, policy) = read(&app, tenant).await;
    assert_eq!(policy["overrides"]["sku_retire"], 3);
    // If-Match is required.
    let r = delete(&app, tenant, "sku_retire", None).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(violation_for(&body_json(r).await, "If-Match").is_some());
    // Probed in run 6.4: the default is never deleted, spelled plain or encoded.
    for default in ["*", "%2A"] {
        let r = delete(&app, tenant, default, Some(&tag)).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{default}");
        assert_eq!(problem_code(&body_json(r).await), "POLICY_DEFAULT_REQUIRED");
    }
    let r = delete(&app, tenant, "sku_teleport", Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    // A kind without an override.
    let r = delete(&app, tenant, "sku_publish", Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    // A stale tag changes nothing.
    let r = delete(&app, tenant, "sku_retire", Some("\"7\"")).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");
    assert_eq!(read(&app, tenant).await, (tag.clone(), policy));
    // The reset answers the policy the kind now follows, with its new tag.
    let r = delete(&app, tenant, "sku_retire", Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    let reset_tag = r.headers()["etag"].to_str().unwrap().to_owned();
    let body = body_json(r).await;
    assert_eq!(body["overrides"], json!({}));
    assert_eq!(read(&app, tenant).await, (reset_tag.clone(), body));
    // Twice: the old tag is stale, the new one finds nothing to reset.
    assert_eq!(
        delete(&app, tenant, "sku_retire", Some(&tag))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        delete(&app, tenant, "sku_retire", Some(&reset_tag))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'approval_policy.reset'"
        )
        .await,
        1,
        "one reset, one audit row"
    );
}

/// Authorization is judged before the precondition on the reset too.
#[tokio::test]
async fn the_reset_is_authorized_before_its_precondition() {
    let tenant = Uuid::new_v4();
    let (db, _, _, _dsn) = test_db().await;
    let (_, state) = rest_app_on_db(tenant, router, resolved_usage_types(), "test", db).await;
    let denied = router(state, &toolkit::api::OpenApiRegistryImpl::new())
        .layer(axum::Extension(denying_enforcer()));
    let r = delete(&denied, tenant, "*", None).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

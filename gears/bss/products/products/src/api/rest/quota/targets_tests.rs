//! The adapter target endpoints (P-D-268, P-D-273): a target is bound once per tenant, plugin
//! instance and key, read and listed in the caller's tenant only, and every request authorizes
//! before it reads the body and asks the PDP for the caller's tenant only.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::api::rest::quota::CANDIDATES;
use crate::api::rest::quota::TARGETS;
use crate::authz::labels;
use crate::quota_test_support::{
    LogCapture, QuotaFakes, TEST_PLUGIN, VCPU_UNIT, quota_app, quota_app_for, refusal, send,
    vcpu_record,
};
use crate::test_support::{
    body_json, get, post, problem_code, request, tenant_user, violation_for,
};
use authz_resolver_sdk::models::TenantMode;
use axum::http::Method;
use serde_json::json;
use uuid::Uuid;

fn target(key: &str, name: &str) -> serde_json::Value {
    json!({"plugin_instance_id": TEST_PLUGIN, "target_key": key, "name": name})
}

#[tokio::test]
async fn a_target_is_created_read_and_listed() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let response = post(
        &q.app,
        tenant,
        TARGETS,
        target("region-1", "Region 1 compute"),
    )
    .await;
    assert_eq!(response.status(), 201);
    let created = body_json(response).await;
    assert_eq!(created["plugin_instance_id"], TEST_PLUGIN);
    assert_eq!(
        created["created_by"],
        json!(tenant_user(tenant).subject_id())
    );
    let id = created["id"].as_str().unwrap();
    let read = body_json(get(&q.app, tenant, &format!("{TARGETS}/{id}")).await).await;
    for member in [
        "id",
        "plugin_instance_id",
        "target_key",
        "name",
        "created_by",
        "created_at",
    ] {
        assert_eq!(read[member], created[member], "{member}");
    }
    let page = body_json(get(&q.app, tenant, TARGETS).await).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["id"], created["id"]);
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_adapter_target.create'")
            .await,
        1
    );
}

#[tokio::test]
async fn a_plugin_instance_of_another_type_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    for plugin in [
        VCPU_UNIT,
        "gts.cf.toolkit.plugins.plugin.v1~cf.bss.products.quota_discovery.v1~\
         cf.compute.quota_adapter.nothing.v1",
    ] {
        let response = post(
            &q.app,
            tenant,
            TARGETS,
            json!({"plugin_instance_id": plugin, "target_key": "k", "name": "n"}),
        )
        .await;
        assert_eq!(response.status(), 400, "{plugin}");
        let body = body_json(response).await;
        assert_eq!(problem_code(&body), "QUOTA_TARGET_PLUGIN_UNKNOWN");
        assert!(
            violation_for(&body, "plugin_instance_id").is_some(),
            "{body}"
        );
    }
    assert_eq!(q.count("products_quota_adapter_target").await, 0);
}

#[tokio::test]
async fn the_same_binding_twice_is_taken() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let first = post(&q.app, tenant, TARGETS, target("region-1", "One")).await;
    assert_eq!(first.status(), 201);
    let again = post(&q.app, tenant, TARGETS, target("region-1", "Other name")).await;
    assert_eq!(again.status(), 409);
    let body = body_json(again).await;
    assert_eq!(problem_code(&body), "QUOTA_TARGET_TAKEN");
    assert_eq!(
        body["context"]["resource_type"],
        labels::QUOTA_ADAPTER_TARGET,
        "{body}"
    );
    assert_eq!(q.count("products_quota_adapter_target").await, 1);
}

#[tokio::test]
async fn another_tenant_cannot_read_or_discover_a_target() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let id = q.create_target(tenant).await;
    assert_eq!(
        get(&q.app, other, &format!("{TARGETS}/{id}"))
            .await
            .status(),
        404
    );
    assert_eq!(
        post(
            &q.app,
            other,
            &format!("{TARGETS}/{id}/discoveries"),
            json!({})
        )
        .await
        .status(),
        404
    );
    assert!(
        body_json(get(&q.app, other, TARGETS).await).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_denied_caller_is_403_before_the_body_is_read() {
    use tower::ServiceExt;
    let tenant = Uuid::new_v4();
    let q = quota_app_for(&[], QuotaFakes::default()).await;
    let response = q
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri(TARGETS)
                .extension(tenant_user(tenant))
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from("not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let body = body_json(response).await;
    assert_eq!(
        body["context"]["resource_type"],
        labels::QUOTA_ADAPTER_TARGET,
        "{body}"
    );
    assert_eq!(
        q.registry.calls(),
        0,
        "nothing is resolved for a denied caller"
    );
    assert_eq!(get(&q.app, tenant, TARGETS).await.status(), 403);
}

#[tokio::test]
async fn every_quota_request_asks_for_the_callers_tenant_only() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    assert_eq!(get(&q.app, tenant, TARGETS).await.status(), 200);
    let created = post(&q.app, tenant, TARGETS, target("region-1", "One")).await;
    assert_eq!(created.status(), 201);
    let target_id = body_json(created).await["id"].clone();
    let discovery = post(
        &q.app,
        tenant,
        &format!("{TARGETS}/{}/discoveries", target_id.as_str().unwrap()),
        json!({}),
    )
    .await;
    assert_eq!(discovery.status(), 501, "no adapter: closed");
    let manual = post(
        &q.app,
        tenant,
        CANDIDATES,
        json!({"target_id": target_id, "record": vcpu_record()}),
    )
    .await;
    assert_eq!(manual.status(), 201);
    let candidate = body_json(manual).await["id"].clone();
    assert_eq!(get(&q.app, tenant, CANDIDATES).await.status(), 200);
    let one = format!("{CANDIDATES}/{}", candidate.as_str().unwrap());
    assert_eq!(get(&q.app, tenant, &one).await.status(), 200);
    let asked = |resource: &str, action: &str| {
        (
            resource.to_owned(),
            action.to_owned(),
            Some(TenantMode::RootOnly),
        )
    };
    let (target, definition) = (labels::QUOTA_ADAPTER_TARGET, labels::QUOTA_DEFINITION);
    assert_eq!(
        q.pdp.asked(),
        vec![
            asked(target, "read"),
            asked(target, "author"),
            asked(target, "author"),
            asked(definition, "author"),
            asked(definition, "read"),
            asked(definition, "read"),
        ]
    );
}

/// Every refusal of a target endpoint names the adapter target, whatever its code (P-D-273): a
/// blank name, a body that does not read, a key the header cannot carry and another body under a
/// used key.
#[tokio::test]
async fn a_target_refusal_names_the_adapter_target() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let user = tenant_user(tenant);
    let body = |name: &str| Some(target("region-1", name).to_string());
    let first = send(
        &q.app,
        &user,
        Method::POST,
        TARGETS,
        (Some("k-1"), None),
        body("One"),
    )
    .await;
    assert_eq!(first.status(), 201);
    for (response, expected) in [
        (
            send(
                &q.app,
                &user,
                Method::POST,
                TARGETS,
                (None, None),
                body(" "),
            )
            .await,
            (400, "VALIDATION"),
        ),
        (
            send(
                &q.app,
                &user,
                Method::POST,
                TARGETS,
                (None, None),
                Some("not json".to_owned()),
            )
            .await,
            (400, "VALIDATION"),
        ),
        (
            send(
                &q.app,
                &user,
                Method::POST,
                TARGETS,
                (Some(" "), None),
                body("One"),
            )
            .await,
            (400, "VALIDATION"),
        ),
        (
            send(
                &q.app,
                &user,
                Method::POST,
                TARGETS,
                (Some("k-1"), None),
                body("Two"),
            )
            .await,
            (409, "IDEMPOTENCY_CONFLICT"),
        ),
    ] {
        let (status, code, resource) = refusal(response).await;
        assert_eq!((status, code.as_str()), expected);
        assert_eq!(resource, labels::QUOTA_ADAPTER_TARGET, "{expected:?}");
    }
}

/// The target list's cursor binds the caller's tenant: the tenant that cut it walks on, and
/// another tenant's reuse of it is 400.
#[tokio::test]
async fn a_target_list_cursor_from_another_tenant_is_refused() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    for (key, name) in [("a", "Alpha"), ("b", "Beta"), ("c", "Gamma")] {
        let response = post(&q.app, tenant, TARGETS, target(key, name)).await;
        assert_eq!(response.status(), 201);
    }
    let first = body_json(get(&q.app, tenant, &format!("{TARGETS}?limit=2")).await).await;
    let names: Vec<_> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["Alpha", "Beta"]);
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    let next = get(&q.app, tenant, &format!("{TARGETS}?cursor={cursor}")).await;
    assert_eq!(next.status(), 200);
    assert_eq!(body_json(next).await["items"][0]["name"], "Gamma");
    let foreign = get(&q.app, other, &format!("{TARGETS}?cursor={cursor}")).await;
    assert_eq!(foreign.status(), 400);
}

/// A name of 1 to 200 characters and a key of 1 to 255 printable ASCII bytes (P-D-225): a text
/// over its cap is `FIELD_TOO_LONG`, an empty or unprintable one `VALIDATION`, and nothing is
/// written.
#[tokio::test]
async fn a_target_outside_its_caps_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    for (body, field, code) in [
        (target("k", &"n".repeat(201)), "name", "FIELD_TOO_LONG"),
        (
            target(&"k".repeat(256), "n"),
            "target_key",
            "FIELD_TOO_LONG",
        ),
        (target("k", " "), "name", "VALIDATION"),
        (target("", "n"), "target_key", "VALIDATION"),
        (target("region\u{7}", "n"), "target_key", "VALIDATION"),
        (target("r\u{e9}gion", "n"), "target_key", "VALIDATION"),
    ] {
        let response = post(&q.app, tenant, TARGETS, body.clone()).await;
        assert_eq!(response.status(), 400, "{body}");
        let answer = body_json(response).await;
        assert_eq!(problem_code(&answer), code, "{body}: {answer}");
        assert!(violation_for(&answer, field).is_some(), "{body}: {answer}");
    }
    let response = post(&q.app, tenant, TARGETS, target("k", &"n".repeat(200))).await;
    assert_eq!(response.status(), 201);
    assert_eq!(q.count("products_quota_adapter_target").await, 1);
}

/// A registry that cannot answer leaves the target unbound: 503 `QUOTA_REGISTRY_UNAVAILABLE`.
#[tokio::test]
async fn a_registry_outage_binds_no_target() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    q.registry.go_down();
    let (logs, _guard) = LogCapture::install();
    let response = post(&q.app, tenant, TARGETS, target("region-1", "One")).await;
    assert_eq!(response.status(), 503);
    let body = body_json(response).await;
    assert!(
        body.to_string().contains("QUOTA_REGISTRY_UNAVAILABLE"),
        "{body}"
    );
    // The registry's own text is logged, never answered.
    assert!(
        !body.to_string().contains("the fake registry is down"),
        "{body}"
    );
    assert!(
        logs.lines()
            .iter()
            .any(|line| line.contains("the fake registry is down")),
        "{:?}",
        logs.lines()
    );
    assert_eq!(q.count("products_quota_adapter_target").await, 0);
}

/// A keyed create answers its first answer again, even after the binding exists, and writes once.
#[tokio::test]
async fn a_keyed_target_create_replays_its_first_answer() {
    use tower::ServiceExt;
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let keyed = || {
        axum::http::Request::builder()
            .method(Method::POST)
            .uri(TARGETS)
            .extension(tenant_user(tenant))
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", "bind-region-1")
            .body(axum::body::Body::from(
                target("region-1", "One").to_string(),
            ))
            .unwrap()
    };
    let first = q.app.clone().oneshot(keyed()).await.unwrap();
    assert_eq!(first.status(), 201);
    let first = body_json(first).await;
    let again = q.app.clone().oneshot(keyed()).await.unwrap();
    assert_eq!(again.status(), 201);
    assert_eq!(body_json(again).await, first);
    assert_eq!(q.count("products_quota_adapter_target").await, 1);
    let unkeyed = request(
        &q.app,
        tenant,
        Method::POST,
        TARGETS,
        Some(target("region-1", "One")),
        None,
    )
    .await;
    assert_eq!(unkeyed.status(), 409);
}

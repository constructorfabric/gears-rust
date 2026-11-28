//! Manual records and candidate reads (P-D-268, P-D-269): a manual record passes the discovery
//! judgement and keeps its author, the same record again answers the stored candidate, other
//! content under its id conflicts, a keyed create replays its first answer, and the reads stay in
//! the caller's tenant with a cursor bound to the tenant and the filters.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::api::rest::quota::{CANDIDATES, TARGETS};
use crate::authz::labels;
use crate::quota_test_support::{
    LogCapture, QuotaApp, QuotaFakes, TEST_PLUGIN, quota_app, quota_app_for, refusal, send,
    submit_body, vcpu_record, vcpu_record_numbered,
};
use crate::test_support::{
    authed_ctx, body_json, get, post, problem_code, request_as, tenant_user, violation_for,
};
use axum::http::{Method, StatusCode};
use sea_orm::{ConnectionTrait, Database};
use serde_json::{Value, json};
use uuid::Uuid;

/// A keyed manual record create.
async fn keyed(q: &QuotaApp, tenant: Uuid, key: &str, body: &Value) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let response = q
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri(CANDIDATES)
                .extension(tenant_user(tenant))
                .header("Content-Type", "application/json")
                .header("Idempotency-Key", key)
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

/// The ids of a candidate page, in order.
fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A second target of `tenant`, bound under another key.
async fn second_target(q: &QuotaApp, tenant: Uuid) -> Uuid {
    let response = post(
        &q.app,
        tenant,
        TARGETS,
        json!({"plugin_instance_id": TEST_PLUGIN, "target_key": "region-2", "name": "Region 2"}),
    )
    .await;
    assert_eq!(response.status(), 201);
    let body = body_json(response).await;
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn a_manual_record_passes_the_discovery_judgement() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let body = json!({"target_id": target, "record": vcpu_record()});
    let first = post(&q.app, tenant, CANDIDATES, body.clone()).await;
    assert_eq!(first.status(), 201);
    let etag = first.headers().get("etag").cloned();
    let created = body_json(first).await;
    assert_eq!(created["origin"], "manual");
    assert_eq!(created["state"], "staged");
    // The stored snapshot row a discovered candidate came from: none for a manual one.
    assert_eq!(created["snapshot_row_id"], Value::Null);
    assert!(created.get("snapshot_id").is_none(), "{created}");
    assert_eq!(
        created["created_by"],
        json!(tenant_user(tenant).subject_id())
    );
    assert!(etag.is_some(), "a created candidate answers its ETag");
    let again = post(&q.app, tenant, CANDIDATES, body).await;
    assert_eq!(again.status(), 200);
    assert_eq!(body_json(again).await["id"], created["id"]);
    assert_eq!(q.count("products_quota_candidate").await, 1);
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.create'")
            .await,
        1
    );
}

#[tokio::test]
async fn a_manual_record_with_an_unknown_unit_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let mut record = vcpu_record();
    record["unit_ref"] =
        json!("gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.nothing.v1");
    let response = post(
        &q.app,
        tenant,
        CANDIDATES,
        json!({"target_id": target, "record": record}),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_REFERENCE_UNKNOWN");
    assert!(violation_for(&body, "record.unit_ref").is_some(), "{body}");
    assert_eq!(
        body["context"]["resource_type"],
        labels::QUOTA_DEFINITION,
        "{body}"
    );
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

#[tokio::test]
async fn a_manual_record_with_the_same_id_and_other_content_conflicts() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let first = post(
        &q.app,
        tenant,
        CANDIDATES,
        json!({"target_id": target, "record": vcpu_record()}),
    )
    .await;
    assert_eq!(first.status(), 201);
    let mut changed = vcpu_record();
    changed["limits"]["max"] = json!("2048");
    let response = post(
        &q.app,
        tenant,
        CANDIDATES,
        json!({"target_id": target, "record": changed}),
    )
    .await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        problem_code(&body_json(response).await),
        "QUOTA_SOURCE_CONFLICT"
    );
    assert_eq!(q.count("products_quota_candidate").await, 1);
}

/// A keyed create answers its first answer again, 201 and the same body; the same record under
/// another key is the stored candidate's 200.
#[tokio::test]
async fn a_keyed_manual_record_replays_its_first_answer() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let body = json!({"target_id": target, "record": vcpu_record()});
    let (status, first) = keyed(&q, tenant, "manual-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let calls = q.registry.calls();
    let (status, again) = keyed(&q, tenant, "manual-1", &body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again, first);
    assert_eq!(q.registry.calls(), calls, "a keyed replay judges nothing");
    let (status, other) = keyed(&q, tenant, "manual-2", &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(other["id"], first["id"]);
    assert_eq!(q.count("products_quota_candidate").await, 1);
}

#[tokio::test]
async fn the_candidate_list_filters_by_state_and_target() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (one, two) = (
        q.create_target(tenant).await,
        second_target(&q, tenant).await,
    );
    let mut created = Vec::new();
    for (target, i) in [(one, 1), (two, 2), (one, 3)] {
        let response = post(
            &q.app,
            tenant,
            CANDIDATES,
            json!({"target_id": target, "record": vcpu_record_numbered(i)}),
        )
        .await;
        assert_eq!(response.status(), 201);
        created.push(body_json(response).await["id"].as_str().unwrap().to_owned());
    }
    let all = body_json(get(&q.app, tenant, CANDIDATES).await).await;
    assert_eq!(
        ids(&all),
        vec![created[2].clone(), created[1].clone(), created[0].clone()],
        "newest first"
    );
    let of_one =
        body_json(get(&q.app, tenant, &format!("{CANDIDATES}?target_id={one}")).await).await;
    assert_eq!(ids(&of_one), vec![created[2].clone(), created[0].clone()]);
    let staged = body_json(get(&q.app, tenant, &format!("{CANDIDATES}?state=staged")).await).await;
    assert_eq!(ids(&staged).len(), 3);
    let approved =
        body_json(get(&q.app, tenant, &format!("{CANDIDATES}?state=approved")).await).await;
    assert!(ids(&approved).is_empty());
    for bad in [
        "state=done",
        "target_id=not-a-uuid",
        "%24filter=state%20eq%20%27staged%27",
    ] {
        let response = get(&q.app, tenant, &format!("{CANDIDATES}?{bad}")).await;
        assert_eq!(response.status(), 400, "{bad}");
    }
    let response = get(&q.app, tenant, &format!("{CANDIDATES}?state=done")).await;
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "VALIDATION");
    assert!(violation_for(&body, "state").is_some(), "{body}");
}

#[tokio::test]
async fn a_candidate_list_cursor_with_another_state_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    for i in 1..=3 {
        let response = post(
            &q.app,
            tenant,
            CANDIDATES,
            json!({"target_id": target, "record": vcpu_record_numbered(i)}),
        )
        .await;
        assert_eq!(response.status(), 201);
    }
    let first = body_json(
        get(
            &q.app,
            tenant,
            &format!("{CANDIDATES}?state=staged&limit=2"),
        )
        .await,
    )
    .await;
    assert_eq!(ids(&first).len(), 2);
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    let next = get(
        &q.app,
        tenant,
        &format!("{CANDIDATES}?state=staged&cursor={cursor}"),
    )
    .await;
    assert_eq!(next.status(), 200);
    assert_eq!(ids(&body_json(next).await).len(), 1);
    for other in [
        format!("{CANDIDATES}?state=submitted&cursor={cursor}"),
        format!("{CANDIDATES}?cursor={cursor}"),
        format!("{CANDIDATES}?state=staged&target_id={target}&cursor={cursor}"),
    ] {
        assert_eq!(get(&q.app, tenant, &other).await.status(), 400, "{other}");
    }
}

/// The candidate list's cursor binds the caller's tenant too: another tenant's reuse of it is 400.
#[tokio::test]
async fn a_candidate_list_cursor_from_another_tenant_is_refused() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    for i in 1..=3 {
        let response = post(
            &q.app,
            tenant,
            CANDIDATES,
            json!({"target_id": target, "record": vcpu_record_numbered(i)}),
        )
        .await;
        assert_eq!(response.status(), 201);
    }
    let first = body_json(get(&q.app, tenant, &format!("{CANDIDATES}?limit=2")).await).await;
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    let foreign = get(&q.app, other, &format!("{CANDIDATES}?cursor={cursor}")).await;
    assert_eq!(foreign.status(), 400);
}

#[tokio::test]
async fn another_tenants_candidate_is_not_found() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let created = body_json(
        post(
            &q.app,
            tenant,
            CANDIDATES,
            json!({"target_id": target, "record": vcpu_record()}),
        )
        .await,
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let own = get(&q.app, tenant, &format!("{CANDIDATES}/{id}")).await;
    assert_eq!(own.status(), 200);
    assert_eq!(
        own.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"1\"")
    );
    assert_eq!(body_json(own).await, created);
    assert_eq!(
        get(&q.app, other, &format!("{CANDIDATES}/{id}"))
            .await
            .status(),
        404
    );
    assert!(
        ids(&body_json(get(&q.app, other, CANDIDATES).await).await).is_empty(),
        "another tenant lists none"
    );
    // A manual record may not name another tenant's target either.
    let response = post(
        &q.app,
        other,
        CANDIDATES,
        json!({"target_id": target, "record": vcpu_record()}),
    )
    .await;
    assert_eq!(response.status(), 404);
    assert_eq!(q.count("products_quota_candidate").await, 1);
}

/// A stored candidate whose record is not a JSON object is a 500 naming no row, never "not found"
/// and never skipped from a page; the row's table, tenant and id stay in the log.
#[tokio::test]
async fn a_stored_candidate_that_does_not_decode_is_500() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let created = body_json(
        post(
            &q.app,
            tenant,
            CANDIDATES,
            json!({"target_id": target, "record": vcpu_record()}),
        )
        .await,
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let raw = Database::connect(&*q.dsn).await.unwrap();
    raw.execute_unprepared("DROP TRIGGER products_quota_candidate_content")
        .await
        .unwrap();
    // The database holds this one candidate.
    raw.execute_unprepared("UPDATE products_quota_candidate SET record_json = '[]'")
        .await
        .unwrap();
    raw.close().await.unwrap();
    for uri in [format!("{CANDIDATES}/{id}"), CANDIDATES.to_owned()] {
        let (logs, guard) = LogCapture::install();
        let response = get(&q.app, tenant, &uri).await;
        drop(guard);
        assert_eq!(response.status(), 500, "{uri}");
        let body = body_json(response).await;
        assert!(
            body.to_string()
                .contains("a stored quota row does not read"),
            "{uri}: {body}"
        );
        assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
        // The row's table, tenant and id are in the log, not on the wire.
        assert!(!body.to_string().contains(id), "{uri}: {body}");
        let lines = logs.lines();
        assert!(
            lines.iter().any(|line| {
                line.starts_with("a stored quota row does not read")
                    && line.contains("products_quota_candidate")
                    && line.contains(&tenant.to_string())
                    && line.contains(id)
            }),
            "{uri}: {lines:?}"
        );
    }
}

/// A body holding U+0000 in any string or key is 400 `VALIDATION` on its path, before anything
/// reads it, on every quota endpoint that takes a body: Postgres would refuse it while `SQLite`
/// stores it, so neither engine sees one, and nothing is written.
#[tokio::test]
async fn a_nul_in_a_quota_body_is_validation_on_its_path() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q
        .submitted_candidate(tenant, vcpu_record_numbered(1), 1)
        .await;
    let staged = q.manual_candidate(tenant, vcpu_record_numbered(2)).await;
    let target = q.fresh_target(tenant).await;
    let mut record = vcpu_record_numbered(3);
    record["mapping_key"] = json!("tenant\u{0}vcpu");
    let mut keyed = vcpu_record_numbered(4);
    keyed["properties"] = json!({"a\u{0}b": "x"});
    let mut submit = submit_body(&vcpu_record_numbered(2), Uuid::new_v4(), 1, None);
    submit["profile_conditions"]["en"] = json!("stopped\u{0}");
    let approver = authed_ctx(tenant);
    for (ctx, uri, if_match, body, path) in [
        (
            tenant_user(tenant),
            TARGETS.to_owned(),
            None,
            json!({"plugin_instance_id": TEST_PLUGIN, "target_key": "k-nul", "name": "a\u{0}b"}),
            "name",
        ),
        (
            tenant_user(tenant),
            CANDIDATES.to_owned(),
            None,
            json!({"target_id": target, "record": record}),
            "record.mapping_key",
        ),
        (
            tenant_user(tenant),
            CANDIDATES.to_owned(),
            None,
            json!({"target_id": target, "record": keyed}),
            "record.properties (a key)",
        ),
        (
            tenant_user(tenant),
            format!("{CANDIDATES}/{}/submit", staged.0),
            Some(staged.1.clone()),
            submit,
            "profile_conditions.en",
        ),
        (
            approver.clone(),
            format!("{CANDIDATES}/{}/approve", submitted.0),
            Some(submitted.1.clone()),
            json!({"note": "fine\u{0}"}),
            "note",
        ),
        (
            approver.clone(),
            format!("{CANDIDATES}/{}/reject", submitted.0),
            Some(submitted.1.clone()),
            json!({"note": "no\u{0}"}),
            "note",
        ),
    ] {
        let response = request_as(
            &q.app,
            &ctx,
            Method::POST,
            &uri,
            Some(body),
            if_match.as_deref(),
        )
        .await;
        let status = response.status();
        let body = body_json(response).await;
        assert_eq!(status, 400, "{uri}: {body}");
        assert_eq!(problem_code(&body), "VALIDATION", "{uri}: {body}");
        assert!(violation_for(&body, path).is_some(), "{uri} {path}: {body}");
    }
    assert_eq!(q.count("products_quota_adapter_target").await, 3);
    assert_eq!(q.count("products_quota_candidate").await, 2);
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'submitted'")
            .await,
        1
    );
    assert_eq!(q.count("products_quota_definition_version").await, 0);
}

/// Every refusal of a candidate endpoint names the quota definition, whatever its code (P-D-273):
/// the list's bad state, a body that does not read, a key the header cannot carry, and another
/// body under a used key.
#[tokio::test]
async fn a_candidate_refusal_names_the_quota_definition() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let target = q.create_target(tenant).await;
    let user = tenant_user(tenant);
    let record =
        |i| Some(json!({"target_id": target, "record": vcpu_record_numbered(i)}).to_string());
    let post_as = |key: Option<&'static str>, body: Option<String>| {
        send(&q.app, &user, Method::POST, CANDIDATES, (key, None), body)
    };
    assert_eq!(post_as(Some("m-1"), record(1)).await.status(), 201);
    for (response, expected) in [
        (
            get(&q.app, tenant, &format!("{CANDIDATES}?state=done")).await,
            (400, "VALIDATION"),
        ),
        (
            post_as(None, Some("not json".to_owned())).await,
            (400, "VALIDATION"),
        ),
        (post_as(Some(" "), record(2)).await, (400, "VALIDATION")),
        (
            post_as(Some("m-1"), record(2)).await,
            (409, "IDEMPOTENCY_CONFLICT"),
        ),
    ] {
        let (status, code, resource) = refusal(response).await;
        assert_eq!((status, code.as_str()), expected);
        assert_eq!(resource, labels::QUOTA_DEFINITION, "{expected:?}");
    }
    assert_eq!(q.count("products_quota_candidate").await, 1);
}

/// A page of approved candidates reads its definitions' digests in one statement, digest only:
/// never one read per candidate, and never a definition's content or its availability.
#[tokio::test]
async fn a_candidate_page_reads_its_definition_digests_in_one_statement() {
    use crate::quota_test_support::{quota_app_recorded, seed_sku_version};
    let tenant = Uuid::new_v4();
    let (q, recorder) = quota_app_recorded(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    for i in 0..2 {
        q.approved(tenant, vcpu_record_numbered(i), sku, 1).await;
    }
    recorder.clear();
    let page = body_json(get(&q.app, tenant, CANDIDATES).await).await;
    let approved = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["definition_ref"]["digest"].is_string())
        .count();
    assert_eq!(approved, 2, "{page}");
    let statements: Vec<String> = recorder.events().into_iter().map(|e| e.sql).collect();
    let definition_reads: Vec<&String> = statements
        .iter()
        .filter(|sql| sql.contains("products_quota_definition_version"))
        .collect();
    assert_eq!(definition_reads.len(), 1, "{statements:#?}");
    assert!(
        !definition_reads[0].contains("content_json"),
        "{statements:#?}"
    );
    assert!(
        !statements
            .iter()
            .any(|sql| sql.contains("products_quota_availability")),
        "{statements:#?}"
    );
}

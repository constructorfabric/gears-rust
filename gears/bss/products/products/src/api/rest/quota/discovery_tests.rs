//! The discovery endpoint (P-D-268): one complete snapshot staged with one candidate per valid
//! record and the others as rejected notes; the same revision again answers the stored rows; other
//! content at the revision is a conflict; an incomplete snapshot or a registry outage stores
//! nothing; drift and retired are notes only; and without an adapter discovery is closed before
//! any effect.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::api::rest::quota::TARGETS;
use crate::authz::labels;
use crate::domain::quota::record::{Resolutions, judge_record};
use crate::domain::quota::{DefinitionVersion, MappingVersion};
use crate::infra::storage::repo::quota_catalog_repo as catalog;
use crate::quota_test_support::{
    FakeDiscovery, FakeResolver, GIB_UNIT, QuotaApp, QuotaFakes, VCPU_METRIC, quota_app,
    ram_record_in, refusal, send, snapshot_of, vcpu_record, vcpu_record_numbered,
};
use crate::test_support::{
    authed_ctx, body_json, post, problem_code, raw_string_opt, repo_connection, request,
    tenant_user,
};
use axum::http::{Method, StatusCode};
use bss_products_sdk::quota::catalog::QuotaVersion;
use serde_json::{Value, json};
use uuid::Uuid;

fn discoveries(target: Uuid) -> String {
    format!("{TARGETS}/{target}/discoveries")
}

/// The code a 501 or 503 carries, leading its detail.
fn detail_code(body: &Value) -> String {
    body["detail"]
        .as_str()
        .and_then(|d| d.split(':').next())
        .unwrap_or_default()
        .to_owned()
}

/// A POST's status and raw body bytes.
async fn discover(q: &QuotaApp, tenant: Uuid, target: Uuid) -> (StatusCode, Vec<u8>) {
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

/// A keyed discovery POST with no body.
async fn keyed(q: &QuotaApp, tenant: Uuid, target: Uuid, key: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let response = q
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri(discoveries(target))
                .extension(tenant_user(tenant))
                .header("Idempotency-Key", key)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, body_json(response).await)
}

#[tokio::test]
async fn a_complete_snapshot_stages_one_candidate_per_valid_record() {
    let tenant = Uuid::new_v4();
    let mut bad = vcpu_record_numbered(2);
    bad["metric_ref"] =
        json!("gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.nothing.v1");
    let q = quota_app(
        tenant,
        QuotaFakes::discovering(
            &[vcpu_record_numbered(0), ram_record_in(GIB_UNIT), bad],
            "7",
        ),
    )
    .await;
    let target = q.create_target(tenant).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 201);
    let body = body_json(response).await;
    assert_eq!(body["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(body["rejected"][0]["code"], "QUOTA_REFERENCE_UNKNOWN");
    assert_eq!(body["rejected"][0]["field"], "metric_ref");
    assert!(
        body["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["state"] == "staged" && c["origin"] == "discovery")
    );
    assert!(
        body["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["snapshot_row_id"] == body["snapshot"]["id"])
    );
    assert_eq!(body["snapshot"]["source_revision"], "7");
    assert_eq!(body["snapshot"]["item_count"], 3);
    assert_eq!(q.count("products_quota_snapshot").await, 1);
    assert_eq!(q.count("products_quota_candidate").await, 2);
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_discovery.intake'")
            .await,
        1
    );
}

#[tokio::test]
async fn the_same_snapshot_again_recovers_the_stored_candidates() {
    let tenant = Uuid::new_v4();
    let q = quota_app(
        tenant,
        QuotaFakes::discovering(&[vcpu_record_numbered(1), vcpu_record_numbered(2)], "7"),
    )
    .await;
    let target = q.create_target(tenant).await;
    let (first_status, first) = discover(&q, tenant, target).await;
    assert_eq!(first_status, StatusCode::CREATED);
    let calls = q.registry.calls();
    let (again_status, again) = discover(&q, tenant, target).await;
    assert_eq!(again_status, StatusCode::OK);
    assert_eq!(
        String::from_utf8(again).unwrap(),
        String::from_utf8(first).unwrap(),
        "the replay answers the stored rows, byte for byte"
    );
    assert_eq!(
        q.registry.calls(),
        calls,
        "the stored revision answers before any record is judged"
    );
    assert_eq!(q.count("products_quota_snapshot").await, 1);
    assert_eq!(q.count("products_quota_candidate").await, 2);
}

/// A registry outage does not turn the replay of a stored revision into a 503: the stored
/// snapshot answers before any record is judged.
#[tokio::test]
async fn a_replay_answers_while_the_registry_is_down() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::discovering(&[vcpu_record()], "7")).await;
    let target = q.create_target(tenant).await;
    let (status, _) = discover(&q, tenant, target).await;
    assert_eq!(status, StatusCode::CREATED);
    q.registry.go_down();
    let (status, body) = discover(&q, tenant, target).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}

#[tokio::test]
async fn other_content_at_the_same_revision_is_a_source_conflict() {
    let tenant = Uuid::new_v4();
    let mut changed = vcpu_record();
    changed["limits"]["max"] = json!("2048");
    let q = quota_app(
        tenant,
        QuotaFakes::answering(FakeDiscovery::scripted(vec![
            snapshot_of(&[vcpu_record()], "7"),
            snapshot_of(&[changed], "7"),
        ])),
    )
    .await;
    let target = q.create_target(tenant).await;
    let (status, _) = discover(&q, tenant, target).await;
    assert_eq!(status, StatusCode::CREATED);
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 409);
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_SOURCE_CONFLICT");
    assert_eq!(
        body["context"]["resource_type"],
        labels::QUOTA_ADAPTER_TARGET,
        "{body}"
    );
    assert_eq!(q.count("products_quota_snapshot").await, 1);
    assert_eq!(q.count("products_quota_candidate").await, 1);
}

#[tokio::test]
async fn a_registry_outage_stores_nothing_and_answers_503() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::discovering(&[vcpu_record()], "7")).await;
    let target = q.create_target(tenant).await;
    q.registry.go_down();
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 503);
    let body = body_json(response).await;
    assert_eq!(detail_code(&body), "QUOTA_REGISTRY_UNAVAILABLE");
    assert!(
        !body.to_string().contains("the fake registry is down"),
        "the registry's own text is never answered: {body}"
    );
    assert_eq!(q.count("products_quota_snapshot").await, 0);
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

#[tokio::test]
async fn an_incomplete_snapshot_stores_nothing_and_answers_503() {
    let tenant = Uuid::new_v4();
    let (a, b) = (vcpu_record_numbered(1), vcpu_record_numbered(2));
    let mut first = snapshot_of(&[a.clone(), b.clone()], "7");
    first.items = vec![a];
    first.next_cursor = Some("c1".to_owned());
    let mut second = snapshot_of(&[first.items[0].clone(), b.clone()], "8");
    second.items = vec![b];
    let q = quota_app(
        tenant,
        QuotaFakes::answering(FakeDiscovery::scripted(vec![first, second])),
    )
    .await;
    let target = q.create_target(tenant).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 503);
    assert_eq!(
        detail_code(&body_json(response).await),
        "QUOTA_DISCOVERY_INCOMPLETE"
    );
    assert_eq!(q.count("products_quota_snapshot").await, 0);
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

/// A record larger than the per-record bound (P-D-268) makes the snapshot incomplete as its page
/// arrives: 503 `QUOTA_DISCOVERY_INCOMPLETE`, and nothing is stored, though the record would be
/// valid and the snapshot hashes whole.
#[tokio::test]
async fn an_oversized_record_stores_nothing_and_answers_503() {
    use crate::domain::quota::snapshot::MAX_RECORD_BYTES;
    let tenant = Uuid::new_v4();
    let mut large = vcpu_record_numbered(1);
    large["properties"] = json!({"note": "x".repeat(MAX_RECORD_BYTES)});
    let records = [large, vcpu_record_numbered(2)];
    let q = quota_app(tenant, QuotaFakes::discovering(&records, "7")).await;
    let target = q.create_target(tenant).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 503);
    let body = body_json(response).await;
    assert_eq!(detail_code(&body), "QUOTA_DISCOVERY_INCOMPLETE", "{body}");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("a record exceeds the configured size")),
        "{body}"
    );
    assert_eq!(q.count("products_quota_snapshot").await, 0);
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

/// A record holding U+0000 in any string or key makes the snapshot incomplete (P-D-268): Postgres
/// would refuse the snapshot's stored records while `SQLite` stores them, so neither engine sees
/// one: 503 `QUOTA_DISCOVERY_INCOMPLETE`, and nothing is stored.
#[tokio::test]
async fn a_record_holding_a_nul_stores_nothing_and_answers_503() {
    let tenant = Uuid::new_v4();
    let mut in_value = vcpu_record_numbered(1);
    in_value["properties"] = json!({"note": "a\u{0}b"});
    let mut in_key = vcpu_record_numbered(2);
    in_key["properties"] = json!({"a\u{0}b": "note"});
    for record in [in_value, in_key] {
        let q = quota_app(tenant, QuotaFakes::discovering(&[record], "7")).await;
        let target = q.create_target(tenant).await;
        let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
        assert_eq!(response.status(), 503);
        let body = body_json(response).await;
        assert_eq!(detail_code(&body), "QUOTA_DISCOVERY_INCOMPLETE", "{body}");
        assert_eq!(q.count("products_quota_snapshot").await, 0);
        assert_eq!(q.count("products_quota_candidate").await, 0);
    }
}

/// An approved mapping and its definition, as an approval writes them, for the record `raw` of
/// the candidate `candidate`: the mapping carries the record's digest and evidence.
async fn approve_directly(q: &QuotaApp, tenant: Uuid, target: Uuid, candidate: Uuid, raw: &Value) {
    let judged = judge_record(
        q.state.registry.as_ref(),
        &authed_ctx(tenant),
        raw,
        &mut Resolutions::default(),
    )
    .await
    .unwrap();
    let (db, scope) = repo_connection(&q.dsn, tenant).await;
    let conn = db.conn().unwrap();
    let now = crate::infra::storage::stored_now();
    let mapping = MappingVersion {
        mapping_id: Uuid::new_v4(),
        version: QuotaVersion::new(1).unwrap(),
        target_id: target,
        capability_key: judged.record.capability_key.clone(),
        plugin_instance_id: crate::quota_test_support::TEST_PLUGIN.to_owned(),
        target_key: "region-1".to_owned(),
        mapping_key: judged.record.mapping_key.clone(),
        mapping_json: json!({
            "record_digest": judged.record_digest,
            "evidence": judged.evidence.to_json(),
        }),
        digest: "a".repeat(64),
        profile_id: Uuid::new_v4(),
        profile_version: QuotaVersion::new(1).unwrap(),
        profile_json: json!({}),
        profile_digest: "b".repeat(64),
        created_by: tenant_user(tenant).subject_id(),
        created_at: now,
    };
    catalog::insert_mapping(&conn, &scope, tenant, &mapping)
        .await
        .unwrap();
    catalog::insert_definition(
        &conn,
        &scope,
        tenant,
        &DefinitionVersion {
            definition_id: Uuid::new_v4(),
            version: QuotaVersion::new(1).unwrap(),
            candidate_id: candidate,
            sku_id: Uuid::new_v4(),
            sku_version: QuotaVersion::new(1).unwrap(),
            metric_uuid: Uuid::new_v4(),
            subject_type_uuid: Uuid::new_v4(),
            profile_id: mapping.profile_id,
            mapping_id: mapping.mapping_id,
            mapping_version: mapping.version,
            content_json: json!({"approved": true}),
            digest: "c".repeat(64),
            created_by: tenant_user(tenant).subject_id(),
            created_at: now,
        },
        &[],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_changed_registry_record_is_reported_as_drift_and_leaves_the_definition() {
    let tenant = Uuid::new_v4();
    let record = vcpu_record();
    let q = quota_app(
        tenant,
        QuotaFakes::answering(FakeDiscovery::scripted(vec![
            snapshot_of(std::slice::from_ref(&record), "7"),
            snapshot_of(std::slice::from_ref(&record), "8"),
        ])),
    )
    .await;
    let target = q.create_target(tenant).await;
    let first = body_json(post(&q.app, tenant, &discoveries(target), json!({})).await).await;
    let candidate = Uuid::parse_str(first["candidates"][0]["id"].as_str().unwrap()).unwrap();
    approve_directly(&q, tenant, target, candidate, &record).await;
    let mut metric = q
        .state
        .registry
        .instance(&authed_ctx(tenant), VCPU_METRIC)
        .await
        .unwrap()
        .unwrap()
        .content;
    metric["description"] = json!({"en": "vCPUs allocated to the tenant, counted anew."});
    q.registry.bump(VCPU_METRIC, metric);
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 201);
    let body = body_json(response).await;
    assert_eq!(body["drift"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["drift"][0]["capability_key"], record["capability_key"]);
    assert_eq!(body["drift"][0]["mapping_version"], "1");
    assert!(body["retired"].as_array().unwrap().is_empty());
    assert_eq!(q.count("products_quota_mapping_version").await, 1);
    assert_eq!(q.count("products_quota_definition_version").await, 1);
    let digest = raw_string_opt(
        &q.dsn,
        "SELECT digest AS v FROM products_quota_definition_version",
    )
    .await;
    assert_eq!(digest, Some("c".repeat(64)), "the definition is unchanged");
}

#[tokio::test]
async fn a_capability_missing_from_a_complete_snapshot_is_a_retired_note() {
    let tenant = Uuid::new_v4();
    let gone = vcpu_record_numbered(9);
    let q = quota_app(
        tenant,
        QuotaFakes::answering(FakeDiscovery::scripted(vec![
            snapshot_of(&[gone.clone(), vcpu_record()], "7"),
            snapshot_of(&[vcpu_record()], "8"),
        ])),
    )
    .await;
    let target = q.create_target(tenant).await;
    let first = body_json(post(&q.app, tenant, &discoveries(target), json!({})).await).await;
    let candidate = first["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["record"]["capability_key"] == gone["capability_key"])
        .map(|c| Uuid::parse_str(c["id"].as_str().unwrap()).unwrap())
        .unwrap();
    approve_directly(&q, tenant, target, candidate, &gone).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 201);
    let body = body_json(response).await;
    assert_eq!(body["retired"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["retired"][0]["capability_key"], "tenant-vcpu-9");
    assert!(body["drift"].as_array().unwrap().is_empty());
    assert_eq!(q.count("products_quota_mapping_version").await, 1);
    assert_eq!(q.count("products_quota_definition_version").await, 1);
}

#[tokio::test]
async fn without_an_adapter_discovery_is_closed_before_any_effect() {
    let tenant = Uuid::new_v4();
    let q = quota_app(
        tenant,
        QuotaFakes {
            discovery: FakeResolver::closed(),
            ..QuotaFakes::default()
        },
    )
    .await;
    let target = q.create_target(tenant).await;
    let calls = q.registry.calls();
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 501);
    let body = body_json(response).await;
    assert_eq!(detail_code(&body), "QUOTA_DISCOVERY_CLOSED");
    assert_eq!(
        body["context"]["resource_type"],
        labels::QUOTA_ADAPTER_TARGET,
        "{body}"
    );
    assert_eq!(q.registry.calls(), calls);
    assert_eq!(q.count("products_quota_snapshot").await, 0);
}

/// A keyed discovery answers its first answer again, before the adapter is asked: the retry
/// recovers revision 7 even though the adapter has moved on to revision 8.
#[tokio::test]
async fn a_keyed_discovery_replays_its_first_answer() {
    let tenant = Uuid::new_v4();
    let discovery = FakeDiscovery::scripted(vec![
        snapshot_of(&[vcpu_record()], "7"),
        snapshot_of(&[vcpu_record()], "8"),
    ]);
    let q = quota_app(tenant, QuotaFakes::answering(discovery.clone())).await;
    let target = q.create_target(tenant).await;
    let (status, first) = keyed(&q, tenant, target, "discover-1").await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, again) = keyed(&q, tenant, target, "discover-1").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(again, first);
    assert_eq!(discovery.requests().len(), 1, "the retry asks no adapter");
    assert_eq!(q.count("products_quota_snapshot").await, 1);
    let unkeyed = request(
        &q.app,
        tenant,
        Method::POST,
        &discoveries(target),
        None,
        None,
    )
    .await;
    assert_eq!(unkeyed.status(), StatusCode::CREATED, "revision 8 is new");
}

/// A revision below the target's highest stored is refused before any record is judged
/// (P-D-268): with the registry down it is 409 `QUOTA_SOURCE_STALE`, not 503, and the registry is
/// not asked.
#[tokio::test]
async fn a_stale_revision_is_refused_before_any_record_is_judged() {
    let tenant = Uuid::new_v4();
    let discovery = FakeDiscovery::scripted(vec![
        snapshot_of(&[vcpu_record()], "8"),
        snapshot_of(&[vcpu_record_numbered(1)], "7"),
    ]);
    let q = quota_app(tenant, QuotaFakes::answering(discovery)).await;
    let target = q.create_target(tenant).await;
    let (status, _) = discover(&q, tenant, target).await;
    assert_eq!(status, StatusCode::CREATED);
    q.registry.go_down();
    let calls = q.registry.calls();
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 409);
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_SOURCE_STALE", "{body}");
    assert_eq!(q.registry.calls(), calls, "nothing is judged");
    assert_eq!(q.count("products_quota_snapshot").await, 1);
}

/// One discovery has an overall deadline (P-D-268): an adapter whose pages each answer within the
/// page timeout but together outlast it is 503 `QUOTA_DISCOVERY_INCOMPLETE`, and nothing is
/// stored.
#[tokio::test]
async fn a_discovery_past_its_deadline_stores_nothing() {
    let tenant = Uuid::new_v4();
    let records: Vec<Value> = (1..=3).map(vcpu_record_numbered).collect();
    let slow = FakeDiscovery::slow(
        snapshot_of(&records, "7"),
        1,
        std::time::Duration::from_millis(60),
    );
    let mut fakes = QuotaFakes::answering(slow);
    fakes.settings.deadline = std::time::Duration::from_millis(100);
    let q = quota_app(tenant, fakes).await;
    let target = q.create_target(tenant).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    assert_eq!(response.status(), 503);
    let body = body_json(response).await;
    assert_eq!(detail_code(&body), "QUOTA_DISCOVERY_INCOMPLETE", "{body}");
    assert_eq!(q.count("products_quota_snapshot").await, 0);
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

/// The discovery deadline covers the judging of the records too (P-D-268): an adapter that
/// answers at once while types-registry answers slowly is cut at the deadline, 503
/// `QUOTA_DISCOVERY_INCOMPLETE`, and nothing is stored.
#[tokio::test]
async fn judging_past_the_discovery_deadline_stores_nothing() {
    let tenant = Uuid::new_v4();
    let records: Vec<Value> = (1..=3).map(vcpu_record_numbered).collect();
    let mut fakes = QuotaFakes::discovering(&records, "7");
    fakes.registry = fakes.registry.slow(std::time::Duration::from_millis(40));
    fakes.settings.deadline = std::time::Duration::from_millis(150);
    let q = quota_app(tenant, fakes).await;
    let target = q.create_target(tenant).await;
    let response = post(&q.app, tenant, &discoveries(target), json!({})).await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(detail_code(&body), "QUOTA_DISCOVERY_INCOMPLETE", "{body}");
    assert_eq!(q.count("products_quota_snapshot").await, 0);
    assert_eq!(q.count("products_quota_candidate").await, 0);
}

/// The discovery operation declares its own request timeout (P-D-268): the configured deadline
/// plus 15 s for the staging, which runs after the deadline's cut, so a late discovery answers
/// 503 `QUOTA_DISCOVERY_INCOMPLETE` before the gateway cuts the request. 40 s by default, and it
/// follows a configured deadline; every other quota operation keeps the gateway's default.
#[tokio::test]
async fn the_discovery_route_declares_its_deadline_and_the_staging_as_its_timeout() {
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
    let tenant = Uuid::new_v4();
    let path = format!("{TARGETS}/{{id}}/discoveries");
    for (deadline_ms, declared) in [(None, "40s"), (Some(60_000), "1m 15s")] {
        let mut cfg = crate::config::ProductsConfig::default();
        if let Some(ms) = deadline_ms {
            cfg.quota_discovery_deadline_ms = ms;
        }
        let fakes = QuotaFakes {
            settings: cfg.quota_settings(),
            ..QuotaFakes::default()
        };
        let q = quota_app(tenant, fakes).await;
        let openapi = OpenApiRegistryImpl::new();
        let _router = crate::api::rest::quota::router(q.state.clone(), &openapi);
        let api =
            serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();
        assert_eq!(
            api["paths"][&path]["post"]["x-toolkit-request-timeout"], declared,
            "{deadline_ms:?}"
        );
        for (route, item) in api["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                if route != &path {
                    assert!(
                        op.get("x-toolkit-request-timeout").is_none(),
                        "{method} {route}"
                    );
                }
            }
        }
    }
}

/// A discovery's refusals name the adapter target, a key the header cannot carry included
/// (P-D-273).
#[tokio::test]
async fn a_discovery_refusal_names_the_adapter_target() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::discovering(&[vcpu_record()], "7")).await;
    let target = q.create_target(tenant).await;
    let response = send(
        &q.app,
        &tenant_user(tenant),
        Method::POST,
        &discoveries(target),
        (Some(" "), None),
        None,
    )
    .await;
    let (status, code, resource) = refusal(response).await;
    assert_eq!((status, code.as_str()), (400, "VALIDATION"));
    assert_eq!(resource, labels::QUOTA_ADAPTER_TARGET);
    assert_eq!(q.count("products_quota_snapshot").await, 0);
}

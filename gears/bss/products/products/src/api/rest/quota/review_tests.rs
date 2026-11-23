//! The review endpoints (P-D-269, P-D-270, P-D-271): a submit proposes a staged candidate for one
//! SKU version with plan units into which its bounds convert exactly, under the candidate's
//! `If-Match`; a keyed retry replays its first answer; every refusal names the quota definition.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::api::rest::quota::CANDIDATES;
use crate::authz::labels;
use crate::quota_test_support::{
    BYTES_UNIT, GB_UNIT, GIB_UNIT, LogCapture, QuotaApp, QuotaFakes, VCPU_METRIC, VCPU_UNIT,
    quota_app, ram_record_in, refusal, seed_sku_version, send, submit_body, vcpu_record,
    vcpu_record_numbered,
};
use crate::test_support::{
    authed_ctx, body_json, problem_code, raw_i64, request, request_as, tenant_user, violation_for,
};
use axum::http::Method;
use serde_json::{Value, json};
use uuid::Uuid;

fn submit_uri(id: Uuid) -> String {
    format!("{CANDIDATES}/{id}/submit")
}

async fn submit_with(
    q: &QuotaApp,
    tenant: Uuid,
    id: Uuid,
    body: &Value,
    etag: &str,
) -> axum::response::Response {
    request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(id),
        Some(body.clone()),
        Some(etag),
    )
    .await
}

/// The submit body of the vCPU record against `sku` at `version`, with `conditions`.
fn proposal(sku: Uuid, version: &str, units: &[&str], conditions: &Value) -> Value {
    json!({"sku_id": sku, "sku_version": version, "allowed_unit_refs": units,
           "supported_bases": ["fixed", "per_plan_unit"], "profile_conditions": conditions})
}

#[tokio::test]
async fn submit_moves_a_staged_candidate_with_its_proposal() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let candidate = q.manual_candidate(tenant, vcpu_record()).await; // (id, etag)
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 3).await;
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(
            json!({"sku_id": sku, "sku_version": "3", "allowed_unit_refs": [VCPU_UNIT],
                    "supported_bases": ["fixed", "per_plan_unit"],
                    "profile_conditions": {"en": "Allocated vCPUs count while a VM is stopped."}}),
        ),
        Some(&candidate.1),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"2\"")
    );
    let body = body_json(response).await;
    assert_eq!(body["state"], "submitted");
    assert_eq!(body["version"], 2);
    assert_eq!(body["proposal"]["allowed_units"][0]["max"], "1024");
    assert_eq!(body["proposal"]["sku_version"], "3");
    assert_eq!(
        body["proposal"]["supported_bases"],
        json!(["fixed", "per_plan_unit"])
    );
    assert_eq!(
        body["submitted_by"],
        json!(tenant_user(tenant).subject_id())
    );
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.submit'")
            .await,
        1
    );
}

#[tokio::test]
async fn submit_needs_english_profile_conditions() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let candidate = q.manual_candidate(tenant, vcpu_record()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 3).await;
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(proposal(
            sku,
            "3",
            &[VCPU_UNIT],
            &json!({"ru": "Vydelennye vCPU uchityvayutsya."}),
        )),
        Some(&candidate.1),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_DISPLAY_TEXT_MISSING");
    assert!(
        violation_for(&body, "profile_conditions").is_some(),
        "{body}"
    );
    let read = body_json(
        request(
            &q.app,
            tenant,
            Method::GET,
            &format!("{CANDIDATES}/{}", candidate.0),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(read["state"], "staged");
}

#[tokio::test]
async fn submit_with_an_unknown_sku_version_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let candidate = q.manual_candidate(tenant, vcpu_record()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 3).await;
    let en = json!({"en": "Counts stopped VMs."});
    for (sku, version) in [(sku, "4"), (sku, "0"), (sku, "03"), (Uuid::new_v4(), "1")] {
        let response = request(
            &q.app,
            tenant,
            Method::POST,
            &submit_uri(candidate.0),
            Some(proposal(sku, version, &[VCPU_UNIT], &en)),
            Some(&candidate.1),
        )
        .await;
        assert_eq!(response.status(), 400, "{version}");
        let body = body_json(response).await;
        assert_eq!(problem_code(&body), "QUOTA_SKU_VERSION_UNKNOWN", "{body}");
        assert!(violation_for(&body, "sku_version").is_some(), "{body}");
    }
}

#[tokio::test]
async fn a_gib_record_cannot_be_offered_in_gb() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let candidate = q.manual_candidate(tenant, ram_record_in(GIB_UNIT)).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let en = json!({"en": "Counts stopped VMs."});
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(proposal(sku, "1", &[GB_UNIT], &en)),
        Some(&candidate.1),
    )
    .await;
    assert_eq!(response.status(), 400);
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_UNIT_INEXACT");
    assert!(
        violation_for(&body, "allowed_unit_refs[0]").is_some(),
        "{body}"
    );
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(proposal(sku, "1", &[BYTES_UNIT, GIB_UNIT], &en)),
        Some(&candidate.1),
    )
    .await;
    assert_eq!(response.status(), 200);
    let units = body_json(response).await["proposal"]["allowed_units"].clone();
    assert_eq!(units[0]["unit_ref"], BYTES_UNIT);
    assert_eq!(
        (&units[0]["step"], &units[0]["max"]),
        (&json!("1073741824"), &json!("68719476736"))
    );
    assert_eq!(units[1]["unit_ref"], GIB_UNIT);
    assert_eq!(
        (&units[1]["step"], &units[1]["max"]),
        (&json!("1"), &json!("64"))
    );
}

/// Each plan unit is judged as the record's units are (P-D-269), its refusal on
/// `allowed_unit_refs[i]` with its index: a unit of another canonical unit is
/// `QUOTA_UNIT_MISMATCH`, a metric named as a unit `QUOTA_REFERENCE_FAMILY`, an unregistered unit
/// `QUOTA_REFERENCE_UNKNOWN`, and a minor-bearing reference `QUOTA_VERSION_UNSUPPORTED`. The
/// candidate stays staged at version 1.
#[tokio::test]
async fn each_plan_unit_refusal_names_its_index_and_leaves_the_candidate_staged() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let candidate = q.manual_candidate(tenant, vcpu_record()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let en = json!({"en": "Counts stopped VMs."});
    let unregistered = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.nothing.v1";
    let minor = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1.1";
    for (units, code, field) in [
        (
            vec![VCPU_UNIT, BYTES_UNIT],
            "QUOTA_UNIT_MISMATCH",
            "allowed_unit_refs[1]",
        ),
        (
            vec![VCPU_METRIC],
            "QUOTA_REFERENCE_FAMILY",
            "allowed_unit_refs[0]",
        ),
        (
            vec![VCPU_UNIT, unregistered],
            "QUOTA_REFERENCE_UNKNOWN",
            "allowed_unit_refs[1]",
        ),
        (
            vec![minor],
            "QUOTA_VERSION_UNSUPPORTED",
            "allowed_unit_refs[0]",
        ),
    ] {
        let response = submit_with(
            &q,
            tenant,
            candidate.0,
            &proposal(sku, "1", &units, &en),
            &candidate.1,
        )
        .await;
        let status = response.status();
        let body = body_json(response).await;
        assert_eq!(status, 400, "{code}: {body}");
        assert_eq!(problem_code(&body), code, "{body}");
        assert!(
            violation_for(&body, field).is_some(),
            "{code} on {field}: {body}"
        );
        assert_eq!(
            q.count("products_quota_candidate WHERE state = 'staged' AND version = 1")
                .await,
            1,
            "{code}"
        );
    }
}

#[tokio::test]
async fn submit_with_a_stale_if_match_is_stale_revision() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let record = vcpu_record();
    let first = q.manual_candidate(tenant, record.clone()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let body = submit_body(&record, sku, 1, None);
    // A wrong tag on a staged candidate is a stale revision.
    let (status, code, resource) =
        refusal(submit_with(&q, tenant, first.0, &body, "\"9\"").await).await;
    assert_eq!((status, code.as_str()), (409, "STALE_REVISION"));
    assert_eq!(resource, labels::QUOTA_DEFINITION);
    assert_eq!(
        submit_with(&q, tenant, first.0, &body, &first.1)
            .await
            .status(),
        200
    );
    // The same tag again: the candidate has moved on to another state.
    let (status, code, resource) =
        refusal(submit_with(&q, tenant, first.0, &body, &first.1).await).await;
    assert_eq!((status, code.as_str()), (409, "QUOTA_CANDIDATE_STATE"));
    assert_eq!(resource, labels::QUOTA_DEFINITION);
}

#[tokio::test]
async fn submit_without_if_match_is_validation() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let record = vcpu_record();
    let candidate = q.manual_candidate(tenant, record.clone()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(submit_body(&record, sku, 1, None)),
        None,
    )
    .await;
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "VALIDATION");
    assert!(violation_for(&body, "If-Match").is_some(), "{body}");
    assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
    assert_eq!(body["status"], 400);
}

/// A keyed submit answers its first answer again, after the move too, and moves once; another
/// body under the key is a conflict.
#[tokio::test]
async fn a_keyed_submit_replays_its_first_answer() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let record = vcpu_record();
    let candidate = q.manual_candidate(tenant, record.clone()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 2).await;
    let user = tenant_user(tenant);
    let uri = submit_uri(candidate.0);
    let keyed = |version: i64| {
        send(
            &q.app,
            &user,
            Method::POST,
            &uri,
            (Some("submit-1"), Some(candidate.1.as_str())),
            Some(submit_body(&record, sku, version, None).to_string()),
        )
    };
    let first = keyed(1).await;
    assert_eq!(first.status(), 200);
    let first = body_json(first).await;
    let calls = q.registry.calls();
    let again = keyed(1).await;
    assert_eq!(again.status(), 200);
    assert_eq!(
        again.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"2\"")
    );
    assert_eq!(body_json(again).await, first);
    assert_eq!(q.registry.calls(), calls, "a replay resolves nothing");
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.submit'")
            .await,
        1
    );
    let (status, code, _) = refusal(keyed(2).await).await;
    assert_eq!((status, code.as_str()), (409, "IDEMPOTENCY_CONFLICT"));
}

/// The proposal pins what intake judged: a metric types-registry revised since the candidate was
/// staged is 409 `QUOTA_SOURCE_CONFLICT`, and the candidate stays staged. The detail names the
/// remedy that works: a discovery of the same revision replays the stored candidate unjudged.
#[tokio::test]
async fn a_registry_revision_since_intake_is_a_source_conflict() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let record = vcpu_record();
    let candidate = q.manual_candidate(tenant, record.clone()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let mut metric = crate::quota_test_support::catalog_instances()
        .into_iter()
        .find(|(id, _)| id == VCPU_METRIC)
        .unwrap()
        .1;
    metric["description"]["en"] = json!("A new description.");
    q.registry.bump(VCPU_METRIC, metric);
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(submit_body(&record, sku, 1, None)),
        Some(&candidate.1),
    )
    .await;
    let body = body_json(response).await;
    assert_eq!(
        (body["status"].clone(), problem_code(&body)),
        (json!(409), "QUOTA_SOURCE_CONFLICT".to_owned())
    );
    assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
    let detail = body["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains(
            "a new snapshot revision of the target, or a manual record under a new record id, \
             judges it again"
        ) && !detail.contains("discover the capability again"),
        "{body}"
    );
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'staged'")
            .await,
        1
    );
}

fn decide_uri(id: Uuid, verb: &str) -> String {
    format!("{CANDIDATES}/{id}/{verb}")
}

/// `ctx` decides `candidate` (`approve` or `reject`) with `body`.
async fn decide(
    q: &QuotaApp,
    ctx: &toolkit_security::SecurityContext,
    candidate: &(Uuid, String),
    verb: &str,
    body: Value,
) -> axum::response::Response {
    request_as(
        &q.app,
        ctx,
        Method::POST,
        &decide_uri(candidate.0, verb),
        Some(body),
        Some(&candidate.1),
    )
    .await
}

/// The status and the resource type of an answer; a miss carries no code of its own.
async fn missed(response: axum::response::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = body_json(response).await;
    (status, body["context"]["resource_type"].clone())
}

/// The version `version` of `definition`, as its read answers it in English.
async fn stored_definition(q: &QuotaApp, tenant: Uuid, definition: &Value, version: u64) -> Value {
    let id = definition.as_str().unwrap();
    let response = request(
        &q.app,
        tenant,
        Method::GET,
        &format!("/bss-products/v1/quota-definitions/{id}/versions/{version}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), 200);
    body_json(response).await
}

#[tokio::test]
async fn an_approval_publishes_one_definition_version_unavailable_for_sale() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 3).await; // by tenant_user
    let approver = authed_ctx(tenant); // another subject of the same tenant
    let calls = q.registry.calls();
    let response = decide(&q, &approver, &submitted, "approve", json!({})).await;
    assert_eq!(response.status(), 200);
    let body = body_json(response).await;
    assert_eq!(body["candidate"]["state"], "approved");
    assert_eq!(
        body["candidate"]["decided_by"],
        json!(approver.subject_id())
    );
    let d = &body["definition"];
    assert_eq!(d["version"], "1");
    assert_eq!(d["sku_version"], "3");
    assert_eq!(d["availability"]["state"], "disabled");
    assert_eq!(d["availability"]["reason_code"], "qualification_pending");
    assert_eq!(d["availability"]["revision"], "1");
    assert!(d["qualification_ref"].is_null());
    assert_eq!(d["display_name"], "Virtual CPUs");
    assert_eq!(d["locale"], "en");
    assert_eq!(d["metric_ref"], VCPU_METRIC);
    assert_eq!(d["allowed_units"][0]["max"], "1024");
    assert_eq!(d["supported_bases"], json!(["fixed", "per_plan_unit"]));
    assert_eq!(
        body["candidate"]["definition_ref"],
        json!({"id": d["definition_id"], "version": "1", "digest": d["definition_digest"]})
    );
    assert_eq!(
        q.registry.calls(),
        calls,
        "approval reads only stored evidence"
    );
    assert_eq!(
        raw_i64(
            &q.dsn,
            "SELECT COUNT(*) AS v FROM products_quota_definition_version"
        )
        .await,
        1
    );
    assert_eq!(q.count("products_quota_mapping_version").await, 1);
    assert_eq!(q.count("products_quota_availability").await, 1);
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.approve'")
            .await,
        1
    );
    assert_eq!(
        stored_definition(&q, tenant, &d["definition_id"], 1).await,
        *d,
        "the answer is the stored version"
    );
}

#[tokio::test]
async fn the_submitter_cannot_approve_and_the_author_of_a_manual_record_cannot_either() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    // Authored and submitted by tenant_user.
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 3).await;
    let user = tenant_user(tenant);
    for verb in ["approve", "reject"] {
        let (logs, guard) = LogCapture::install();
        let as_submitter = decide(&q, &user, &submitted, verb, json!({"note": "mine"})).await;
        drop(guard);
        let (status, code, resource) = refusal(as_submitter).await;
        assert_eq!((status, code.as_str()), (403, "SOD_VIOLATION"), "{verb}");
        assert_eq!(resource, labels::QUOTA_DEFINITION);
        // The denial is logged under the authorization denial target, with the subject.
        let lines = logs.lines();
        assert!(
            lines.iter().any(|line| {
                line.contains("refused by separation of duties")
                    && line.contains(&user.subject_id().to_string())
                    && line.contains(&submitted.0.to_string())
                    && line.ends_with("target=bss_products.authz.deny")
            }),
            "{verb}: {lines:?}"
        );
    }
    // A manual record authored by A and submitted by B: A may not approve either.
    let author = authed_ctx(tenant);
    let by_author = q
        .submitted_candidate_by(tenant, &author, vcpu_record_numbered(1), 3)
        .await;
    let as_author = decide(&q, &author, &by_author, "approve", json!({})).await;
    assert_eq!(as_author.status(), 403);
    assert_eq!(problem_code(&body_json(as_author).await), "SOD_VIOLATION");
    assert_eq!(q.count("products_quota_definition_version").await, 0);
    let independent = decide(&q, &authed_ctx(tenant), &by_author, "approve", json!({})).await;
    assert_eq!(independent.status(), 200);
}

#[tokio::test]
async fn a_reject_needs_a_note_and_closes_the_candidate() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let approver = authed_ctx(tenant);
    for body in [json!({}), json!({"note": "  "})] {
        let (status, code, resource) =
            refusal(decide(&q, &approver, &submitted, "reject", body).await).await;
        assert_eq!((status, code.as_str()), (400, "NOTE_REQUIRED"));
        assert_eq!(resource, labels::QUOTA_DEFINITION);
    }
    let rejected = decide(
        &q,
        &approver,
        &submitted,
        "reject",
        json!({"note": "units"}),
    )
    .await;
    assert_eq!(rejected.status(), 200);
    let body = body_json(rejected).await;
    assert_eq!(body["state"], "rejected");
    assert_eq!(body["decided_note"], "units");
    assert_eq!(body["version"], 3);
    let again = decide(
        &q,
        &approver,
        &(submitted.0, "\"3\"".to_owned()),
        "approve",
        json!({}),
    )
    .await;
    let (status, code, _) = refusal(again).await;
    assert_eq!((status, code.as_str()), (409, "QUOTA_CANDIDATE_STATE"));
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.reject'")
            .await,
        1
    );
    assert_eq!(q.count("products_quota_definition_version").await, 0);
}

/// An approve or a reject without `If-Match` is 400 `VALIDATION`, with a tag of another version
/// 409 `STALE_REVISION`, and a note over 2000 characters `FIELD_TOO_LONG`; each names the quota
/// definition, and the candidate stays submitted.
#[tokio::test]
async fn a_decision_needs_its_if_match_and_a_short_note() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let approver = authed_ctx(tenant);
    for verb in ["approve", "reject"] {
        let stale = decide(
            &q,
            &approver,
            &(submitted.0, "\"9\"".to_owned()),
            verb,
            json!({"note": "ok"}),
        )
        .await;
        let (status, code, resource) = refusal(stale).await;
        assert_eq!((status, code.as_str()), (409, "STALE_REVISION"), "{verb}");
        assert_eq!(resource, labels::QUOTA_DEFINITION);
        let untagged = request_as(
            &q.app,
            &approver,
            Method::POST,
            &decide_uri(submitted.0, verb),
            Some(json!({"note": "ok"})),
            None,
        )
        .await;
        let body = body_json(untagged).await;
        assert_eq!(
            (body["status"].clone(), problem_code(&body)),
            (json!(400), "VALIDATION".into())
        );
        assert!(violation_for(&body, "If-Match").is_some(), "{body}");
        assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
        let long = decide(
            &q,
            &approver,
            &submitted,
            verb,
            json!({"note": "n".repeat(2001)}),
        )
        .await;
        let (status, code, resource) = refusal(long).await;
        assert_eq!((status, code.as_str()), (400, "FIELD_TOO_LONG"), "{verb}");
        assert_eq!(resource, labels::QUOTA_DEFINITION);
    }
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'submitted'")
            .await,
        1
    );
}

/// A keyed approval answers its first answer again after the decision, and publishes once.
#[tokio::test]
async fn a_keyed_approval_replays_after_the_decision() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let approver = authed_ctx(tenant);
    let uri = decide_uri(submitted.0, "approve");
    let keyed = |body: &'static str| {
        send(
            &q.app,
            &approver,
            Method::POST,
            &uri,
            (Some("approve-1"), Some(submitted.1.as_str())),
            Some(body.to_owned()),
        )
    };
    let first = keyed("{}").await;
    assert_eq!(first.status(), 200);
    let first = body_json(first).await;
    let again = keyed("{}").await;
    assert_eq!(again.status(), 200, "after the decision too");
    assert_eq!(body_json(again).await, first);
    assert_eq!(q.count("products_quota_definition_version").await, 1);
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.approve'")
            .await,
        1
    );
    let (status, code, _) = refusal(keyed(r#"{"note": "other"}"#).await).await;
    assert_eq!((status, code.as_str()), (409, "IDEMPOTENCY_CONFLICT"));
    // Without the key, a client whose answer was lost sees the state.
    let (status, code, _) =
        refusal(decide(&q, &approver, &submitted, "approve", json!({})).await).await;
    assert_eq!((status, code.as_str()), (409, "QUOTA_CANDIDATE_STATE"));
}

/// A keyed rejection answers its first answer again, byte for byte with the same `ETag`, and
/// rejects once.
#[tokio::test]
async fn a_keyed_reject_replays_its_first_answer() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let approver = authed_ctx(tenant);
    let uri = decide_uri(submitted.0, "reject");
    let keyed = || {
        send(
            &q.app,
            &approver,
            Method::POST,
            &uri,
            (Some("reject-1"), Some(submitted.1.as_str())),
            Some(json!({"note": "units"}).to_string()),
        )
    };
    let (first, first_tag, first_bytes) = answered(keyed().await).await;
    assert_eq!(first, 200);
    assert_eq!(first_tag.as_deref(), Some("\"3\""));
    let (again, again_tag, again_bytes) = answered(keyed().await).await;
    assert_eq!(again, 200);
    assert_eq!(again_tag, first_tag);
    assert_eq!(
        again_bytes, first_bytes,
        "the replay is the first answer's bytes"
    );
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.reject'")
            .await,
        1
    );
}

/// An answer's status, `ETag` and raw body bytes.
async fn answered(response: axum::response::Response) -> (u16, Option<String>, Vec<u8>) {
    let status = response.status().as_u16();
    let tag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, tag, bytes.to_vec())
}

#[tokio::test]
async fn a_registry_revision_after_approval_leaves_the_definition_unchanged() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let submitted = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let approved =
        body_json(decide(&q, &authed_ctx(tenant), &submitted, "approve", json!({})).await).await;
    let mut metric = crate::quota_test_support::registered(VCPU_METRIC);
    metric["description"]["en"] = json!("A new description.");
    q.registry.bump(VCPU_METRIC, metric);
    let d = &approved["definition"];
    let read = stored_definition(&q, tenant, &d["definition_id"], 1).await;
    assert_eq!(read["definition_digest"], d["definition_digest"]);
    assert_eq!(read["metric_digest"], d["metric_digest"]);
    assert_eq!(read["description"], d["description"]);
    assert_ne!(read["description"], "A new description.");
}

#[tokio::test]
async fn a_second_version_of_a_definition_keeps_the_first_readable() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let target = q.fresh_target(tenant).await;
    let user = tenant_user(tenant);
    let first_record = vcpu_record();
    let first = q
        .manual_candidate_on(&user, target, first_record.clone())
        .await;
    let first = q.submit(tenant, &first, &first_record, sku, 1, None).await;
    let v1 = body_json(decide(&q, &authed_ctx(tenant), &first, "approve", json!({})).await).await;
    let definition = Uuid::parse_str(v1["definition"]["definition_id"].as_str().unwrap()).unwrap();
    // A manual record of the same capability with another mapping key, versioning the definition.
    let mut second_record = vcpu_record();
    second_record["id"] = json!(Uuid::new_v4().to_string());
    second_record["mapping_key"] = json!("tenant-vcpu-mapping-4");
    let second = q
        .manual_candidate_on(&user, target, second_record.clone())
        .await;
    let second = q
        .submit(tenant, &second, &second_record, sku, 1, Some(definition))
        .await;
    let v2 = body_json(decide(&q, &authed_ctx(tenant), &second, "approve", json!({})).await).await;
    let d2 = &v2["definition"];
    assert_eq!(d2["definition_id"], json!(definition));
    assert_eq!(d2["version"], "2");
    assert_eq!(d2["mapping_ref"]["version"], "2");
    assert_eq!(
        d2["mapping_ref"]["id"], v1["definition"]["mapping_ref"]["id"],
        "one lineage"
    );
    assert_eq!(
        stored_definition(&q, tenant, &v1["definition"]["definition_id"], 1).await,
        v1["definition"],
        "version 1 reads as stored"
    );
    assert_eq!(q.count("products_quota_mapping_version").await, 2);
}

/// A proposal may version only a definition of the candidate's own metric.
#[tokio::test]
async fn a_named_definition_of_another_metric_is_refused() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let ram = q
        .submitted_candidate(tenant, ram_record_in(GIB_UNIT), 1)
        .await;
    let approved =
        body_json(decide(&q, &authed_ctx(tenant), &ram, "approve", json!({})).await).await;
    let definition =
        Uuid::parse_str(approved["definition"]["definition_id"].as_str().unwrap()).unwrap();
    let record = vcpu_record();
    let candidate = q.manual_candidate(tenant, record.clone()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(submit_body(&record, sku, 1, Some(definition))),
        Some(&candidate.1),
    )
    .await;
    let body = body_json(response).await;
    assert_eq!(problem_code(&body), "QUOTA_RECORD_INVALID", "{body}");
    assert!(violation_for(&body, "definition_id").is_some(), "{body}");
    // A definition the tenant does not hold is a miss on the quota definition (P-D-273).
    let unknown = Uuid::new_v4();
    let response = request(
        &q.app,
        tenant,
        Method::POST,
        &submit_uri(candidate.0),
        Some(submit_body(&record, sku, 1, Some(unknown))),
        Some(&candidate.1),
    )
    .await;
    assert_eq!(
        missed(response).await,
        (404, json!(labels::QUOTA_DEFINITION))
    );
}

/// A proposal may version only a definition of its own SKU (P-D-270): a definition approved for
/// another SKU is 400 `QUOTA_RECORD_INVALID` on `definition_id`, whose detail names both SKUs,
/// and the candidate stays staged.
#[tokio::test]
async fn a_named_definition_of_another_sku_is_refused_at_submit() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let compute = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (definition, _) = q
        .approved(tenant, vcpu_record_numbered(0), compute, 1)
        .await;
    let storage = seed_sku_version(&q.dsn, tenant, "STORAGE", 1).await;
    let record = vcpu_record_numbered(1);
    let candidate = q.manual_candidate(tenant, record.clone()).await;
    let body = body_json(
        submit_with(
            &q,
            tenant,
            candidate.0,
            &submit_body(&record, storage, 1, Some(definition)),
            &candidate.1,
        )
        .await,
    )
    .await;
    assert_eq!(body["status"], 400, "{body}");
    assert_eq!(problem_code(&body), "QUOTA_RECORD_INVALID", "{body}");
    assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
    let detail = violation_for(&body, "definition_id").expect("refused on definition_id");
    assert!(
        detail.contains(&compute.to_string()) && detail.contains(&storage.to_string()),
        "{detail}"
    );
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'staged'")
            .await,
        1
    );
}

/// The approval reads the named definition's SKU again inside its transaction (P-D-270): a
/// submitted proposal whose named definition applies to another SKU, stored past the submit's
/// own check, is 400 `QUOTA_RECORD_INVALID` on `definition_id`, and nothing is stored.
#[tokio::test]
async fn a_named_definition_of_another_sku_is_refused_at_approval() {
    use crate::domain::quota::ReviewWrite;
    use crate::domain::quota::review::proposal_json;
    use crate::infra::storage::repo::quota_intake_repo as store;
    use crate::quota_test_support::{judged_entities, proposal_of};
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let compute = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (definition, _) = q
        .approved(tenant, vcpu_record_numbered(0), compute, 1)
        .await;
    let storage = seed_sku_version(&q.dsn, tenant, "STORAGE", 1).await;
    let record = vcpu_record_numbered(1);
    let staged = q.manual_candidate(tenant, record.clone()).await;
    let (db, scope) = crate::test_support::repo_connection(&q.dsn, tenant).await;
    let (_, pinned) = judged_entities(&record);
    let moved = store::review_candidate(
        &db.conn().unwrap(),
        &scope,
        tenant,
        staged.0,
        1,
        ReviewWrite::Submit {
            proposal: proposal_json(&proposal_of(&record, storage, 1, Some(definition))),
            evidence: pinned.to_json(),
            by: tenant_user(tenant).subject_id(),
            at: crate::infra::storage::stored_now(),
            note: None,
        },
    )
    .await
    .unwrap();
    assert!(moved, "the candidate is submitted through the store");
    drop(db);
    let submitted = (staged.0, "\"2\"".to_owned());
    let body =
        body_json(decide(&q, &authed_ctx(tenant), &submitted, "approve", json!({})).await).await;
    assert_eq!(body["status"], 400, "{body}");
    assert_eq!(problem_code(&body), "QUOTA_RECORD_INVALID", "{body}");
    assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
    let detail = violation_for(&body, "definition_id").expect("refused on definition_id");
    assert!(
        detail.contains(&compute.to_string()) && detail.contains(&storage.to_string()),
        "{detail}"
    );
    assert_eq!(q.count("products_quota_definition_version").await, 1);
    assert_eq!(q.count("products_quota_mapping_version").await, 1);
    assert_eq!(q.count("products_quota_availability").await, 1);
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'submitted'")
            .await,
        1
    );
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.approve'")
            .await,
        1,
        "the first approval only"
    );
}

/// A candidate of another target versions a definition of its own SKU and metric (P-D-270): the
/// definition takes its next version, and the other target's lineage mints its own mapping.
#[tokio::test]
async fn a_candidate_of_another_target_versions_a_definition_of_its_sku_and_metric() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let user = tenant_user(tenant);
    let (first_target, other_target) = (q.fresh_target(tenant).await, q.fresh_target(tenant).await);
    let first_record = vcpu_record_numbered(0);
    let first = q
        .manual_candidate_on(&user, first_target, first_record.clone())
        .await;
    let first = q.submit(tenant, &first, &first_record, sku, 1, None).await;
    let v1 = body_json(decide(&q, &authed_ctx(tenant), &first, "approve", json!({})).await).await;
    let definition = Uuid::parse_str(v1["definition"]["definition_id"].as_str().unwrap()).unwrap();
    let record = vcpu_record_numbered(1);
    let second = q
        .manual_candidate_on(&user, other_target, record.clone())
        .await;
    let second = q
        .submit(tenant, &second, &record, sku, 1, Some(definition))
        .await;
    let response = decide(&q, &authed_ctx(tenant), &second, "approve", json!({})).await;
    assert_eq!(response.status(), 200);
    let d2 = body_json(response).await["definition"].clone();
    assert_eq!(d2["definition_id"], json!(definition));
    assert_eq!(d2["version"], "2");
    assert_eq!(d2["mapping_ref"]["version"], "1", "{d2}");
    assert_ne!(
        d2["mapping_ref"]["id"], v1["definition"]["mapping_ref"]["id"],
        "another target's lineage"
    );
    assert_eq!(q.count("products_quota_mapping_version").await, 2);
}

/// Every review request asks its own grant on `quota_definition` for the caller's tenant only:
/// `submit` for a submit, `approve` for an approval and a rejection.
#[tokio::test]
async fn every_review_request_asks_its_grant_for_the_callers_tenant_only() {
    use authz_resolver_sdk::models::TenantMode;
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let to_approve = q.submitted_candidate(tenant, vcpu_record(), 1).await;
    let to_reject = q
        .submitted_candidate(tenant, vcpu_record_numbered(1), 1)
        .await;
    let before = q.pdp.asked().len();
    let approver = authed_ctx(tenant);
    assert_eq!(
        decide(&q, &approver, &to_approve, "approve", json!({}))
            .await
            .status(),
        200
    );
    assert_eq!(
        decide(&q, &approver, &to_reject, "reject", json!({"note": "no"}))
            .await
            .status(),
        200
    );
    let asked = q.pdp.asked();
    let definition = labels::QUOTA_DEFINITION.to_owned();
    assert_eq!(
        asked[before..],
        [
            (
                definition.clone(),
                "approve".to_owned(),
                Some(TenantMode::RootOnly)
            ),
            (
                definition.clone(),
                "approve".to_owned(),
                Some(TenantMode::RootOnly)
            ),
        ]
    );
    assert!(
        asked.contains(&(definition, "submit".to_owned(), Some(TenantMode::RootOnly))),
        "{asked:?}"
    );
}

/// A grant on some definitions versions only those (P-D-273): the submit, and the approval again
/// inside its transaction, read the definition a proposal names under the whole grant, so a
/// definition outside it is 404 on the quota definition, the candidate does not move, and
/// nothing is stored. A definition inside it takes its next version.
#[tokio::test]
async fn a_grant_on_some_definitions_versions_only_those() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (d1, _) = q.approved(tenant, vcpu_record_numbered(1), sku, 1).await;
    let (d2, _) = q.approved(tenant, vcpu_record_numbered(2), sku, 1).await;
    // Submitted for d2 while the grant still reaches it; a second candidate stays staged.
    let named = vcpu_record_numbered(3);
    let staged = q.manual_candidate(tenant, named.clone()).await;
    let for_d2 = q.submit(tenant, &staged, &named, sku, 1, Some(d2)).await;
    let other = vcpu_record_numbered(4);
    let candidate = q.manual_candidate(tenant, other.clone()).await;
    // The grant is now `owner_tenant_id = T AND id IN (d1)`.
    q.pdp.narrow_to(&[d1]);
    let response = submit_with(
        &q,
        tenant,
        candidate.0,
        &submit_body(&other, sku, 1, Some(d2)),
        &candidate.1,
    )
    .await;
    assert_eq!(
        missed(response).await,
        (404, json!(labels::QUOTA_DEFINITION))
    );
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'staged' AND version = 1")
            .await,
        1
    );
    let approver = authed_ctx(tenant);
    let response = decide(&q, &approver, &for_d2, "approve", json!({})).await;
    assert_eq!(
        missed(response).await,
        (404, json!(labels::QUOTA_DEFINITION))
    );
    assert_eq!(q.count("products_quota_definition_version").await, 2);
    assert_eq!(q.count("products_quota_mapping_version").await, 2);
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'submitted'")
            .await,
        1
    );
    // A definition the grant reaches takes its next version.
    let for_d1 = q.submit(tenant, &candidate, &other, sku, 1, Some(d1)).await;
    let published = body_json(decide(&q, &approver, &for_d1, "approve", json!({})).await).await;
    assert_eq!(
        published["definition"]["definition_id"],
        json!(d1),
        "{published}"
    );
    assert_eq!(published["definition"]["version"], "2", "{published}");
}

/// Two approvals of different candidates that name one definition race for its next version
/// (P-D-270): the loser is 409 `CONTENDED` on the quota definition, without a retry of its own,
/// and writes nothing. `SQLite` queues the two writers, so the race's window (between the
/// loser's read of the latest version and its insert) is opened by a trigger: when the loser's
/// candidate moves, the other approval's version 2 is written inside that window.
#[tokio::test]
async fn a_lost_race_for_a_definitions_next_version_is_contended() {
    use crate::test_support::id_matches;
    use sea_orm::{ConnectionTrait, Database};
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (definition, _) = q.approved(tenant, vcpu_record_numbered(0), sku, 1).await;
    let mut naming = Vec::new();
    for i in 1..=2 {
        let record = vcpu_record_numbered(i);
        let candidate = q.manual_candidate(tenant, record.clone()).await;
        naming.push(
            q.submit(tenant, &candidate, &record, sku, 1, Some(definition))
                .await,
        );
    }
    let (loser, winner) = (&naming[0], &naming[1]);
    let columns = "tenant_id, definition_id, version, candidate_id, sku_id, sku_version, \
                   metric_uuid, subject_type_uuid, profile_id, mapping_id, mapping_version, \
                   content_json, digest, created_by, created_at";
    let raw = Database::connect(&*q.dsn).await.unwrap();
    raw.execute_unprepared(&format!(
        "CREATE TRIGGER the_other_approval_takes_version_two AFTER UPDATE OF state ON \
         products_quota_candidate WHEN NEW.state = 'approved' AND {} BEGIN \
         INSERT INTO products_quota_definition_version ({columns}) \
         SELECT v.tenant_id, v.definition_id, 2, \
         (SELECT c.id FROM products_quota_candidate c WHERE {}), v.sku_id, v.sku_version, \
         v.metric_uuid, v.subject_type_uuid, v.profile_id, v.mapping_id, v.mapping_version, \
         v.content_json, v.digest, v.created_by, v.created_at \
         FROM products_quota_definition_version v WHERE {} AND v.version = 1; END",
        id_matches("NEW.id", loser.0),
        id_matches("c.id", winner.0),
        id_matches("v.definition_id", definition),
    ))
    .await
    .unwrap();
    let rows = format!(
        "products_quota_definition_version WHERE {}",
        id_matches("definition_id", definition)
    );
    let (status, code, resource) =
        refusal(decide(&q, &authed_ctx(tenant), loser, "approve", json!({})).await).await;
    assert_eq!((status, code.as_str()), (409, "CONTENDED"));
    assert_eq!(resource, labels::QUOTA_DEFINITION);
    // The loser wrote nothing: its candidate is still submitted, the definition holds one row.
    assert_eq!(q.count(&rows).await, 1);
    assert_eq!(
        q.count("products_quota_candidate WHERE state = 'submitted'")
            .await,
        2
    );
    assert_eq!(
        q.count("products_audit_log WHERE action = 'quota_candidate.approve'")
            .await,
        1
    );
    raw.execute_unprepared("DROP TRIGGER the_other_approval_takes_version_two")
        .await
        .unwrap();
    raw.close().await.unwrap();
    // The other approval publishes version 2, one row; the loser's own retry takes version 3.
    let won = body_json(decide(&q, &authed_ctx(tenant), winner, "approve", json!({})).await).await;
    assert_eq!(won["definition"]["version"], "2", "{won}");
    assert_eq!(q.count(&format!("{rows} AND version = 2")).await, 1);
    let retried =
        body_json(decide(&q, &authed_ctx(tenant), loser, "approve", json!({})).await).await;
    assert_eq!(retried["definition"]["version"], "3", "{retried}");
}

/// The review operations say what they answer (P-D-269): the submit names its plan-unit, basis
/// and pin refusals; approve and reject say that their JSON body is required (`{}` at least) and
/// when the note is; the submit and the reject declare their `ETag`, the approval (whose
/// candidate is frozen) none.
#[test]
fn the_review_operations_declare_their_answers() {
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
    let openapi = OpenApiRegistryImpl::new();
    let _router = super::review::register(axum::Router::new(), &openapi);
    let api =
        serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();
    let op = |verb: &str| api["paths"][format!("{CANDIDATES}/{{id}}/{verb}")]["post"].clone();
    let text = |verb: &str| op(verb)["description"].as_str().unwrap().to_owned();
    let etag = |verb: &str| op(verb)["responses"]["200"]["headers"]["ETag"].clone();
    let submit = text("submit");
    for named in [
        "QUOTA_VERSION_UNSUPPORTED",
        "VALIDATION on `supported_bases[i]`",
        "QUOTA_REFERENCE_UNKNOWN on the record's field",
        "QUOTA_SOURCE_CONFLICT",
        "a new snapshot revision of the target, or a manual record under a new record id",
    ] {
        assert!(submit.contains(named), "{named}: {submit}");
    }
    assert!(text("approve").contains("A JSON body is required (`{}` at least)"));
    assert!(text("approve").contains("the note is optional"));
    for verb in ["submit", "approve"] {
        assert!(
            text(verb).contains("QUOTA_RECORD_INVALID on `definition_id`")
                && text(verb).contains("of another SKU or metric"),
            "{verb}: {}",
            text(verb)
        );
    }
    assert!(text("reject").contains("A JSON body is required (`{}` at least)"));
    assert!(text("reject").contains("the note is required"));
    assert!(etag("submit").is_object(), "{}", op("submit"));
    assert!(etag("reject").is_object(), "{}", op("reject"));
    assert!(etag("approve").is_null(), "{}", op("approve"));
}

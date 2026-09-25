use std::collections::BTreeMap;

use gts::GtsTypeId;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    ActiveQuotaCounts, AttributionDigest, CapPatch, ContractRef, DECISION_BLOB_VERSION,
    DebitRequest, Decision, DecisionPreview, DecisionResult, EnforcementMode,
    EvaluationAttribution, IdempotencyRecord, IdempotencyScope, IdempotencySubjectKey, LeaseState,
    MetricId, MetricKind, NO_APPLICABLE_QUOTA, NotificationEventKind, OperationType, PageRequest,
    PageResult, PartialIdempotencyWrite, PayloadHash, PeriodType, PolicyId, PolicyScope,
    ProjectionBinding, Quota, QuotaDebitPlan, QuotaDraft, QuotaId, QuotaPatch, QuotaSource,
    QuotaSpec, QuotaStatus, QuotaType, QuotaView, ResourceProjection, Retention,
    RollbackableOperation, ScopeError, SubjectRef, SubjectScope, TenantId, UnknownValue,
    ValidityWindow, apportion, positive_amount,
};
use crate::gts::{SCOPE_TENANT, SCOPE_TYPE, SCOPE_USER};

fn ts(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).expect("valid unix timestamp")
}

// --- GTS-anchored closed enums ---------------------------------------------

#[test]
fn quota_type_round_trips_through_its_gts_instance_id() {
    for value in QuotaType::ALL {
        let id = value.as_gts_id();
        assert!(id.starts_with(QuotaType::BASE_TYPE_ID), "{id}");
        assert!(!id.ends_with('~'), "instance ids never end with '~': {id}");
        let parsed: QuotaType = id.parse().expect("parse back");
        assert_eq!(parsed, *value);
        let json = serde_json::to_string(value).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
        let back: QuotaType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, *value);
    }
}

#[test]
fn gts_enums_reject_short_names_and_unknown_ids() {
    let err = "allocation"
        .parse::<QuotaType>()
        .expect_err("short name rejected");
    assert_eq!(
        err,
        UnknownValue {
            kind: "quota_type",
            value: "allocation".to_owned(),
        }
    );
    assert!(
        "gts.cf.qe.quota.type.v1~cf.qe.quota.burst.v1"
            .parse::<QuotaType>()
            .is_err()
    );
    assert!(
        serde_json::from_str::<EnforcementMode>("\"hard\"").is_err(),
        "enforcement mode must be the full GTS id"
    );
    assert!("".parse::<PeriodType>().is_err());
}

#[test]
fn every_gts_enum_value_derives_from_its_declared_base() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            EnforcementMode::BASE_TYPE_ID,
            EnforcementMode::ALL.iter().map(|v| v.as_gts_id()).collect(),
        ),
        (
            QuotaSource::BASE_TYPE_ID,
            QuotaSource::ALL.iter().map(|v| v.as_gts_id()).collect(),
        ),
        (
            PeriodType::BASE_TYPE_ID,
            PeriodType::ALL.iter().map(|v| v.as_gts_id()).collect(),
        ),
    ];
    for (base, ids) in cases {
        assert!(base.ends_with('~'), "base must be a type id: {base}");
        for id in &ids {
            assert!(id.starts_with(base), "{id} must derive from {base}");
        }
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len(), "duplicate ids under {base}");
    }
    assert_eq!(PeriodType::ALL.len(), 5, "PRD 5.4 reserves five periods");
    assert_eq!(
        PeriodType::OneTime.as_gts_id(),
        "gts.cf.qe.period.type.v1~cf.qe.period.one_time.v1"
    );
}

// --- plain closed enums ----------------------------------------------------

#[test]
fn notification_event_kinds_serialize_as_the_prd_kebab_case_catalog() {
    let cases = vec![
        (NotificationEventKind::ThresholdCrossed, "threshold-crossed"),
        (NotificationEventKind::PeriodRollover, "period-rollover"),
        (
            NotificationEventKind::LeaseAutoReleased,
            "lease-auto-released",
        ),
        (
            NotificationEventKind::LeaseResolvedByDeactivation,
            "lease-resolved-by-deactivation",
        ),
        (NotificationEventKind::QuotaChanged, "quota-changed"),
        (
            NotificationEventKind::QuotaCounterAdjusted,
            "quota-counter-adjusted",
        ),
        (
            NotificationEventKind::QuotaRollbackApplied,
            "quota-rollback-applied",
        ),
        (NotificationEventKind::PolicyChanged, "policy-changed"),
    ];
    for (kind, wire) in cases {
        assert_eq!(serde_json::to_value(kind).expect("serialize"), json!(wire));
        let back: NotificationEventKind = serde_json::from_value(json!(wire)).expect("parse");
        assert_eq!(back, kind);
    }
    assert!(serde_json::from_value::<NotificationEventKind>(json!("quota-deleted")).is_err());
}

#[test]
fn operation_type_names_match_the_storage_discriminator() {
    for op in [
        OperationType::Debit,
        OperationType::Credit,
        OperationType::Rollback,
        OperationType::Reserve,
        OperationType::Commit,
        OperationType::Release,
        OperationType::BatchDebit,
    ] {
        let wire = serde_json::to_value(op).expect("serialize");
        assert_eq!(
            wire,
            json!(op.as_str()),
            "serde and as_str must agree for {op:?}"
        );
    }
    assert_eq!(OperationType::BatchDebit.as_str(), "batch_debit");
}

#[test]
fn only_the_active_lease_state_is_non_terminal() {
    assert!(!LeaseState::Active.is_terminal());
    for terminal in [
        LeaseState::Committed,
        LeaseState::Released,
        LeaseState::AutoReleased,
        LeaseState::ResolvedByDeactivation,
    ] {
        assert!(terminal.is_terminal(), "{terminal:?} must be terminal");
    }
}

// --- identifiers and digests -----------------------------------------------

#[test]
fn generated_quota_ids_are_time_ordered_uuid_v7() {
    let a = QuotaId::generate();
    let b = QuotaId::generate();
    assert_eq!(a.as_uuid().get_version_num(), 7);
    assert!(a <= b, "UUIDv7 ids generated in sequence must not decrease");
    let raw = Uuid::from_u128(7);
    assert_eq!(QuotaId::new(raw).to_string(), raw.to_string());
    assert_eq!(Uuid::from(QuotaId::from(raw)), raw);
}

#[test]
fn policy_id_recognizes_the_seeded_global_policy() {
    assert!(PolicyId::global().is_global());
    assert!(!PolicyId::new("metric-a").is_global());
    let json = serde_json::to_string(&PolicyId::global()).expect("serialize");
    assert_eq!(json, "\"global\"");
}

#[test]
fn metric_id_accepts_instance_ids_and_rejects_type_ids() {
    let ok = MetricId::parse("gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1");
    assert!(ok.is_ok(), "{ok:?}");
    assert!(
        MetricId::parse("gts.cf.qe.metric.type.v1~").is_err(),
        "type id rejected"
    );
    assert!(MetricId::parse("not-a-gts-id").is_err());
    let metric = ok.expect("parsed");
    let json = serde_json::to_string(&metric).expect("serialize");
    assert_eq!(json, format!("\"{}\"", metric.as_str()));
}

#[test]
fn digest_hex_round_trip_and_rejection() {
    let mut bytes = [0_u8; 32];
    bytes[0] = 0xab;
    bytes[31] = 0x01;
    let key = IdempotencySubjectKey::from_bytes(bytes);
    let hex = key.to_hex();
    assert_eq!(hex.len(), 64);
    assert!(hex.starts_with("ab") && hex.ends_with("01"));
    assert_eq!(IdempotencySubjectKey::parse_hex(&hex).expect("parse"), key);
    assert_eq!(
        IdempotencySubjectKey::parse_hex(&hex.to_uppercase()).expect("uppercase"),
        key
    );
    let json = serde_json::to_string(&key).expect("serialize");
    assert_eq!(json, format!("\"{hex}\""));
    for bad in ["", "abc", &"zz".repeat(32), &"ab".repeat(31)] {
        assert!(
            IdempotencySubjectKey::parse_hex(bad).is_err(),
            "must reject {bad:?}"
        );
    }
    assert!(
        format!("{key:?}").contains(&hex),
        "debug must show the hex form"
    );
}

// --- structs ---------------------------------------------------------------

#[test]
fn validity_window_bounds_are_inclusive_and_optional() {
    let both = ValidityWindow {
        start: Some(ts(100)),
        end: Some(ts(200)),
    };
    assert!(both.contains(ts(100)));
    assert!(both.contains(ts(200)));
    assert!(!both.contains(ts(99)));
    assert!(!both.contains(ts(201)));
    let open_end = ValidityWindow {
        start: Some(ts(100)),
        end: None,
    };
    assert!(open_end.contains(ts(1_000_000)));
    assert!(!open_end.contains(ts(0)));
    assert!(ValidityWindow::default().contains(ts(0)));
}

#[test]
fn validity_window_serializes_rfc3339_and_rejects_unknown_fields() {
    let window = ValidityWindow {
        start: Some(ts(0)),
        end: None,
    };
    let value = serde_json::to_value(window).expect("serialize");
    assert_eq!(
        value,
        json!({ "start": "1970-01-01T00:00:00Z", "end": null })
    );
    let back: ValidityWindow = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back, window);
    let bad = serde_json::from_value::<ValidityWindow>(json!({ "start": null, "until": 1 }));
    assert!(bad.is_err(), "unknown fields are rejected");
}

#[test]
fn quota_patch_default_is_empty_and_any_field_makes_it_non_empty() {
    assert!(QuotaPatch::default().is_empty());
    let patch = QuotaPatch {
        cap: Some(CapPatch::Unbounded),
        ..QuotaPatch::default()
    };
    assert!(!patch.is_empty());
    let parsed: QuotaPatch =
        serde_json::from_value(json!({ "cap": { "bounded": 10 } })).expect("parse patch");
    assert_eq!(parsed.cap, Some(CapPatch::Bounded(10)));
    assert!(serde_json::from_value::<QuotaPatch>(json!({ "metric": "x" })).is_err());
}

#[test]
fn decision_result_is_tagged_and_denied_carries_its_reason() {
    let quota = QuotaId::new(Uuid::from_u128(1));
    let denied = Decision {
        result: DecisionResult::Denied {
            violated_quota_ids: vec![quota],
            reason: "NO_APPLICABLE_QUOTA".to_owned(),
        },
        debit_plan: BTreeMap::new(),
        diagnostics: BTreeMap::new(),
    };
    let value = serde_json::to_value(&denied).expect("serialize");
    assert_eq!(value["result"]["outcome"], json!("denied"));
    assert_eq!(value["result"]["reason"], json!("NO_APPLICABLE_QUOTA"));
    let back: Decision = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back, denied);

    let allowed = Decision {
        result: DecisionResult::Allowed,
        debit_plan: BTreeMap::from([(quota, QuotaDebitPlan { amount: 5 })]),
        diagnostics: BTreeMap::new(),
    };
    let value = serde_json::to_value(&allowed).expect("serialize");
    assert_eq!(value["result"], json!({ "outcome": "allowed" }));
    assert_eq!(value["debit_plan"][quota.to_string()]["amount"], json!(5));
}

#[test]
fn policy_scope_is_tagged_by_kind() {
    let metric = MetricId::parse("gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1")
        .expect("metric id");
    let scope = PolicyScope::Metric {
        metric: metric.clone(),
    };
    let value = serde_json::to_value(&scope).expect("serialize");
    assert_eq!(value["kind"], json!("metric"));
    assert_eq!(value["metric"], json!(metric.as_str()));
    assert_eq!(
        serde_json::to_value(PolicyScope::Global).expect("serialize"),
        json!({ "kind": "global" })
    );
}

#[test]
fn page_types_default_to_the_platform_page_size_and_map_items() {
    let request = PageRequest::default();
    assert_eq!(request.limit, PageRequest::DEFAULT_LIMIT);
    assert_eq!(request.limit, 100, "PRD 5.10 default page size");
    assert!(request.cursor.is_none());
    let page = PageResult {
        items: vec![1_u32, 2, 3],
        next_cursor: Some("c".to_owned()),
    };
    let mapped = page.map(|n| n * 10);
    assert_eq!(mapped.items, vec![10, 20, 30]);
    assert_eq!(mapped.next_cursor.as_deref(), Some("c"));
    assert!(PageResult::<u8>::empty().items.is_empty());
}

#[test]
fn subject_ref_equality_covers_both_halves_of_the_identity() {
    let projection = GtsTypeId::new("gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~");
    let a = SubjectRef {
        projection_type: projection.clone(),
        subject_id: "u1".to_owned(),
    };
    let b = SubjectRef {
        projection_type: projection,
        subject_id: "u2".to_owned(),
    };
    assert_ne!(a, b);
    assert_eq!(a.clone(), a);
}

// --- Scopes and attribution (projection-contracts feature) -----------------

#[test]
fn subject_scope_parse_and_deserialization_accept_only_scope_instances() {
    let user = SubjectScope::parse(SCOPE_USER).expect("user scope");
    assert_eq!(user, SubjectScope::user());
    assert!(!user.is_tenant());
    let tenant = SubjectScope::parse(SCOPE_TENANT).expect("tenant scope");
    assert_eq!(tenant, SubjectScope::tenant());
    assert!(tenant.is_tenant());

    let rejected = [
        SCOPE_TYPE,                                             // a type, not an instance
        "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~", // another type
        "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1", // an instance of another type
        "not-a-gts-id",
        "",
    ];
    for raw in rejected {
        let direct = SubjectScope::parse(raw).expect_err(raw);
        assert!(
            matches!(
                direct,
                ScopeError::Invalid { .. } | ScopeError::NotAScope { .. }
            ),
            "{raw}: {direct:?}"
        );
        let json = serde_json::to_string(raw).expect("json string");
        assert!(
            serde_json::from_str::<SubjectScope>(&json).is_err(),
            "deserialization must run the same check: {raw}"
        );
    }
    assert!(matches!(
        SubjectScope::parse("gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1"),
        Err(ScopeError::NotAScope { .. })
    ));
}

#[test]
fn subject_scope_serializes_as_its_instance_id_and_round_trips() {
    let json = serde_json::to_string(&SubjectScope::user()).expect("serialize");
    assert_eq!(json, format!("\"{SCOPE_USER}\""));
    let back: SubjectScope = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, SubjectScope::user());
    assert_eq!(back.as_gts().as_ref(), SCOPE_USER);
    assert_eq!(back.to_string(), SCOPE_USER);
}

#[test]
fn evaluation_attribution_distinguishes_omitted_metadata_from_null() {
    let tenant = Uuid::from_u128(7);
    let metric = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1";

    let omitted: EvaluationAttribution =
        serde_json::from_value(json!({ "tenant_id": tenant, "metric": metric }))
            .expect("metadata may be omitted at the type level");
    assert_eq!(
        omitted.metadata, None,
        "the gear rejects the omission later"
    );
    assert!(omitted.subjects.is_empty());
    assert_eq!(omitted.resource, None);

    let empty: EvaluationAttribution =
        serde_json::from_value(json!({ "tenant_id": tenant, "metric": metric, "metadata": {} }))
            .expect("an empty object is a present object");
    assert_eq!(empty.metadata.as_ref().map(serde_json::Map::len), Some(0));

    for field in ["metadata", "resource"] {
        let explicit_null = json!({ "tenant_id": tenant, "metric": metric, field: null });
        assert!(
            serde_json::from_value::<EvaluationAttribution>(explicit_null).is_err(),
            "{field}: null is not absence"
        );
    }
    let unknown =
        json!({ "tenant_id": tenant, "metric": metric, "metadata": {}, "caller_type": "x" });
    assert!(serde_json::from_value::<EvaluationAttribution>(unknown).is_err());
}

#[test]
fn resource_projection_keeps_an_absent_id_absent() {
    let without_id: ResourceProjection = serde_json::from_value(json!({
        "type": "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~",
        "metadata": { "model_family": "gpt" }
    }))
    .expect("id may be omitted");
    assert_eq!(without_id.id, None);
    let written = serde_json::to_value(&without_id).expect("serialize");
    let mut keys: Vec<String> = written
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["metadata", "type"],
        "no id key is written for an absent id"
    );

    let with_id: ResourceProjection = serde_json::from_value(json!({
        "type": "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~",
        "id": "model-7",
        "metadata": {}
    }))
    .expect("id may be present");
    assert_eq!(with_id.id.as_deref(), Some("model-7"));

    for field in ["id", "metadata"] {
        let explicit_null = json!({
            "type": "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~",
            "metadata": {},
            field: null
        });
        assert!(
            serde_json::from_value::<ResourceProjection>(explicit_null).is_err(),
            "{field}: null is rejected, the resource base requires a value when present"
        );
    }
    let no_metadata = json!({ "type": "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~" });
    let parsed: ResourceProjection = serde_json::from_value(no_metadata).expect("type only");
    assert_eq!(
        parsed.metadata, None,
        "the gear rejects the omission at ingress"
    );
}

#[test]
fn contract_ref_for_type_takes_the_major_version_of_the_last_segment() {
    let id = GtsTypeId::try_new(
        "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v3~",
    )
    .expect("type id");
    let reference = ContractRef::for_type(&id).expect("versioned");
    assert_eq!(reference.type_id, id);
    assert_eq!(reference.version, 3);
}

#[test]
fn projection_bindings_are_distinct_by_pair() {
    let metric =
        MetricId::parse("gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1").expect("metric");
    let user =
        GtsTypeId::try_new("gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~").expect("type");
    let tenant =
        GtsTypeId::try_new("gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.tenant.v1~").expect("type");
    let set: std::collections::HashSet<ProjectionBinding> = [
        ProjectionBinding {
            metric: metric.clone(),
            projection_type: user.clone(),
        },
        ProjectionBinding {
            metric: metric.clone(),
            projection_type: user,
        },
        ProjectionBinding {
            metric,
            projection_type: tenant,
        },
    ]
    .into_iter()
    .collect();
    assert_eq!(set.len(), 2);
}

// --- quota-lifecycle read and create shapes ---------------------------------

fn spec() -> QuotaSpec {
    QuotaSpec {
        tenant_id: TenantId::new(Uuid::from_u128(7)),
        subject: SubjectRef {
            projection_type: GtsTypeId::new("gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~"),
            subject_id: "u1".to_owned(),
        },
        metric: MetricId::parse("gts.cf.qe.metric.type.v1~cf.qe.metric.ai_tokens_input.v1")
            .expect("metric"),
        quota_type: QuotaType::Consumption,
        period: Some(PeriodType::Month),
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(10),
        notification_thresholds: vec![50, 90],
        validity_window: None,
        fail_open_hint: false,
        metadata: serde_json::Map::new(),
        source: QuotaSource::Operator,
    }
}

fn contract() -> ContractRef {
    ContractRef {
        type_id: GtsTypeId::new(
            "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v1~",
        ),
        version: 1,
    }
}

fn stored(spec: QuotaSpec, window: Option<ValidityWindow>) -> Quota {
    let draft = QuotaDraft::from_spec(spec, contract());
    Quota {
        id: QuotaId::new(Uuid::from_u128(1)),
        tenant_id: draft.tenant_id,
        subject: draft.subject,
        metric: draft.metric,
        quota_type: draft.quota_type,
        period: draft.period,
        enforcement_mode: draft.enforcement_mode,
        cap: draft.cap,
        notification_thresholds: draft.notification_thresholds,
        validity_window: window,
        fail_open_hint: draft.fail_open_hint,
        metadata: draft.metadata,
        source: draft.source,
        status: QuotaStatus::Active,
        constraint_contract: draft.constraint_contract,
        record_version: 1,
        created_at: ts(0),
        updated_at: ts(0),
    }
}

#[test]
fn quota_view_is_computed_for_one_instant_and_flattens_the_record() {
    let window = ValidityWindow {
        start: Some(ts(100)),
        end: Some(ts(200)),
    };
    let quota = stored(spec(), Some(window));
    let inside = QuotaView::compute(quota.clone(), Some(MetricKind::Counter), ts(150));
    assert!(inside.currently_within_window);
    assert_eq!(inside.metric_kind, Some(MetricKind::Counter));
    let after = QuotaView::compute(quota.clone(), None, ts(201));
    assert!(
        !after.currently_within_window,
        "past the end, status untouched"
    );
    assert_eq!(after.quota.status, QuotaStatus::Active);
    let unbounded = QuotaView::compute(stored(spec(), None), Some(MetricKind::Gauge), ts(0));
    assert!(unbounded.currently_within_window);

    let value = serde_json::to_value(&inside).expect("serialize");
    assert_eq!(
        value["id"],
        json!(quota.id.to_string()),
        "record fields are flattened"
    );
    assert_eq!(value["currently_within_window"], json!(true));
    assert_eq!(value["metric_kind"], json!("counter"));
    let back: QuotaView = serde_json::from_value(value).expect("round trip");
    assert_eq!(back, inside);
}

#[test]
fn metric_kind_is_closed_and_snake_case() {
    assert_eq!(
        serde_json::to_value(MetricKind::Gauge).expect("serialize"),
        json!("gauge")
    );
    assert_eq!(
        serde_json::from_value::<MetricKind>(json!("counter")).expect("counter"),
        MetricKind::Counter
    );
    assert!(serde_json::from_value::<MetricKind>(json!("histogram")).is_err());
}

#[test]
fn quota_spec_has_no_contract_field_and_becomes_a_draft_with_one() {
    let spec = spec();
    let value = serde_json::to_value(&spec).expect("serialize");
    assert!(value.get("constraint_contract").is_none());
    let mut with_contract = value;
    with_contract["constraint_contract"] = json!({ "type_id": "x", "version": 1 });
    assert!(
        serde_json::from_value::<QuotaSpec>(with_contract).is_err(),
        "a caller cannot smuggle the contract in"
    );
    let draft = QuotaDraft::from_spec(spec.clone(), contract());
    assert_eq!(draft.constraint_contract, contract());
    assert_eq!(draft.metric, spec.metric);
    assert_eq!(draft.notification_thresholds, vec![50, 90]);
}

#[test]
fn max_cap_is_the_largest_signed_64_bit_value() {
    assert_eq!(Quota::MAX_CAP, 9_223_372_036_854_775_807);
    assert_eq!(i64::try_from(Quota::MAX_CAP), Ok(i64::MAX));
}

#[test]
fn active_quota_counts_default_to_zero_and_round_trip() {
    let mut counts = ActiveQuotaCounts::default();
    assert_eq!((counts.cap_zero, counts.cap_unbounded), (0, 0));
    counts.cap_zero = 2;
    counts.by_metric.insert(
        MetricId::parse("gts.cf.qe.metric.type.v1~cf.qe.metric.ai_tokens_input.v1")
            .expect("metric"),
        3,
    );
    let value = serde_json::to_value(&counts).expect("serialize");
    let back: ActiveQuotaCounts = serde_json::from_value(value).expect("round trip");
    assert_eq!(back, counts);
}

#[test]
fn notification_event_kind_names_equal_their_serialized_form() {
    for kind in [
        NotificationEventKind::ThresholdCrossed,
        NotificationEventKind::PeriodRollover,
        NotificationEventKind::LeaseAutoReleased,
        NotificationEventKind::LeaseResolvedByDeactivation,
        NotificationEventKind::QuotaChanged,
        NotificationEventKind::QuotaCounterAdjusted,
        NotificationEventKind::QuotaRollbackApplied,
        NotificationEventKind::PolicyChanged,
    ] {
        assert_eq!(
            serde_json::to_value(kind).expect("serialize"),
            json!(kind.as_str())
        );
    }
}

#[test]
fn public_policy_inputs_reject_server_owned_fields() {
    let create =
        json!({"scope":{"kind":"global"},"engine_id":"most-restrictive-wins","engine_config":{}});
    let patch = json!({"if_match_version":1,"timeout_ms":8});
    assert!(serde_json::from_value::<super::PolicySpec>(create.clone()).is_ok());
    assert!(serde_json::from_value::<super::PolicyPatch>(patch.clone()).is_ok());
    for field in [
        "created_by",
        "schemas",
        "schema_snapshot",
        "compiled_artifact",
    ] {
        let mut spoofed = create.clone();
        spoofed[field] = json!("caller controlled");
        assert!(serde_json::from_value::<super::PolicySpec>(spoofed).is_err());
        let mut spoofed = patch.clone();
        spoofed[field] = json!("caller controlled");
        assert!(serde_json::from_value::<super::PolicyPatch>(spoofed).is_err());
    }
}

// --- digests ---------------------------------------------------------------

fn subject(projection: &str, id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::new(projection),
        subject_id: id.to_owned(),
    }
}

#[test]
fn a_subject_key_ignores_the_order_and_multiplicity_of_its_pairs() {
    let tenant = subject("gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-1");
    let user = subject("gts.cf.core.qe.subj.v1~acme.user.v1", "u-9");

    let one = IdempotencySubjectKey::of(&[tenant.clone(), user.clone()]);
    let other = IdempotencySubjectKey::of(&[user, tenant.clone(), tenant]);

    assert_eq!(one, other);
}

#[test]
fn a_subject_key_separates_fields_that_would_otherwise_concatenate_alike() {
    let split_one = IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~a.xy.v1", "z")]);
    let split_other = IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~a.x.v1", "yz")]);

    assert_ne!(
        split_one, split_other,
        "length prefixes keep the encoding injective"
    );
}

#[test]
fn different_subject_sets_fingerprint_differently() {
    let one = IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-1")]);
    let other =
        IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-2")]);

    assert_ne!(one, other);
}

#[test]
fn the_empty_subject_set_still_fingerprints_deterministically() {
    assert_eq!(
        IdempotencySubjectKey::of(&[]),
        IdempotencySubjectKey::of(&[])
    );
}

#[test]
fn a_subject_key_is_pinned_to_its_byte_form() {
    // Golden vector: one pair, each field length-prefixed big-endian.
    let expected = {
        use aws_lc_rs::digest::{Context, SHA256};
        let mut hasher = Context::new(&SHA256);
        for field in ["gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-1"] {
            hasher.update(
                &u32::try_from(field.len())
                    .expect("short field")
                    .to_be_bytes(),
            );
            hasher.update(field.as_bytes());
        }
        {
            let digest = hasher.finish();
            let mut bytes = [0; 32];
            bytes.copy_from_slice(digest.as_ref());
            IdempotencySubjectKey::from_bytes(bytes)
        }
    };

    assert_eq!(
        IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-1")]),
        expected
    );
}

#[test]
fn a_payload_hash_ignores_key_insertion_order_at_every_depth() {
    let one = json!({"b": 1, "a": {"y": 2, "x": 3}});
    let other = json!({"a": {"x": 3, "y": 2}, "b": 1});

    assert_eq!(
        PayloadHash::of_canonical(&one).expect("serializable"),
        PayloadHash::of_canonical(&other).expect("serializable")
    );
}

#[test]
fn a_payload_hash_respects_array_order_which_carries_meaning() {
    let one = json!({"subjects": ["a", "b"]});
    let other = json!({"subjects": ["b", "a"]});

    assert_ne!(
        PayloadHash::of_canonical(&one).expect("serializable"),
        PayloadHash::of_canonical(&other).expect("serializable")
    );
}

#[test]
fn a_payload_hash_distinguishes_the_amount_it_covers() {
    let one = json!({"amount": 1});
    let other = json!({"amount": 2});

    assert_ne!(
        PayloadHash::of_canonical(&one).expect("serializable"),
        PayloadHash::of_canonical(&other).expect("serializable")
    );
}

#[test]
fn an_attribution_digest_is_canonical_in_the_same_way() {
    let one = json!({"metric": "m", "subjects": [{"kind": "tenant", "id": "t-1"}]});
    let other = json!({"subjects": [{"id": "t-1", "kind": "tenant"}], "metric": "m"});

    assert_eq!(
        AttributionDigest::of_canonical(&one).expect("serializable"),
        AttributionDigest::of_canonical(&other).expect("serializable")
    );
    assert_ne!(
        AttributionDigest::of_canonical(&one)
            .expect("serializable")
            .to_hex(),
        AttributionDigest::of_canonical(&json!({"metric": "other"}))
            .expect("serializable")
            .to_hex()
    );
}

// --- consumer requests -----------------------------------------------------

#[test]
fn a_debit_request_ignores_decision_shaped_fields_a_caller_echoed_back() {
    let request: DebitRequest = serde_json::from_value(json!({
        "attribution": {
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "metric": "gts.cf.qe.metric.type.v1~acme.tokens.v1",
            "subjects": [{"kind": "gts.cf.core.qe.scope.v1~cf.qe.scope.tenant.v1", "id": "t-1"}],
            "metadata": {}
        },
        "amount": 5,
        "idempotency_key": "k-1",
        "result": {"outcome": "allowed"},
        "debit_plan": {},
        "diagnostics": {"engine": "noise"}
    }))
    .expect("server-derived fields are ignored, never rejected");

    assert_eq!(request.amount, 5);
    assert_eq!(request.idempotency_key, "k-1");
}

#[test]
fn an_attribution_inside_a_request_keeps_rejecting_its_own_unknown_fields() {
    let error = serde_json::from_value::<DebitRequest>(json!({
        "attribution": {
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "metric": "gts.cf.qe.metric.type.v1~acme.tokens.v1",
            "subjects": [],
            "metadata": {},
            "tenant": "typo"
        },
        "amount": 5,
        "idempotency_key": "k-1"
    }))
    .expect_err("a misspelled attribution field is still an error");

    assert!(error.to_string().contains("tenant"), "{error}");
}

#[test]
fn a_signed_amount_reaches_the_domain_instead_of_failing_deserialization() {
    let request: DebitRequest = serde_json::from_value(json!({
        "attribution": {
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "metric": "gts.cf.qe.metric.type.v1~acme.tokens.v1",
            "subjects": [],
            "metadata": {}
        },
        "amount": -3,
        "idempotency_key": "k-1"
    }))
    .expect("a negative amount parses so the domain can answer INVALID_AMOUNT");

    assert_eq!(request.amount, -3);
    assert_eq!(positive_amount(request.amount), None);
    assert_eq!(positive_amount(0), None);
    assert_eq!(positive_amount(7), Some(7));
    assert_eq!(positive_amount(i64::MAX), Some(9_223_372_036_854_775_807));
}

#[test]
fn a_preview_flattens_the_decision_next_to_its_marker() {
    let preview = DecisionPreview::of(Decision::allowed_with_plan(BTreeMap::new()));

    let value = serde_json::to_value(&preview).expect("serializable");
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();

    assert_eq!(
        keys,
        vec!["debit_plan", "diagnostics", "preview", "result"],
        "the decision is flattened next to the marker, adding no envelope"
    );
    assert_eq!(value["preview"], json!(true));
}

// --- idempotency records ---------------------------------------------------

#[test]
fn a_partial_write_completes_into_the_scope_the_transaction_derived() {
    let partial = PartialIdempotencyWrite {
        tenant_id: TenantId::new(Uuid::from_u128(1)),
        key: "k-1".to_owned(),
        payload_hash: PayloadHash::of_canonical(&json!({"amount": 5})).expect("serializable"),
    };
    let subject_key =
        IdempotencySubjectKey::of(&[subject("gts.cf.core.qe.subj.v1~acme.tenant.v1", "t-1")]);

    let write = partial.clone().complete(subject_key, OperationType::Credit);

    assert_eq!(write.scope.tenant_id, TenantId::new(Uuid::from_u128(1)));
    assert_eq!(write.scope.subject_key, subject_key);
    assert_eq!(write.scope.operation_type, OperationType::Credit);
    assert_eq!(write.scope.key, "k-1");
    assert_eq!(write.payload_hash, partial.payload_hash);
}

#[test]
fn a_versioned_blob_decodes_into_the_decision_it_recorded() {
    let record = IdempotencyRecord {
        scope: IdempotencyScope {
            tenant_id: TenantId::new(Uuid::from_u128(1)),
            subject_key: IdempotencySubjectKey::of(&[]),
            operation_type: OperationType::Debit,
            key: "k-1".to_owned(),
        },
        payload_hash: PayloadHash::of_canonical(&json!({})).expect("serializable"),
        decision_blob: json!({
            "__version": DECISION_BLOB_VERSION,
            "result": {"outcome": "allowed"},
            "debit_plan": {},
            "diagnostics": {}
        }),
        engine_id: None,
        policy_id: None,
        policy_version: None,
        attribution_hash: None,
        created_at: ts(0),
        expires_at: ts(86_400),
    };

    let decision = record.decision().expect("the blob decodes");

    assert_eq!(decision.result, DecisionResult::Allowed);
    assert_eq!(decision.denied_reason(), None);
}

#[test]
fn a_denial_reports_its_reason_and_whether_it_records_anything() {
    let denied = Decision {
        result: DecisionResult::Denied {
            violated_quota_ids: Vec::new(),
            reason: NO_APPLICABLE_QUOTA.to_owned(),
        },
        debit_plan: BTreeMap::new(),
        diagnostics: BTreeMap::new(),
    };

    assert_eq!(denied.denied_reason(), Some(NO_APPLICABLE_QUOTA));
    assert!(denied.is_no_applicable_quota());
    assert!(!Decision::allowed_with_plan(BTreeMap::new()).is_no_applicable_quota());
}

#[test]
fn retention_reports_the_deadline_only_when_something_was_recorded() {
    assert_eq!(
        Retention::Recorded {
            expires_at: ts(86_400)
        }
        .expires_at(),
        Some(ts(86_400))
    );
    assert_eq!(Retention::Unrecorded.expires_at(), None);
}

// --- commit apportionment --------------------------------------------------

/// The reserved amount a plan was acquired under. Never zero: an acquisition
/// rejects a non-positive amount long before it reaches storage.
fn reserved(amount: u64) -> std::num::NonZeroU64 {
    std::num::NonZeroU64::new(amount).expect("a lease reserves a positive amount")
}

#[test]
fn a_commit_charges_the_ceiling_of_its_share_and_never_more_than_a_hold_holds() {
    // A plan need not hold the reserved amount on every Quota, and may hold
    // less than it in total; using one unit still charges one.
    assert_eq!(apportion(&[1], 1, reserved(10)), Ok(vec![1]));
    // Rounding up rather than to nearest: half a unit used is a unit charged.
    assert_eq!(apportion(&[3], 1, reserved(2)), Ok(vec![2]));
    // Whole units split exactly, so no remainder is handed out.
    assert_eq!(apportion(&[10, 10], 6, reserved(10)), Ok(vec![6, 6]));
    assert_eq!(apportion(&[10, 5, 5], 6, reserved(10)), Ok(vec![6, 3, 3]));
    // The unit that rounding creates goes to one hold, not to every hold: the
    // charged total is the ceiling of the whole, not the sum of ceilings.
    assert_eq!(apportion(&[1, 1], 1, reserved(2)), Ok(vec![1, 0]));
    assert_eq!(apportion(&[1, 1, 1], 1, reserved(3)), Ok(vec![1, 0, 0]));
    // Committing everything keeps every hold whole, and committing nothing
    // returns every hold.
    assert_eq!(apportion(&[7, 2], 9, reserved(9)), Ok(vec![7, 2]));
    assert_eq!(apportion(&[7, 2], 0, reserved(9)), Ok(vec![0, 0]));
    assert_eq!(apportion(&[], 5, reserved(5)), Ok(Vec::new()));
}

#[test]
fn the_apportioned_shares_conserve_the_charged_total_for_every_plan() {
    // Exhaustive over small plans: the three invariants the commit path relies
    // on hold for every shape the engine contract permits, not only the ones
    // spelled out above.
    for reserved_amount in 1_u64..8 {
        for actual in 0..=reserved_amount {
            for first in 0_u64..6 {
                for second in 0_u64..6 {
                    for third in 0_u64..6 {
                        let holds = [first, second, third];
                        let kept =
                            apportion(&holds, actual, reserved(reserved_amount)).expect("in range");
                        let total_held: u64 = holds.iter().sum();
                        let expected = u64::try_from(
                            (u128::from(total_held) * u128::from(actual))
                                .div_ceil(u128::from(reserved_amount)),
                        )
                        .expect("a share of the held total fits");
                        assert_eq!(
                            kept.iter().sum::<u64>(),
                            expected,
                            "holds {holds:?} of {reserved_amount}, committing {actual}"
                        );
                        for (kept, held) in kept.iter().zip(&holds) {
                            assert!(
                                kept <= held,
                                "a hold is never charged more than it holds: {holds:?}"
                            );
                        }
                        assert_eq!(
                            actual > 0 && total_held > 0,
                            kept.iter().sum::<u64>() > 0,
                            "any use charges a unit, and no use charges none: {holds:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn apportionment_refuses_an_over_commit_and_reports_overflow_instead_of_panicking() {
    assert_eq!(
        apportion(&[10], 11, reserved(10)),
        Err(crate::models::ApportionError::OverCommit)
    );
    // The product of two `u64::MAX`-sized values leaves `u128`; the caller
    // learns that rather than losing the transaction to a panic.
    assert_eq!(
        apportion(&[u64::MAX, u64::MAX], u64::MAX, reserved(u64::MAX)),
        Err(crate::models::ApportionError::Overflow)
    );
}

#[test]
fn a_rollback_names_the_namespace_of_the_operation_it_reverses() {
    // A debit and a lease commit can hold the same key over the same subjects,
    // so the selector is what tells them apart.
    assert_eq!(
        RollbackableOperation::Debit.operation_type(),
        OperationType::Debit
    );
    assert_eq!(
        RollbackableOperation::LeaseCommit.operation_type(),
        OperationType::Commit
    );
    assert_eq!(
        RollbackableOperation::default(),
        RollbackableOperation::Debit
    );
    // A request written before leases existed still means a debit.
    let legacy: crate::models::RollbackRequest = serde_json::from_value(json!({
        "attribution": {
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "metric": "gts.cf.qe.metric.type.v1~acme.tokens.v1",
            "subjects": [],
            "metadata": {}
        },
        "original_idempotency_key": "k-0",
        "idempotency_key": "k-1"
    }))
    .expect("the selector defaults");
    assert_eq!(legacy.original_operation, RollbackableOperation::Debit);
}

#[test]
fn a_batch_is_denied_when_any_item_is_and_keeps_every_item_in_order() {
    let allowed = crate::Decision::allowed_with_plan(std::collections::BTreeMap::new());
    let denied = crate::Decision {
        result: crate::DecisionResult::Denied {
            violated_quota_ids: Vec::new(),
            reason: "QUOTA_EXCEEDED".to_owned(),
        },
        debit_plan: std::collections::BTreeMap::new(),
        diagnostics: std::collections::BTreeMap::new(),
    };
    let keys = || ["a".to_owned(), "b".to_owned(), "c".to_owned()];

    let mixed = crate::BatchDecision::of(keys(), vec![allowed.clone(), denied, allowed.clone()]);
    assert_eq!(mixed.result, crate::BatchResult::Denied);
    let order: Vec<&str> = mixed
        .items
        .iter()
        .map(|i| i.idempotency_key.as_str())
        .collect();
    assert_eq!(order, ["a", "b", "c"]);

    let clean = crate::BatchDecision::of(keys(), vec![allowed.clone(), allowed.clone(), allowed]);
    assert_eq!(clean.result, crate::BatchResult::Allowed);
}

#[test]
fn a_batch_record_carries_its_schema_version() {
    let record = crate::BatchRecord::new(Vec::new());
    let value = serde_json::to_value(&record).expect("serialize");
    assert_eq!(
        value["__version"],
        serde_json::json!(crate::BatchRecord::VERSION)
    );
    let back: crate::BatchRecord = serde_json::from_value(value).expect("deserialize");
    assert_eq!(back, record);
}

#[test]
fn a_batch_timer_is_armed_once_and_shared_by_every_attempt() {
    let timer = crate::BatchTimer::new(std::time::Duration::from_mins(1));
    assert!(!timer.expired(), "an unarmed timer has not run out");
    assert_eq!(
        timer.armed_remaining(),
        None,
        "an unarmed timer bounds nothing"
    );
    let first = timer.arm();
    assert!(timer.armed_remaining().is_some_and(|left| left <= first));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = timer.arm();
    assert!(
        second < first,
        "the second attempt gets what is left, not a fresh budget"
    );

    let spent = crate::BatchTimer::new(std::time::Duration::ZERO);
    assert!(spent.arm().is_zero());
    assert!(spent.expired());
    assert_eq!(spent.armed_remaining(), Some(std::time::Duration::ZERO));
}

//! Judging one capability record (P-D-268) against a registry a test adapter seeded: shape,
//! identity, units, policies and evidence, each refusal with its code and field, an outage never a
//! refusal, and one registry answer per entity for a whole run.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::quota_test_support::{
    ALLOCATED_VCPU, BYTES_UNIT, DERIVED_CAPABILITY, FakeQuotaRegistry, GB_UNIT, GIB_UNIT,
    REGIONAL_POOL, TENANT_TYPE, VCPU_METRIC, VCPU_UNIT, VM_TYPE, ram_record_in, seed_catalog,
    vcpu_record,
};
use crate::test_support::authed_ctx;
use bss_products_sdk::quota::gts::{
    ACCOUNTING_POLICY_TYPE, METRIC_TYPE, SCOPE_POLICY_TYPE, UNIT_TYPE,
};
use serde_json::json;

async fn judge(
    registry: &FakeQuotaRegistry,
    raw: serde_json::Value,
) -> Result<JudgedRecord, JudgeError> {
    judge_record(
        registry,
        &authed_ctx(uuid::Uuid::new_v4()),
        &raw,
        &mut Resolutions::default(),
    )
    .await
}

/// One change that takes a record outside the v1 shape.
type Change = Box<dyn Fn(&mut serde_json::Value)>;

fn refused(result: Result<JudgedRecord, JudgeError>) -> RecordRefusal {
    match result {
        Err(JudgeError::Refused(r)) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_valid_record_is_judged_with_its_evidence() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let judged = judge(&registry, vcpu_record()).await.unwrap();
    for id in [
        "gts.cf.bss.products.quota_capability.v1~",
        "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1",
        VCPU_UNIT,
        "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated_vcpu.v1",
        "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1",
        "gts.cf.core.compute.tenant.v1~",
        "gts.cf.core.compute.vm.v1~",
    ] {
        let e = judged.evidence.0.get(id).expect(id);
        assert_eq!(e.digest.len(), 64);
        assert!(e.resource_version >= 1);
        assert_eq!(e.uuid, gts::GtsId::try_new(id).unwrap().to_uuid());
    }
    assert_eq!(judged.evidence.0.len(), 7);
    assert_eq!(
        judged.record_digest,
        bss_products_sdk::quota::digest::record_digest(&vcpu_record())
    );
    assert_eq!(judged.raw, vcpu_record());
    assert_eq!(judged.metric.id.as_ref(), VCPU_METRIC);
    assert_eq!(judged.canonical_unit, judged.unit);
    let round_trip = EvidenceSet::from_json(&judged.evidence.to_json()).unwrap();
    assert_eq!(round_trip, judged.evidence);
}

#[tokio::test]
async fn an_unregistered_metric_is_unknown() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["metric_ref"] =
        json!("gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.nothing.v1");
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_UNKNOWN", "metric_ref")
    );
}

#[tokio::test]
async fn a_unit_named_as_the_metric_is_a_family_refusal() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["metric_ref"] = json!(VCPU_UNIT);
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_FAMILY", "metric_ref")
    );
}

#[tokio::test]
async fn a_minor_bearing_reference_is_refused() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["unit_ref"] = json!("gts.cf.bss.products.quota_unit.v1.1~cf.compute.quota_adapter.vcpu.v1");
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_VERSION_UNSUPPORTED", "unit_ref")
    );
}

#[tokio::test]
async fn a_minor_bearing_adapter_instance_is_refused() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    for (field, reference) in [
        (
            "unit_ref",
            "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1.1",
        ),
        (
            "metric_ref",
            "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1.2",
        ),
        (
            "scope_policy_ref",
            "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1.1",
        ),
    ] {
        let mut raw = vcpu_record();
        raw[field] = json!(reference);
        let r = refused(judge(&registry, raw).await);
        assert_eq!(
            (r.code, r.field.as_str()),
            ("QUOTA_VERSION_UNSUPPORTED", field)
        );
    }
    let mut raw = vcpu_record();
    raw["type_id"] = json!(
        DERIVED_CAPABILITY.replace("allocation_capability.v1~", "allocation_capability.v1.1~")
    );
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_VERSION_UNSUPPORTED", "type_id")
    );
}

#[tokio::test]
async fn a_v2_family_is_unsupported() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["type_id"] = json!("gts.cf.bss.products.quota_capability.v2~");
    assert_eq!(
        refused(judge(&registry, raw).await).code,
        "QUOTA_VERSION_UNSUPPORTED"
    );
}

#[tokio::test]
async fn a_type_id_outside_the_capability_family_is_refused() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    for type_id in [UNIT_TYPE, "gts.cf.core.compute.vm.v1~"] {
        let mut raw = vcpu_record();
        raw["type_id"] = json!(type_id);
        let r = refused(judge(&registry, raw).await);
        assert_eq!(
            (r.code, r.field.as_str()),
            ("QUOTA_REFERENCE_FAMILY", "type_id"),
            "{type_id}"
        );
    }
}

#[tokio::test]
async fn a_metric_that_is_not_allocation_is_unsupported() {
    let id = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.trial_credit.v1";
    let registry = seed_catalog(FakeQuotaRegistry::default()).with_instance(
        id,
        METRIC_TYPE,
        json!({"id": id, "name": "Trial credit", "display_name": {"en": "Credit"},
               "description": {"en": "Credit"}, "quota_kind": "monetary",
               "canonical_unit_ref": VCPU_UNIT, "properties": {}}),
    );
    let mut raw = vcpu_record();
    raw["metric_ref"] = json!(id);
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_CAPABILITY_UNSUPPORTED", "metric_ref")
    );
}

#[tokio::test]
async fn a_metric_without_english_text_is_refused() {
    let id = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.ru_only.v1";
    let registry = seed_catalog(FakeQuotaRegistry::default()).with_instance(
        id,
        METRIC_TYPE,
        json!({"id": id, "name": "x", "display_name": {"ru": "x"}, "description": {"ru": "x"},
               "quota_kind": "allocation", "canonical_unit_ref": VCPU_UNIT, "properties": {}}),
    );
    let mut raw = vcpu_record();
    raw["metric_ref"] = json!(id);
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_DISPLAY_TEXT_MISSING", "metric_ref.display_name")
    );
}

#[tokio::test]
async fn a_zero_step_is_refused() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["limits"]["step"] = json!("0");
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_RECORD_INVALID", "limits.step")
    );
}

#[tokio::test]
async fn the_record_unit_and_the_metric_must_share_a_canonical_unit() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record(); // a vCPU metric
    raw["unit_ref"] = json!(GIB_UNIT); // a bytes-family unit
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_UNIT_MISMATCH", "unit_ref")
    );
}

#[tokio::test]
async fn a_registry_outage_is_unavailable_not_a_refusal() {
    let registry = seed_catalog(FakeQuotaRegistry::default()).outage();
    assert!(matches!(
        judge(&registry, vcpu_record()).await,
        Err(JudgeError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_derived_capability_schema_with_validated_properties_is_accepted() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let mut raw = vcpu_record();
    raw["type_id"] = json!(DERIVED_CAPABILITY);
    raw["properties"] = json!({"reviewed_by": "adapter-owner"});
    let judged = judge(&registry, raw).await.unwrap();
    assert!(judged.evidence.0.contains_key(DERIVED_CAPABILITY));
    assert!(
        judged
            .evidence
            .0
            .contains_key(bss_products_sdk::quota::gts::CAPABILITY_TYPE)
    );
    let invalid = seed_catalog(FakeQuotaRegistry::default()).invalid_for(
        DERIVED_CAPABILITY,
        "properties.reviewed_by must be a string",
    );
    let mut bad = vcpu_record();
    bad["type_id"] = json!(DERIVED_CAPABILITY);
    bad["properties"] = json!({"reviewed_by": 1});
    let r = refused(judge(&invalid, bad).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_RECORD_INVALID", "properties")
    );
    assert!(r.detail.contains("reviewed_by"), "{}", r.detail);
}

#[tokio::test]
async fn an_abstract_subject_type_is_a_family_refusal() {
    let registry = seed_catalog(FakeQuotaRegistry::default()).abstract_type(TENANT_TYPE);
    let r = refused(judge(&registry, vcpu_record()).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_FAMILY", "subject_type_ref")
    );
}

#[tokio::test]
async fn one_run_asks_the_registry_once_per_entity() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let ctx = authed_ctx(uuid::Uuid::new_v4());
    let mut memo = Resolutions::default();
    judge_record(&registry, &ctx, &vcpu_record(), &mut memo)
        .await
        .unwrap();
    let after_one = registry.calls();
    let mut second = vcpu_record();
    second["id"] = json!(uuid::Uuid::new_v4().to_string());
    second["capability_key"] = json!("tenant-vcpu-2");
    judge_record(&registry, &ctx, &second, &mut memo)
        .await
        .unwrap();
    assert_eq!(
        registry.calls(),
        after_one + 1,
        "only the second record's schema validation is new"
    );
}

/// A record whose unit is not the metric's canonical one keeps the canonical unit as evidence
/// too, and a unit whose canonical unit refers onward is invalid.
#[tokio::test]
async fn a_unit_converts_directly_into_a_canonical_unit_that_refers_to_itself() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let judged = judge(&registry, ram_record_in(GIB_UNIT)).await.unwrap();
    assert!(judged.evidence.0.contains_key(BYTES_UNIT));
    assert_eq!(judged.canonical_unit.id.as_ref(), BYTES_UNIT);
    let chained = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.kib.v1";
    let registry = seed_catalog(FakeQuotaRegistry::default())
        .with_instance(
            chained,
            UNIT_TYPE,
            json!({"id": chained, "symbol": "KiB", "canonical_unit_ref": GB_UNIT,
                   "numerator": "1024", "denominator": "1"}),
        )
        .with_instance(
            "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.chained.v1",
            METRIC_TYPE,
            json!({"id": "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.chained.v1",
                   "name": "x", "display_name": {"en": "x"}, "description": {"en": "x"},
                   "quota_kind": "allocation", "canonical_unit_ref": GB_UNIT, "properties": {}}),
        );
    let mut raw = ram_record_in(chained);
    raw["metric_ref"] =
        json!("gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.chained.v1");
    assert_eq!(
        refused(judge(&registry, raw).await).code,
        "QUOTA_UNIT_INVALID"
    );
    let reducible = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.half.v1";
    let registry = seed_catalog(FakeQuotaRegistry::default()).with_instance(
        reducible,
        UNIT_TYPE,
        json!({"id": reducible, "symbol": "x", "canonical_unit_ref": BYTES_UNIT,
               "numerator": "2", "denominator": "4"}),
    );
    let r = refused(judge(&registry, ram_record_in(reducible)).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_UNIT_INVALID", "unit_ref")
    );
}

/// A policy outside its v1 meaning, without English text or with a repeated dimension is
/// `QUOTA_POLICY_INVALID`; a policy of another family is a family refusal.
#[tokio::test]
async fn a_policy_outside_its_v1_meaning_is_refused() {
    let used = json!({"id": ALLOCATED_VCPU, "quantity_basis": "used_capacity", "aggregation": "sum",
                      "description": {"en": "x"}});
    let untitled = json!({"id": ALLOCATED_VCPU, "quantity_basis": "allocated_capacity",
                          "aggregation": "sum", "description": {"ru": "x"}});
    for content in [used, untitled] {
        let registry = seed_catalog(FakeQuotaRegistry::default()).with_instance(
            ALLOCATED_VCPU,
            ACCOUNTING_POLICY_TYPE,
            content,
        );
        let r = refused(judge(&registry, vcpu_record()).await);
        assert_eq!(r.code, "QUOTA_POLICY_INVALID");
        assert!(r.field.starts_with("accounting_policy_ref"), "{}", r.field);
    }
    let twice = json!({"id": REGIONAL_POOL, "subject_selector": "resource_owner",
                       "aggregation": "single_verified_scope",
                       "required_dimensions": ["region", "region"]});
    let registry = seed_catalog(FakeQuotaRegistry::default()).with_instance(
        REGIONAL_POOL,
        SCOPE_POLICY_TYPE,
        twice,
    );
    let r = refused(judge(&registry, vcpu_record()).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_POLICY_INVALID", "scope_policy_ref")
    );
    let mut raw = vcpu_record();
    raw["scope_policy_ref"] =
        json!(ALLOCATED_VCPU.replace("quota_accounting_policy", "quota_scope_policy"));
    let r = refused(judge(&seed_catalog(FakeQuotaRegistry::default()), raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_UNKNOWN", "scope_policy_ref")
    );
    let mut raw = vcpu_record();
    raw["scope_policy_ref"] = json!(ALLOCATED_VCPU);
    let r = refused(judge(&seed_catalog(FakeQuotaRegistry::default()), raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_FAMILY", "scope_policy_ref")
    );
}

/// The shape is the closed v1 capability: an unknown member, a missing one, a malformed bound or
/// reference, `min` above `max`, an over-long key or a repeated resource type is invalid on its
/// field.
#[tokio::test]
async fn a_record_outside_the_v1_shape_is_invalid_on_its_field() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let cases: Vec<(&str, Change)> = vec![
        ("paid", Box::new(|r| r["paid"] = json!(true))),
        (
            "operations",
            Box::new(|r| {
                r.as_object_mut().unwrap().remove("operations");
            }),
        ),
        (
            "operations.resume",
            Box::new(|r| {
                r["operations"].as_object_mut().unwrap().remove("resume");
            }),
        ),
        ("limits.min", Box::new(|r| r["limits"]["min"] = json!("01"))),
        ("limits.max", Box::new(|r| r["limits"]["max"] = json!(null))),
        (
            "limits.max",
            Box::new(|r| {
                r["limits"]["min"] = json!("10");
                r["limits"]["max"] = json!("9");
            }),
        ),
        (
            "metric_ref",
            Box::new(|r| r["metric_ref"] = json!("not a gts id")),
        ),
        ("type_id", Box::new(|r| r["type_id"] = json!(VCPU_UNIT))),
        (
            "capability_key",
            Box::new(|r| r["capability_key"] = json!("k".repeat(256))),
        ),
        ("mapping_key", Box::new(|r| r["mapping_key"] = json!(""))),
        (
            "resource_type_refs",
            Box::new(|r| r["resource_type_refs"] = json!([VM_TYPE, VM_TYPE])),
        ),
        (
            "resource_type_refs",
            Box::new(|r| r["resource_type_refs"] = json!([])),
        ),
        ("id", Box::new(|r| r["id"] = json!("not-a-uuid"))),
    ];
    for (field, change) in cases {
        let mut raw = vcpu_record();
        change(&mut raw);
        let r = refused(judge(&registry, raw).await);
        assert_eq!((r.code, r.field.as_str()), ("QUOTA_RECORD_INVALID", field));
    }
    assert_eq!(
        registry.calls(),
        0,
        "a record outside the shape reaches no registry"
    );
}

/// The platform's own registry, seeded as a test adapter registers its entities, behind the
/// production adapter.
fn platform_registry() -> crate::infra::quota::registry::TypesRegistryQuotaRegistry {
    let mock = std::sync::Arc::new(types_registry_sdk::testing_platform::MockTypesRegistry::new());
    crate::quota_test_support::seed_mock(&mock);
    let hub = std::sync::Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn types_registry_sdk::TypesRegistryApi>(mock);
    crate::infra::quota::registry::TypesRegistryQuotaRegistry::new(hub)
}

/// Through the platform's registry, whose capability schema narrows every reference by pattern:
/// the package's example record is valid end to end, and a unit named as the metric answers its
/// family refusal on `metric_ref`, not the schema's generic invalidity, because identity resolves
/// every reference before it asks for schema validation.
#[tokio::test]
async fn the_platform_registry_judges_references_before_the_schema() {
    let registry = platform_registry();
    let ctx = authed_ctx(uuid::Uuid::new_v4());
    let judged = judge_record(&registry, &ctx, &vcpu_record(), &mut Resolutions::default())
        .await
        .unwrap();
    assert_eq!(judged.metric.id.as_ref(), VCPU_METRIC);
    let mut raw = vcpu_record();
    raw["metric_ref"] = json!(VCPU_UNIT);
    let r = refused(judge_record(&registry, &ctx, &raw, &mut Resolutions::default()).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_REFERENCE_FAMILY", "metric_ref")
    );
}

/// A live capability schema that does not compile rejects the record that names it: the record is
/// invalid on `type_id`, the detail names the schema, and it is no registry outage.
#[tokio::test]
async fn a_capability_schema_that_does_not_compile_rejects_the_record() {
    let registry = seed_catalog(FakeQuotaRegistry::default())
        .uncompilable(DERIVED_CAPABILITY, "a pattern is not a regular expression");
    let mut raw = vcpu_record();
    raw["type_id"] = json!(DERIVED_CAPABILITY);
    let r = refused(judge(&registry, raw).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_RECORD_INVALID", "type_id")
    );
    assert!(r.detail.contains(DERIVED_CAPABILITY), "{}", r.detail);
    assert!(r.detail.contains("does not compile"), "{}", r.detail);
}

/// The metric's canonical unit must name itself, as every reference does: a unit instance whose
/// content names another id is not the canonical unit the metric counts in.
#[tokio::test]
async fn a_canonical_unit_that_does_not_name_itself_is_invalid() {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    registry.bump(
        BYTES_UNIT,
        json!({"id": GB_UNIT, "symbol": "B", "canonical_unit_ref": BYTES_UNIT,
               "numerator": "1", "denominator": "1"}),
    );
    let r = refused(judge(&registry, ram_record_in(GIB_UNIT)).await);
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_UNIT_INVALID", "metric_ref")
    );
}

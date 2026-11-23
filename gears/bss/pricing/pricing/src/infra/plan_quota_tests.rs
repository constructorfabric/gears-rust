//! The quota catalog port (D-542, D-545): the catalog is resolved per call and answers on a task
//! of its own, an outage is 503 and a refusal is forwarded, the SKU versions are read once per SKU,
//! the request carries each item's SKU version and no quantity at authoring, the stored evidence
//! keeps identity without text or availability, a stored rule converts both ways and a tampered
//! one is a corrupt row, and a capture reads its set in one read transaction and calls Products
//! outside it, while its check inside the unit's transaction sees a moved set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
use super::*;
use crate::infra::storage::entity::{plan as plan_e, plan_item, price_book};
use crate::infra::storage::repo::{book_repo, plan_repo};
use crate::test_support::{TestDsn, at, test_db};
use bss_products_sdk::models::{
    ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuContent, SkuType, SkuVersion,
};
use bss_products_sdk::quota::catalog::{QuotaDefinitionListRequest, QuotaDefinitionPage};
use bss_products_sdk::quota::validation::{QuotaCheck, QuotaCheckCode, QuotaRuleVerdict};
use bss_products_sdk::{PricingReferenceRegistry, ReferenceRegistryV1};
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_canonical_errors::Http;
use toolkit_db::{DBProvider, DbError};

const VCPU_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1";
const SKU: Uuid = Uuid::from_u128(0x5);
const OTHER_SKU: Uuid = Uuid::from_u128(0x6);
const DEF: Uuid = Uuid::from_u128(0xd1);
const VCPU_METRIC: &str = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1";
const ALLOCATED_VCPU: &str =
    "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated_vcpu.v1";
const REGIONAL_POOL: &str =
    "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1";

fn user_in(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x0e))
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap()
}
fn user() -> SecurityContext {
    user_in(Uuid::from_u128(0x7e))
}

/// An enabled vCPU definition approved at SKU version 3, as Products answers it.
fn sample_definition() -> QuotaDefinition {
    serde_json::from_value(json!({
        "definition_id": DEF, "version": "1", "definition_digest": "a".repeat(64),
        "sku_id": SKU, "sku_version": "3", "metric_ref": VCPU_METRIC,
        "metric_digest": "b".repeat(64), "quota_kind": "allocation", "display_name": "vCPU",
        "description": "Virtual CPUs", "locale": "en", "canonical_unit_ref": VCPU_UNIT,
        "allowed_units": [{"unit_ref": VCPU_UNIT, "digest": "c".repeat(64), "symbol": "vCPU",
            "numerator": "1", "denominator": "1", "min": "0", "step": "1", "max": "1024"}],
        "subject_type_ref": "gts.cf.core.compute.tenant.v1~",
        "resource_type_refs": ["gts.cf.core.compute.vm.v1~"],
        "mapping_ref": {"id": Uuid::from_u128(0xa1), "version": "1", "digest": "d".repeat(64)},
        "profile_ref": {"id": Uuid::from_u128(0xb1), "version": "2", "digest": "e".repeat(64)},
        "qualification_ref": null,
        "accounting_policy_ref": ALLOCATED_VCPU,
        "accounting_policy_digest": "2".repeat(64),
        "accounting_policy": {"id": ALLOCATED_VCPU,
            "quantity_basis": "allocated_capacity", "aggregation": "sum",
            "description": {"en": "Counts allocated capacity."}},
        "scope_policy_ref": REGIONAL_POOL,
        "scope_policy_digest": "3".repeat(64),
        "scope_policy": {"id": REGIONAL_POOL,
            "subject_selector": "resource_owner", "aggregation": "single_verified_scope",
            "required_dimensions": ["region", "resource_pool"]},
        "change_support": {"initial_set": true, "increase": false},
        "profile_conditions": "Counts stopped VMs.", "supported_bases": ["Fixed", "PerPlanUnit"],
        "availability": {"state": "disabled", "revision": "1",
            "observed_at": "2026-11-21T08:30:00Z", "reason_code": "qualification_pending"}
    }))
    .unwrap()
}

fn sample_rule(item: Uuid) -> PlanQuotaRuleV1 {
    sample_rule_n(1, item, "10")
}
fn sample_rule_n(n: u128, item: Uuid, amount: &str) -> PlanQuotaRuleV1 {
    PlanQuotaRuleV1 {
        rule_id: Uuid::from_u128(n),
        definition_ref: QuotaDefinitionRef {
            id: DEF,
            version: std::num::NonZeroU64::MIN,
            digest: [0xaa; 32],
        },
        item_id: item,
        dimension_value: None,
        logical_binding_key: "compute".into(),
        subject_selector: QuotaSubjectSelector::ResourceOwner,
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: VCPU_UNIT.into(),
        basis: PlanQuotaBasis::PerPlanUnit,
    }
}

fn empty_request() -> QuotaSetRequest {
    set_request(&[], &[], &BTreeMap::new(), Purpose::Authoring)
}

/// A catalog double: it fails every call with `status`, or answers each rule with the sample
/// definition. It records each request, the task it ran on and whether it could open a
/// connection to `db` (toolkit-db refuses one on a task inside a transaction).
#[derive(Default)]
struct Catalog {
    status: Option<u16>,
    /// The call panics, as a call that does not finish does.
    panics: bool,
    db: Option<DBProvider<DbError>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<QuotaSetRequest>>,
    tasks: Mutex<Vec<Option<tokio::task::Id>>>,
}
fn refusal(status: u16) -> CanonicalError {
    use crate::infra::pricing_reads::PlanResource;
    match status {
        400 => crate::api::rest::authoring::support::invalid("rules[0].unit_ref", "VALIDATION"),
        403 => crate::api::rest::authoring::support::forbidden("QUOTA_DEFINITION_READ_DENIED"),
        409 => PlanResource::aborted("contended")
            .with_reason("CONTENDED")
            .create(),
        429 => PlanResource::resource_exhausted("rate limited")
            .with_quota_violation("requests", "too many requests")
            .create(),
        500 => CanonicalError::internal("a stored row does not read").create(),
        other => CanonicalError::service_unavailable()
            .with_override(Http::status_code(other))
            .create(),
    }
}
#[async_trait::async_trait]
impl ProductsQuotaCatalogClientV1 for Catalog {
    async fn list_quota_definitions(
        &self,
        _: &SecurityContext,
        _: QuotaDefinitionListRequest,
    ) -> Result<QuotaDefinitionPage, CanonicalError> {
        Err(refusal(self.status.unwrap_or(503)))
    }
    async fn get_quota_definition(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: QuotaVersion,
        _: Option<&str>,
    ) -> Result<QuotaDefinition, CanonicalError> {
        Err(refusal(self.status.unwrap_or(503)))
    }
    async fn validate_quota_set(
        &self,
        _: &SecurityContext,
        request: QuotaSetRequest,
    ) -> Result<QuotaSetValidation, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.tasks.lock().unwrap().push(tokio::task::try_id());
        assert!(!self.panics, "the catalog call does not finish");
        if let Some(db) = &self.db
            && db.conn().is_err()
        {
            return Err(CanonicalError::internal("called inside a transaction").create());
        }
        self.requests.lock().unwrap().push(request.clone());
        if let Some(status) = self.status {
            return Err(refusal(status));
        }
        Ok(QuotaSetValidation {
            rules: request
                .rules
                .iter()
                .map(|rule| QuotaRuleVerdict {
                    rule_id: rule.rule_id,
                    definition: Some(sample_definition()),
                    limit: None,
                })
                .collect(),
            checks: vec![QuotaCheck {
                code: QuotaCheckCode::QualificationUnavailable,
                rule_ids: request.rules.iter().map(|rule| rule.rule_id).collect(),
                field: "definition_ref".into(),
                detail: "disabled: qualification_pending".into(),
            }],
        })
    }
}

#[tokio::test]
async fn an_absent_catalog_is_503_quota_catalog_unavailable() {
    let hub = toolkit::ClientHub::default();
    let error = catalog(&hub).err().expect("no catalog registered");
    assert_eq!(error.status_code(), 503);
    assert!(
        error.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{error}"
    );
    let error = validate(&hub, &user(), empty_request()).await.unwrap_err();
    assert!(
        error.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_catalog_outage_is_503_and_a_refusal_is_forwarded() {
    for (status, answered) in [
        (503, 503),
        (502, 503),
        (504, 503),
        (429, 503),
        (409, 503),
        (400, 400),
        (403, 403),
        (500, 500),
    ] {
        let hub = toolkit::ClientHub::default();
        hub.register::<dyn ProductsQuotaCatalogClientV1>(Arc::new(Catalog {
            status: Some(status),
            ..Catalog::default()
        }));
        let error = validate(&hub, &user(), empty_request()).await.unwrap_err();
        assert_eq!(error.status_code(), answered, "{status}: {error}");
        assert_eq!(
            error.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
            answered == 503,
            "{status}: {error}"
        );
    }
    let refused = refusal(400);
    let hub = toolkit::ClientHub::default();
    hub.register::<dyn ProductsQuotaCatalogClientV1>(Arc::new(Catalog {
        status: Some(400),
        ..Catalog::default()
    }));
    let forwarded = validate(&hub, &user(), empty_request()).await.unwrap_err();
    assert_eq!(
        (forwarded.status_code(), forwarded.to_string()),
        (refused.status_code(), refused.to_string()),
        "a refusal is forwarded as Products gave it"
    );
}

/// A rate limit and a contention conflict are unavailability, as the reference work judges them;
/// a conflict with another reason settles the call and is forwarded.
#[test]
fn a_rate_limit_or_a_contention_conflict_is_an_outage() {
    use crate::infra::pricing_reads::PlanResource;
    let conflict = |reason: &str| {
        PlanResource::aborted("conflict")
            .with_reason(reason)
            .create()
    };
    for (error, answered) in [
        (refusal(429), 503),
        (conflict("CONTENDED"), 503),
        (conflict("UNIT_CONTENDED"), 503),
        (conflict("QUOTA_DEFINITION_CHANGED"), 409),
        (refusal(403), 403),
    ] {
        let shown = error.to_string();
        let mapped = outage(error);
        assert_eq!(mapped.status_code(), answered, "{shown}: {mapped}");
        assert_eq!(
            mapped.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
            answered == 503,
            "{shown}: {mapped}"
        );
    }
}

/// A catalog call that does not finish (it panicked on its own task) is 503
/// `QUOTA_CATALOG_UNAVAILABLE`.
#[tokio::test]
async fn a_catalog_call_that_does_not_finish_is_503() {
    let hub = toolkit::ClientHub::default();
    hub.register::<dyn ProductsQuotaCatalogClientV1>(Arc::new(Catalog {
        panics: true,
        ..Catalog::default()
    }));
    let error = validate(&hub, &user(), empty_request()).await.unwrap_err();
    assert_eq!(error.status_code(), 503, "{error}");
    assert!(
        error.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{error}"
    );
}

#[tokio::test]
async fn the_catalog_answers_on_a_task_of_its_own_even_inside_a_transaction() {
    let (db, _, _, _dsn) = test_db().await;
    let catalog = Arc::new(Catalog {
        db: Some(db.clone()),
        ..Catalog::default()
    });
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<dyn ProductsQuotaCatalogClientV1>(catalog.clone());
    let caller = tokio::task::try_id();
    let in_tx = hub.clone();
    let answer = support::transaction_door_read(&db.db(), move |_tx| {
        let hub = in_tx.clone();
        Box::pin(async move {
            Ok(validate(&hub, &user(), empty_request())
                .await
                .map(|validation| validation.rules.len()))
        })
    })
    .await
    .unwrap();
    assert_eq!(answer.unwrap(), 0, "the catalog opened its own connection");
    let tasks = catalog.tasks.lock().unwrap().clone();
    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0].is_some() && tasks[0] != caller,
        "{tasks:?} vs {caller:?}"
    );
}

#[test]
fn the_set_request_carries_each_items_sku_version() {
    let items = [
        RuleItem {
            id: Uuid::from_u128(0xa),
            sku_id: SKU,
            dimension_key: None,
        },
        RuleItem {
            id: Uuid::from_u128(0xb),
            sku_id: OTHER_SKU,
            dimension_key: None,
        },
    ];
    let mut on_b = sample_rule_n(2, Uuid::from_u128(0xb), "4");
    on_b.dimension_value = Some("eu".into());
    on_b.basis = PlanQuotaBasis::Fixed;
    let rules = vec![
        sample_rule(Uuid::from_u128(0xa)),
        sample_rule_n(3, Uuid::from_u128(0xf), "1"),
        on_b,
    ];
    let request = set_request(
        &rules,
        &items,
        &BTreeMap::from([(SKU, 3)]),
        Purpose::Authoring,
    );
    assert_eq!(
        request.rules.len(),
        1,
        "the foreign item's rule and the SKU without a version are Pricing's own checks"
    );
    let sent = &request.rules[0];
    assert_eq!((sent.sku_id, sent.sku_version.get()), (SKU, 3));
    assert_eq!(sent.basis, QuotaBasis::PerPlanUnit);
    assert_eq!(sent.definition.id, DEF);
    assert_eq!(sent.definition.version.get(), 1);
    assert_eq!(
        sent.definition.digest,
        "a".repeat(64),
        "lowercase hex, as Products writes it"
    );
    assert_eq!(sent.subject_selector, "resource_owner");
    assert_eq!(sent.scope_selection, rules[0].scope_selection);
    assert_eq!(
        (sent.amount.as_str(), sent.unit_ref.as_str()),
        ("10", VCPU_UNIT)
    );
    assert_eq!(request.purpose, QuotaValidationPurpose::Authoring);
    assert_eq!(request.change_kind, QuotaChangeKind::InitialSet);
    assert_eq!(request.quantity, None, "no quantity at authoring");
    assert!(request.prior.is_empty() && request.locale.is_none());

    let both = set_request(
        &rules,
        &items,
        &BTreeMap::from([(SKU, 3), (OTHER_SKU, 7)]),
        Purpose::Acceptance {
            quantity: "2".into(),
        },
    );
    assert_eq!(both.rules.len(), 2);
    assert_eq!(both.rules[1].alternative.as_deref(), Some("eu"));
    assert_eq!(both.rules[1].basis, QuotaBasis::Fixed);
    assert_eq!(both.rules[1].sku_version.get(), 7);
    assert_eq!(
        (both.purpose, both.quantity.as_deref()),
        (QuotaValidationPurpose::Acceptance, Some("2")),
        "an acceptance names its quantity"
    );
    let zero = set_request(
        &rules,
        &items,
        &BTreeMap::from([(SKU, 0)]),
        Purpose::Authoring,
    );
    assert!(zero.rules.is_empty(), "a version below 1 is no version");
}

#[test]
fn the_evidence_keeps_identity_and_no_text_or_availability() {
    let d = sample_definition();
    let evidence = evidence_json(&d, VCPU_UNIT);
    assert_eq!(evidence["definition_digest"], d.definition_digest);
    assert_eq!(evidence["unit"]["unit_ref"], VCPU_UNIT);
    assert!(evidence.get("display_name").is_none() && evidence.get("availability").is_none());
    assert_eq!(
        evidence,
        json!({
            "definition_id": DEF, "version": "1", "definition_digest": "a".repeat(64),
            "sku_id": SKU, "sku_version": "3", "metric_ref": VCPU_METRIC,
            "metric_digest": "b".repeat(64), "quota_kind": "allocation",
            "canonical_unit_ref": VCPU_UNIT,
            "unit": {"unit_ref": VCPU_UNIT, "digest": "c".repeat(64), "numerator": "1",
                "denominator": "1", "min": "0", "step": "1", "max": "1024"},
            "mapping_ref": {"id": Uuid::from_u128(0xa1), "version": "1", "digest": "d".repeat(64)},
            "profile_ref": {"id": Uuid::from_u128(0xb1), "version": "2", "digest": "e".repeat(64)},
            "qualification_ref": null,
            "accounting_policy_ref": ALLOCATED_VCPU, "accounting_policy_digest": "2".repeat(64),
            "scope_policy_ref": REGIONAL_POOL, "scope_policy_digest": "3".repeat(64),
            "change_support": {"initial_set": true, "increase": false},
            "subject_type_ref": "gts.cf.core.compute.tenant.v1~",
            "resource_type_refs": ["gts.cf.core.compute.vm.v1~"]
        }),
        "identity, digests and references only: no text, no policy content, no availability"
    );
    assert_eq!(evidence_json(&d, "gts.other.unit")["unit"], Value::Null);
    assert_eq!(
        availability_json(&d).unwrap(),
        json!({"state": "disabled", "revision": "1", "observed_at": "2026-11-21T08:30:00Z",
            "reason_code": "qualification_pending"})
    );
}

/// D-547: the evidence pins the grid and maximum of the rule's own unit (`unit_ref`), not those
/// of another unit the definition allows, and reads them back with the quota kind; a unit
/// without a maximum stores `max: null` and reads as `None`.
#[test]
fn the_evidence_pins_the_grid_of_the_rules_own_unit() {
    use bss_pricing_sdk::plan_quota::PlanQuotaKind;
    const PACK_UNIT: &str =
        "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu_pack.v1";
    let mut d = sample_definition();
    let mut pack = d.allowed_units[0].clone();
    pack.unit_ref = PACK_UNIT.into();
    pack.digest = "9".repeat(64);
    pack.numerator = "8".into();
    pack.min = "1".into();
    pack.step = "2".into();
    pack.max = None;
    d.allowed_units.push(pack);
    let vcpu = evidence_json(&d, VCPU_UNIT);
    assert_eq!(
        (
            &vcpu["unit"]["min"],
            &vcpu["unit"]["step"],
            &vcpu["unit"]["max"]
        ),
        (&json!("0"), &json!("1"), &json!("1024")),
        "{vcpu}"
    );
    let packed = evidence_json(&d, PACK_UNIT);
    assert_eq!(
        packed["unit"],
        json!({"unit_ref": PACK_UNIT, "digest": "9".repeat(64), "numerator": "8",
            "denominator": "1", "min": "1", "step": "2", "max": null}),
        "the pack's own grid, not the vCPU unit's"
    );
    let read = evidence_of(&vcpu).unwrap();
    assert_eq!(read.quota_kind, PlanQuotaKind::Allocation);
    assert_eq!(
        (read.unit_min, read.unit_step, read.unit_max),
        ("0".to_owned(), "1".to_owned(), Some("1024".to_owned()))
    );
    let read = evidence_of(&packed).unwrap();
    assert_eq!(
        (read.unit_min, read.unit_step, read.unit_max),
        ("1".to_owned(), "2".to_owned(), None)
    );
    assert_eq!(read.unit_numerator, "8");
}

/// D-547: stored evidence whose quota kind, unit grid or unit maximum does not read is a corrupt
/// row naming the member: a member missing (`max` included, which may be `null` but not absent),
/// a value that is not a canonical decimal integer, a `max` below the `min`, or a kind outside
/// the closed set. A step of `0` and a `max` equal to the `min` read.
#[test]
fn stored_evidence_with_a_bad_grid_or_kind_is_a_corrupt_row_naming_it() {
    /// One damage done to stored evidence, and what the refusal names.
    type Damage = (&'static str, Box<dyn Fn(&mut Value)>, &'static str);
    let good = evidence_json(&sample_definition(), VCPU_UNIT);
    let remove = |member: &'static str| {
        Box::new(move |e: &mut Value| {
            e["unit"].as_object_mut().unwrap().remove(member);
        }) as Box<dyn Fn(&mut Value)>
    };
    let cases: Vec<Damage> = vec![
        ("min absent", remove("min"), "`min`"),
        ("step absent", remove("step"), "`step`"),
        ("max absent", remove("max"), "`max`"),
        (
            "kind absent",
            Box::new(|e| {
                e.as_object_mut().unwrap().remove("quota_kind");
            }),
            "`quota_kind`",
        ),
        (
            "min padded",
            Box::new(|e| e["unit"]["min"] = json!("01")),
            "unit min",
        ),
        (
            "min negative",
            Box::new(|e| e["unit"]["min"] = json!("-1")),
            "unit min",
        ),
        (
            "step a fraction",
            Box::new(|e| e["unit"]["step"] = json!("1.5")),
            "unit step",
        ),
        (
            "step empty",
            Box::new(|e| e["unit"]["step"] = json!("")),
            "unit step",
        ),
        (
            "max a word",
            Box::new(|e| e["unit"]["max"] = json!("many")),
            "unit max",
        ),
        (
            "max above u128::MAX",
            Box::new(|e| e["unit"]["max"] = json!("340282366920938463463374607431768211456")),
            "unit max",
        ),
        (
            "max below min",
            Box::new(|e| {
                e["unit"]["min"] = json!("5");
                e["unit"]["max"] = json!("4");
            }),
            "unit max is below its unit min",
        ),
        (
            "kind in another case",
            Box::new(|e| e["quota_kind"] = json!("Allocation")),
            "quota kind",
        ),
        (
            "kind unknown",
            Box::new(|e| e["quota_kind"] = json!("consumption")),
            "quota kind",
        ),
    ];
    for (name, tamper, named) in cases {
        let mut evidence = good.clone();
        tamper(&mut evidence);
        match evidence_of(&evidence) {
            Err(RepoError::CorruptRow(detail)) => {
                assert!(detail.contains(named), "{name}: {detail}");
            }
            other => panic!("{name}: {other:?} for {evidence}"),
        }
    }
    let mut edge = good;
    edge["unit"]["step"] = json!("0");
    edge["unit"]["min"] = json!("1024");
    let read = evidence_of(&edge).unwrap();
    assert_eq!(
        (read.unit_min.as_str(), read.unit_step.as_str()),
        ("1024", "0"),
        "a step of 0 admits the min alone, and a max equal to the min reads"
    );
    edge["unit"]["max"] = Value::Null;
    assert_eq!(evidence_of(&edge).unwrap().unit_max, None);
}

#[test]
fn a_stored_rule_round_trips_with_its_evidence_and_observation() {
    let (tenant, revision) = (Uuid::from_u128(0x7e), Uuid::from_u128(0x200));
    let mut rule = sample_rule(Uuid::from_u128(0xa));
    rule.dimension_value = Some("eu".into());
    rule.basis = PlanQuotaBasis::Fixed;
    rule.definition_ref.version = std::num::NonZeroU64::new(u64::MAX).unwrap();
    rule.definition_ref.digest[31] = 0x0f;
    let mut definition = sample_definition();
    definition.version = QuotaVersion::new(u64::MAX).unwrap();
    definition.definition_digest = format!("{}0f", "a".repeat(62));
    let row = row_of(tenant, revision, &rule, &definition, at(10)).unwrap();
    assert_eq!(
        (row.tenant_id, row.revision_id, row.created_at),
        (tenant, revision, at(10))
    );
    assert_eq!(row.definition_version, "18446744073709551615");
    assert_eq!(row.definition_digest, format!("{}0f", "a".repeat(62)));
    assert_eq!(row.basis, "fixed");
    assert_eq!(row.subject_selector, "resource_owner");
    let stored = stored_rule(&row).unwrap();
    assert_eq!(stored.rule, rule);
    assert_eq!(stored.evidence, evidence_json(&definition, VCPU_UNIT));
    assert_eq!(stored.availability, definition.availability);
    assert_eq!(rule_of(&row).unwrap(), rule);
    let other = sample_definition();
    assert!(
        row_of(tenant, revision, &rule, &other, at(10)).is_err(),
        "a definition of another version is not the rule's verdict"
    );
    let mut changed = definition;
    changed.definition_digest = "a".repeat(64);
    assert!(
        row_of(tenant, revision, &rule, &changed, at(10)).is_err(),
        "a definition of the same version with another digest is not the rule's verdict"
    );
}

/// D-547: the stored evidence of a rule reads back as the SDK's pinned evidence: the quota kind,
/// the mapping and profile references, the rule's unit with its digest, factor, grid and maximum,
/// both policies and the change support, digests as bytes and versions as positive integers.
#[test]
fn stored_evidence_reads_as_the_pinned_evidence() {
    use bss_pricing_sdk::read::{PinnedQuotaEvidence, QuotaOwnerRef};
    let d = sample_definition();
    let evidence = evidence_of(&evidence_json(&d, VCPU_UNIT)).unwrap();
    let bytes = |c: char| digest_from_hex(&c.to_string().repeat(64)).unwrap();
    assert_eq!(
        evidence,
        PinnedQuotaEvidence {
            sku_id: SKU,
            sku_version: NonZeroU64::new(3).unwrap(),
            metric_ref: VCPU_METRIC.into(),
            metric_digest: bytes('b'),
            quota_kind: bss_pricing_sdk::plan_quota::PlanQuotaKind::Allocation,
            canonical_unit_ref: VCPU_UNIT.into(),
            unit_digest: bytes('c'),
            unit_numerator: "1".into(),
            unit_denominator: "1".into(),
            unit_min: "0".into(),
            unit_step: "1".into(),
            unit_max: Some("1024".into()),
            mapping: QuotaOwnerRef {
                id: Uuid::from_u128(0xa1),
                version: NonZeroU64::MIN,
                digest: bytes('d'),
            },
            profile: QuotaOwnerRef {
                id: Uuid::from_u128(0xb1),
                version: NonZeroU64::new(2).unwrap(),
                digest: bytes('e'),
            },
            accounting_policy_ref: ALLOCATED_VCPU.into(),
            accounting_policy_digest: bytes('2'),
            scope_policy_ref: REGIONAL_POOL.into(),
            scope_policy_digest: bytes('3'),
            initial_set_supported: true,
            increase_supported: false,
        }
    );
}

/// D-547: stored evidence that does not read as the pinned evidence is a corrupt row (a 500 at
/// resolve), never a default: a rule unit the definition did not allow (`unit: null`), a digest
/// that is not 64 lowercase hex characters, a version that is not a canonical integer from 1, a
/// unit factor that Products' parser refuses (not two amounts from 1 that fit in 128 bits, or not
/// reduced), and a missing or mistyped member.
#[test]
fn stored_evidence_that_does_not_read_is_a_corrupt_row() {
    /// One damage done to stored evidence.
    type Damage = Box<dyn Fn(&mut Value)>;
    let good = evidence_json(&sample_definition(), VCPU_UNIT);
    let cases: Vec<(&str, Damage)> = vec![
        ("unit null", Box::new(|e| e["unit"] = Value::Null)),
        (
            "unit absent",
            Box::new(|e| {
                e.as_object_mut().unwrap().remove("unit");
            }),
        ),
        (
            "metric digest upper case",
            Box::new(|e| e["metric_digest"] = json!("B".repeat(64))),
        ),
        (
            "unit digest short",
            Box::new(|e| e["unit"]["digest"] = json!("c".repeat(63))),
        ),
        (
            "mapping digest",
            Box::new(|e| e["mapping_ref"]["digest"] = json!("x".repeat(64))),
        ),
        (
            "policy digest",
            Box::new(|e| e["scope_policy_digest"] = json!("")),
        ),
        (
            "sku version zero",
            Box::new(|e| e["sku_version"] = json!("0")),
        ),
        (
            "sku version padded",
            Box::new(|e| e["sku_version"] = json!("03")),
        ),
        (
            "sku version a number",
            Box::new(|e| e["sku_version"] = json!(3)),
        ),
        (
            "profile version zero",
            Box::new(|e| e["profile_ref"]["version"] = json!("0")),
        ),
        (
            "numerator",
            Box::new(|e| e["unit"]["numerator"] = json!("1.0")),
        ),
        (
            "denominator",
            Box::new(|e| e["unit"]["denominator"] = json!("0")),
        ),
        // Products' factor rule (`Factor::parse`): a reduced fraction of two amounts.
        (
            "factor not reduced",
            Box::new(|e| {
                e["unit"]["numerator"] = json!("2");
                e["unit"]["denominator"] = json!("4");
            }),
        ),
        (
            "numerator above u128::MAX",
            Box::new(|e| {
                e["unit"]["numerator"] = json!("340282366920938463463374607431768211456");
            }),
        ),
        (
            "denominator of 40 digits",
            Box::new(|e| e["unit"]["denominator"] = json!("1".repeat(40))),
        ),
        ("sku id", Box::new(|e| e["sku_id"] = json!("not a uuid"))),
        (
            "metric absent",
            Box::new(|e| {
                e.as_object_mut().unwrap().remove("metric_ref");
            }),
        ),
        (
            "change support",
            Box::new(|e| e["change_support"]["increase"] = json!("no")),
        ),
        ("not an object", Box::new(|e| *e = json!([]))),
    ];
    for (name, tamper) in cases {
        let mut evidence = good.clone();
        tamper(&mut evidence);
        assert!(
            matches!(evidence_of(&evidence), Err(RepoError::CorruptRow(_))),
            "{name}: {evidence}"
        );
    }
    assert!(evidence_of(&good).is_ok());
}

/// One damage done to a stored row.
type Tamper = Box<dyn Fn(&mut rule_e::Model)>;

#[test]
fn a_tampered_row_is_a_corrupt_row() {
    let rule = sample_rule(Uuid::from_u128(0xa));
    let good = row_of(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        &rule,
        &sample_definition(),
        at(1),
    )
    .unwrap();
    let tampered: Vec<(&str, Tamper)> = vec![
        ("version 0", Box::new(|r| r.definition_version = "0".into())),
        (
            "version 01",
            Box::new(|r| r.definition_version = "01".into()),
        ),
        (
            "version text",
            Box::new(|r| r.definition_version = "v1".into()),
        ),
        (
            "version over 64 bits",
            Box::new(|r| r.definition_version = "18446744073709551616".into()),
        ),
        (
            "uppercase digest",
            Box::new(|r| r.definition_digest = "A".repeat(64)),
        ),
        (
            "short digest",
            Box::new(|r| r.definition_digest = "a".repeat(62)),
        ),
        ("basis token", Box::new(|r| r.basis = "monthly".into())),
        (
            "subject token",
            Box::new(|r| r.subject_selector = "tenant".into()),
        ),
        (
            "scope array",
            Box::new(|r| r.scope_selection_json = "[\"region\"]".into()),
        ),
        (
            "scope number",
            Box::new(|r| r.scope_selection_json = "{\"region\":1}".into()),
        ),
        (
            "scope text",
            Box::new(|r| r.scope_selection_json = "region=eu".into()),
        ),
        (
            "scope key",
            Box::new(|r| r.scope_selection_json = "{\"a b\":\"eu\"}".into()),
        ),
        ("amount", Box::new(|r| r.amount = "1.5".into())),
        (
            "binding key",
            Box::new(|r| r.logical_binding_key = "a/b".into()),
        ),
        (
            "evidence array",
            Box::new(|r| r.evidence_json = "[]".into()),
        ),
        (
            "evidence text",
            Box::new(|r| r.evidence_json = "evidence".into()),
        ),
        (
            "availability state",
            Box::new(|r| {
                r.availability_json = r.availability_json.replace("disabled", "paused");
            }),
        ),
        (
            "availability text",
            Box::new(|r| r.availability_json = "{}".into()),
        ),
    ];
    for (what, tamper) in tampered {
        let mut row = good.clone();
        tamper(&mut row);
        match stored_rule(&row) {
            Err(RepoError::CorruptRow(detail)) => {
                assert!(
                    detail.contains(&row.rule_id.to_string()),
                    "{what}: {detail}"
                );
                assert!(
                    detail.contains(&row.revision_id.to_string()),
                    "{what}: {detail}"
                );
            }
            other => panic!("{what}: {other:?}"),
        }
        assert!(rule_of(&row).is_err(), "{what}");
    }
    assert!(stored_rule(&good).is_ok());
}

/// A registry double: each SKU's version, or the status its read fails with, and a later version
/// with the day it is in force from; it counts the reads and records the day each read asks.
#[derive(Default)]
struct Registry {
    versions: BTreeMap<Uuid, Result<i64, u16>>,
    /// A SKU's next version and its first day: a read of that day or a later one answers it.
    later: BTreeMap<Uuid, (time::Date, i64)>,
    reads: AtomicUsize,
    days: Mutex<Vec<(Uuid, time::Date)>>,
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Registry {
    async fn reserve(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: Uuid,
        _: ReferenceKind,
        _: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn confirm(&self, _: &SecurityContext, _: Uuid, _: Uuid) -> Result<(), CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn release(&self, _: &SecurityContext, _: Uuid, _: Uuid) -> Result<(), CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn states(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn sku_for_write(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: Uuid,
    ) -> Result<Sku, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn skus_for_write(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &[Uuid],
    ) -> Result<Vec<Sku>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn sku_version_as_of(
        &self,
        _: &SecurityContext,
        _: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.days.lock().unwrap().push((id, date));
        let later = self.later.get(&id).filter(|(from, _)| date >= *from);
        match self.versions.get(&id) {
            None => Ok(None),
            Some(Err(404)) => Err(crate::api::rest::authoring::support::missing_what("sku")),
            Some(Err(status)) => Err(refusal(*status)),
            Some(Ok(version)) => Ok(Some(SkuVersion {
                sku_id: id,
                published_version: later.map_or(*version, |(_, later)| *later),
                effective_from: later.map_or(date, |(from, _)| *from),
                content: SkuContent {
                    code: "VCPU".into(),
                    name: "vCPU".into(),
                    r#type: SkuType::Recurring,
                    category_id: None,
                    description: String::new(),
                    sellable: true,
                    gl_code: None,
                    tax_category: None,
                    invoice_line_template: None,
                    billing_timing: None,
                    usage_type_ref: None,
                    unit: None,
                },
                created_at: at(1),
            })),
        }
    }
}
fn hub_with(registry: Arc<Registry>, catalog: Arc<Catalog>) -> toolkit::ClientHub {
    let hub = toolkit::ClientHub::default();
    hub.register::<PricingReferenceRegistry>(Arc::new(PricingReferenceRegistry(registry)));
    hub.register::<dyn ProductsQuotaCatalogClientV1>(catalog);
    hub
}
fn day() -> time::Date {
    at(1).date()
}

#[tokio::test]
async fn the_sku_versions_are_read_once_per_sku_and_a_missing_version_is_absent() {
    let registry = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Ok(3)), (Uuid::from_u128(0x40), Err(404))]),
        ..Registry::default()
    });
    let hub = hub_with(registry.clone(), Arc::new(Catalog::default()));
    let versions = sku_versions(
        &hub,
        &user(),
        &[SKU, SKU, OTHER_SKU, Uuid::from_u128(0x40)],
        day(),
    )
    .await
    .unwrap();
    assert_eq!(versions, BTreeMap::from([(SKU, 3)]));
    assert_eq!(
        registry.reads.load(Ordering::SeqCst),
        3,
        "one read per distinct SKU"
    );
    assert!(
        sku_versions(&hub, &user(), &[], day())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(registry.reads.load(Ordering::SeqCst), 3, "no SKU, no read");
    for (status, answered, code) in [(403, 403, ""), (503, 503, "REGISTRY_UNAVAILABLE")] {
        let registry = Arc::new(Registry {
            versions: BTreeMap::from([(SKU, Err(status))]),
            ..Registry::default()
        });
        let hub = hub_with(registry, Arc::new(Catalog::default()));
        let error = sku_versions(&hub, &user(), &[SKU], day())
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), answered, "{status}");
        assert!(error.to_string().contains(code), "{status}: {error}");
    }
    let error = sku_versions(&toolkit::ClientHub::default(), &user(), &[SKU], day())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("REGISTRY_UNAVAILABLE"),
        "{error}"
    );
}

/// A tenant's draft with two items on one database file, the first item ruled.
struct Ruled {
    db: DBProvider<DbError>,
    tenant: Uuid,
    revision: plan_revision::Model,
    items: Vec<Uuid>,
    dsn: TestDsn,
}
impl Ruled {
    fn scope(&self) -> AccessScope {
        AccessScope::for_tenant(self.tenant)
    }
    /// Replace the set with `rules`, at the revision's stored version.
    async fn replace(&self, rules: &[PlanQuotaRuleV1]) {
        let conn = self.db.conn().unwrap();
        let current = plan_revision_repo::find(&conn, &self.scope(), self.tenant, self.revision.id)
            .await
            .unwrap()
            .unwrap();
        let rows = rules
            .iter()
            .map(|rule| {
                row_of(
                    self.tenant,
                    self.revision.id,
                    rule,
                    &sample_definition(),
                    at(10),
                )
                .unwrap()
            })
            .collect();
        plan_quota_repo::replace(
            &conn,
            &self.scope(),
            self.tenant,
            self.revision.id,
            current.version,
            rows,
            digest_text(bss_pricing_sdk::digest::quota_rules_digest(rules)),
            Uuid::from_u128(0x0e),
            at(10),
        )
        .await
        .unwrap();
    }
    async fn item(&self, sku: Uuid) -> Uuid {
        plan_item_repo::insert(
            &self.db.conn().unwrap(),
            &self.scope(),
            plan_item::Model {
                id: Uuid::new_v4(),
                tenant_id: self.tenant,
                revision_id: self.revision.id,
                sku_id: sku,
                price_book_entry_id: None,
                treatment: "included".into(),
                included_qty: None,
                qty_min: None,
                reservation_id: None,
                reference_state: "unreserved".into(),
                version: 1,
                created_by: Uuid::from_u128(0x0e),
                created_at: at(9),
                updated_at: at(9),
            },
        )
        .await
        .unwrap()
        .id
    }
    /// Raw SQL past every repository.
    async fn raw(&self, sql: &str) {
        use sea_orm::ConnectionTrait;
        sea_orm::Database::connect(String::from(&self.dsn))
            .await
            .unwrap()
            .execute_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                sql.to_owned(),
            ))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    /// `observation.check_local` in a serializable read transaction of its own.
    async fn check(&self, observation: &QuotaObservation) -> Result<(), DoorError> {
        let (observation, tenant) = (observation.clone(), self.tenant);
        support::transaction_door_read(&self.db.db(), move |tx| {
            let observation = observation.clone();
            Box::pin(async move { observation.check_local(tx, tenant).await })
        })
        .await
    }
}
impl Ruled {
    /// Another plan's draft revision in the same tenant, with no item and no rule.
    async fn other_revision(&self) -> Uuid {
        let conn = self.db.conn().unwrap();
        let user = Uuid::from_u128(0x0e);
        let plan = plan_repo::insert(
            &conn,
            &self.scope(),
            plan_e::Model {
                id: Uuid::new_v4(),
                tenant_id: self.tenant,
                code: "free".into(),
                name: "Free".into(),
                published_rev: None,
                version: 1,
                created_by: user,
                created_at: at(9),
                updated_at: at(9),
                work_revision_id: None,
                work_state: None,
                scheduled_revision_id: None,
                scheduled_from: None,
                published_revision_id: None,
                current_book_id: None,
                current_currency: None,
                last_activity_at: at(9),
            },
        )
        .await
        .unwrap();
        plan_revision_repo::insert(
            &conn,
            &self.scope(),
            plan_revision::Model {
                id: Uuid::new_v4(),
                plan_id: plan.id,
                ..self.revision.clone()
            },
        )
        .await
        .unwrap()
        .id
    }
    /// The checks' stored contexts of `ids`, in one serializable read transaction.
    async fn contexts(
        &self,
        ids: Vec<Uuid>,
    ) -> Result<BTreeMap<Uuid, crate::domain::plan::PlanContext>, DoorError> {
        let tenant = self.tenant;
        support::transaction_door_read(&self.db.db(), move |tx| {
            let ids = ids.clone();
            Box::pin(async move {
                crate::api::rest::authoring::plans::stored_contexts(
                    tx,
                    &AccessScope::for_tenant(tenant),
                    tenant,
                    &ids,
                    day(),
                )
                .await
            })
        })
        .await
    }
}
async fn ruled() -> Ruled {
    let (db, scope, tenant, dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let user = Uuid::from_u128(0x0e);
    let book = book_repo::insert(
        &conn,
        &scope,
        price_book::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            code: "eur".into(),
            name: "Default EUR".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: at(9),
            updated_at: at(9),
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let plan = plan_repo::insert(
        &conn,
        &scope,
        plan_e::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            code: "pro".into(),
            name: "Pro".into(),
            published_rev: None,
            version: 1,
            created_by: user,
            created_at: at(9),
            updated_at: at(9),
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: at(9),
        },
    )
    .await
    .unwrap();
    let revision = plan_revision_repo::insert(
        &conn,
        &scope,
        plan_revision::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            plan_id: plan.id,
            rev_no: 1,
            book_id: book.id,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: user,
            created_at: at(9),
            updated_at: at(9),
        },
    )
    .await
    .unwrap();
    let mut f = Ruled {
        db,
        tenant,
        revision,
        items: Vec::new(),
        dsn,
    };
    f.items = vec![f.item(SKU).await, f.item(OTHER_SKU).await];
    f
}

#[tokio::test]
async fn a_capture_calls_products_outside_the_transaction_and_its_check_sees_a_moved_set() {
    let f = ruled().await;
    let registry = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Ok(3))]),
        ..Registry::default()
    });
    let catalog = Arc::new(Catalog {
        db: Some(f.db.clone()),
        ..Catalog::default()
    });
    let hub = hub_with(registry.clone(), catalog.clone());
    let ctx = user_in(f.tenant);
    let empty = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    assert!(empty.rules().is_empty(), "a revision without rules");
    assert_eq!(empty.digest(), plan_quota_repo::empty_digest());
    assert!(empty.verdict().unwrap().ok());
    assert_eq!(
        registry.reads.load(Ordering::SeqCst),
        0,
        "reads no SKU version"
    );
    assert_eq!(
        catalog.calls.load(Ordering::SeqCst),
        0,
        "and asks no catalog"
    );

    let rules = vec![
        sample_rule_n(1, f.items[0], "10"),
        sample_rule_n(2, f.items[1], "20"),
    ];
    f.replace(&rules).await;
    let observation = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    assert_eq!(observation.rules(), rules.as_slice());
    assert_eq!(observation.versions(), &BTreeMap::from([(SKU, 3)]));
    assert_eq!(
        observation.digest(),
        digest_text(bss_pricing_sdk::digest::quota_rules_digest(&rules))
    );
    let verdict = observation.verdict().unwrap();
    assert_eq!(
        verdict.rules.len(),
        1,
        "the rule whose SKU has no version is not sent"
    );
    assert_eq!(
        verdict.checks[0].code,
        QuotaCheckCode::QualificationUnavailable
    );
    let sent = catalog.requests.lock().unwrap()[0].clone();
    assert_eq!(sent.purpose, QuotaValidationPurpose::Authoring);
    assert_eq!(sent.quantity, None);
    assert_eq!(
        (sent.rules[0].rule_id, sent.rules[0].sku_version.get()),
        (Uuid::from_u128(1), 3)
    );
    assert_eq!(
        catalog.calls.load(Ordering::SeqCst),
        1,
        "outside any transaction"
    );
    assert_eq!(registry.reads.load(Ordering::SeqCst), 2, "one read per SKU");

    f.check(&observation).await.unwrap();
    let mut moved = rules.clone();
    moved[1].amount = "21".into();
    f.replace(&moved).await;
    assert!(matches!(
        f.check(&observation).await,
        Err(DoorError::SelectionMoved)
    ));

    let observation = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    f.check(&observation).await.unwrap();
    f.item(Uuid::from_u128(0x7)).await;
    assert!(
        matches!(f.check(&observation).await, Err(DoorError::SelectionMoved)),
        "an item added since the capture"
    );

    let observation = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    f.check(&observation).await.unwrap();
    f.raw(&format!(
        "UPDATE pricing_plan_revision SET available_from = '2027-01-01' WHERE id = X'{}'",
        f.revision.id.simple()
    ))
    .await;
    assert!(
        matches!(f.check(&observation).await, Err(DoorError::SelectionMoved)),
        "a sale date moved since the capture reads other SKU versions"
    );
}

#[tokio::test]
async fn a_dependency_failure_is_kept_in_the_capture() {
    let f = ruled().await;
    f.replace(&[sample_rule_n(1, f.items[0], "10")]).await;
    let ctx = user_in(f.tenant);
    let catalog = Arc::new(Catalog::default());
    let down = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Err(503))]),
        ..Registry::default()
    });
    let observation = QuotaObservation::capture(
        &f.db.db(),
        &hub_with(down, catalog.clone()),
        &ctx,
        &f.revision,
        day(),
    )
    .await
    .unwrap();
    let error = observation.verdict().unwrap_err();
    assert!(
        error.to_string().contains("REGISTRY_UNAVAILABLE"),
        "{error}"
    );
    assert_eq!(
        catalog.calls.load(Ordering::SeqCst),
        0,
        "no version read, no catalog call"
    );

    let registry = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Ok(3))]),
        ..Registry::default()
    });
    let outage = Arc::new(Catalog {
        status: Some(503),
        ..Catalog::default()
    });
    let observation = QuotaObservation::capture(
        &f.db.db(),
        &hub_with(registry, outage),
        &ctx,
        &f.revision,
        day(),
    )
    .await
    .unwrap();
    let error = observation.verdict().unwrap_err();
    assert!(
        error.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{error}"
    );
    f.check(&observation).await.unwrap();
}

#[tokio::test]
async fn a_set_whose_skus_have_no_version_on_the_sale_date_asks_no_catalog() {
    let f = ruled().await;
    f.replace(&[sample_rule_n(1, f.items[0], "10")]).await;
    let catalog = Arc::new(Catalog::default());
    let observation = QuotaObservation::capture(
        &f.db.db(),
        &hub_with(Arc::new(Registry::default()), catalog.clone()),
        &user_in(f.tenant),
        &f.revision,
        day(),
    )
    .await
    .unwrap();
    assert!(observation.verdict().unwrap().ok());
    assert!(observation.versions().is_empty());
    assert_eq!(catalog.calls.load(Ordering::SeqCst), 0);
}

/// A capture reads each ruled SKU's version on the revision's sale date, not today: a revision
/// sold from a later day judges its rules against the version in force then, and the stored sale
/// date is what its check compares.
#[tokio::test]
async fn a_capture_reads_the_sku_version_on_the_sale_date_not_today() {
    let f = ruled().await;
    f.replace(&[sample_rule_n(1, f.items[0], "10")]).await;
    let sale = day() + time::Duration::days(30);
    let registry = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Ok(3))]),
        later: BTreeMap::from([(SKU, (sale, 4))]),
        ..Registry::default()
    });
    let catalog = Arc::new(Catalog::default());
    let hub = hub_with(registry.clone(), catalog.clone());
    let ctx = user_in(f.tenant);
    let today = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    assert_eq!(today.versions(), &BTreeMap::from([(SKU, 3)]));
    f.raw(&format!(
        "UPDATE pricing_plan_revision SET available_from = '{sale}' WHERE id = X'{}'",
        f.revision.id.simple()
    ))
    .await;
    let scheduled = plan_revision::Model {
        available_from: Some(sale),
        ..f.revision.clone()
    };
    let later = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &scheduled, day())
        .await
        .unwrap();
    assert_eq!(later.versions(), &BTreeMap::from([(SKU, 4)]));
    let sent: Vec<u64> = catalog
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.rules[0].sku_version.get())
        .collect();
    assert_eq!(sent, [3, 4], "today's version, then the sale date's");
    assert_eq!(*registry.days.lock().unwrap(), [(SKU, day()), (SKU, sale)]);
    f.check(&later).await.unwrap();
    assert!(
        matches!(f.check(&today).await, Err(DoorError::SelectionMoved)),
        "a capture of another sale date is moved"
    );
}

/// A replace between a unit's capture and its transaction: a capture of a revision without rules
/// keeps the empty set's digest, so a set added before the unit's transaction moves the selection
/// and the capture runs again; the check of an empty capture reads only the set.
#[tokio::test]
async fn an_empty_capture_sees_a_set_added_before_the_transaction() {
    let f = ruled().await;
    let registry = Arc::new(Registry {
        versions: BTreeMap::from([(SKU, Ok(3))]),
        ..Registry::default()
    });
    let catalog = Arc::new(Catalog::default());
    let hub = hub_with(registry.clone(), catalog.clone());
    let ctx = user_in(f.tenant);
    let empty = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    assert_eq!(empty.digest(), plan_quota_repo::empty_digest());
    assert_eq!(
        (
            registry.reads.load(Ordering::SeqCst),
            catalog.calls.load(Ordering::SeqCst)
        ),
        (0, 0),
        "an empty set reads no SKU version and asks no catalog"
    );
    f.check(&empty).await.unwrap();
    f.item(Uuid::from_u128(0x7)).await;
    f.check(&empty)
        .await
        .expect("an empty capture judges only the set, as a quota-free unit always did");
    f.replace(&[sample_rule_n(1, f.items[0], "10")]).await;
    assert!(
        matches!(f.check(&empty).await, Err(DoorError::SelectionMoved)),
        "a set added since the capture moves the selection"
    );
    let again = QuotaObservation::capture(&f.db.db(), &hub, &ctx, &f.revision, day())
        .await
        .unwrap();
    assert_eq!(again.rules().len(), 1);
    f.check(&again).await.unwrap();
}

/// The checks' stored contexts carry each revision's own rules, in `rule_id` order, and its
/// stored digest; a revision without rules carries none and the empty set's digest. A tampered
/// rule row is a corrupt row.
#[tokio::test]
async fn the_checks_contexts_carry_each_revisions_own_set() {
    let f = ruled().await;
    let rules = vec![
        sample_rule_n(2, f.items[1], "4"),
        sample_rule_n(1, f.items[0], "10"),
    ];
    f.replace(&rules).await;
    let free = f.other_revision().await;
    let contexts = f.contexts(vec![f.revision.id, free]).await.unwrap();
    let with_rules = &contexts[&f.revision.id];
    assert_eq!(
        with_rules
            .quota_rules
            .iter()
            .map(|rule| (rule.rule_id, rule.item_id))
            .collect::<Vec<_>>(),
        [
            (Uuid::from_u128(1), f.items[0]),
            (Uuid::from_u128(2), f.items[1])
        ],
        "the stored order, each on its own item"
    );
    assert_eq!(
        with_rules.quota_digest,
        digest_text(bss_pricing_sdk::digest::quota_rules_digest(&rules))
    );
    assert!(
        with_rules.quota_verdict.is_none(),
        "the readers ask Products"
    );
    let other = &contexts[&free];
    assert!(other.quota_rules.is_empty(), "no other revision's rules");
    assert_eq!(other.quota_digest, plan_quota_repo::empty_digest());
    f.raw(&format!(
        "UPDATE pricing_plan_quota_rule SET scope_selection_json = '{{\"region\": 1}}' \
         WHERE revision_id = X'{}'",
        f.revision.id.simple()
    ))
    .await;
    assert!(
        matches!(
            f.contexts(vec![f.revision.id, free]).await,
            Err(DoorError::Repo(RepoError::CorruptRow(_)))
        ),
        "a tampered rule row is a corrupt row"
    );
}

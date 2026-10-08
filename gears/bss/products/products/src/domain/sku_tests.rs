#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::recognized::UsageTypeAnswer;
use bss_products_sdk::models::{Lifecycle, SkuContent, SkuType};
use uuid::Uuid;

fn content(t: SkuType) -> SkuContent {
    SkuContent {
        code: "STORAGE".into(),
        name: "Storage".into(),
        r#type: t,
        category_id: Some(Uuid::new_v4()),
        description: String::new(),
        sellable: true,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
    }
}

#[test]
fn blank_code_or_name_is_a_violation() {
    let mut n = NewSku {
        code: " ".into(),
        name: String::new(),
        r#type: SkuType::Recurring,
        category_id: Some(Uuid::new_v4()),
        description: String::new(),
        sellable: true,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
    };
    let r = validate_new(&n);
    assert!(has(&r, "code", "VALIDATION") && has(&r, "name", "VALIDATION"));
    n.code = "x".repeat(65);
    n.name = "ok".into();
    assert!(has(&validate_new(&n), "code", "FIELD_TOO_LONG"));
}
#[test]
fn usage_needs_a_meter_and_a_resolved_type_bundle_has_none() {
    let mut u = content(SkuType::Usage);
    assert!(has(
        &validate_publish(&u, None),
        "usage_type_ref",
        "USAGE_NEEDS_METER"
    ));
    u.usage_type_ref = Some("storage_gb_hours".into());
    u.unit = Some("GB\u{b7}month".into());
    assert!(has(
        &validate_publish(&u, Some(&resolved_answer())),
        "usage_type_ref",
        "DERIVED_USAGE_TYPE_REQUIRED"
    ));
    assert!(has(
        &validate_publish(&u, Some(&unresolved_answer())),
        "usage_type_ref",
        "DERIVED_USAGE_TYPE_REQUIRED"
    ));
    let mut b = content(SkuType::Bundle);
    b.unit = Some("x".into());
    assert!(has(
        &validate_publish(&b, None),
        "unit",
        "BUNDLE_HAS_NO_METER"
    ));
    // RT-06: the meter half, on its own.
    let mut metered = content(SkuType::Bundle);
    metered.usage_type_ref = Some("storage".into());
    let report = validate_publish(&metered, None);
    assert!(has(&report, "usage_type_ref", "BUNDLE_HAS_NO_METER"));
    assert!(!has(&report, "unit", "BUNDLE_HAS_NO_METER"));
}
#[test]
fn the_type_is_frozen_once_a_price_book_entry_exists() {
    assert!(validate_type_change(0).is_ok());
    assert!(matches!(
        validate_type_change(1),
        Err(DomainError::Conflict {
            code: "SKU_TYPE_FROZEN",
            ..
        })
    ));
}
#[test]
fn lifecycle_edges_are_publish_deprecate_and_retire() {
    use Lifecycle::*;
    for (f, t, ok) in [
        (Draft, Published, true),
        (Published, Deprecated, true),
        (Deprecated, Published, true),
        (Published, Retired, true),
        (Deprecated, Retired, true),
        (Draft, Retired, false),
        (Retired, Published, false),
        (Published, Draft, false),
        (Draft, Deprecated, false),
    ] {
        assert_eq!(lifecycle_edge(f, t), ok, "{f:?} → {t:?}");
    }
}
#[test]
fn apply_patch_and_changed_fields_agree_and_clear_works() {
    let a = content(SkuType::Recurring);
    let p = SkuPatch {
        name: Some("Storage Plus".into()),
        gl_code: Some(Some("4012".into())),
        tax_category: Some(None),
        ..Default::default()
    };
    let b = apply_patch(&a, &p);
    assert_eq!(b.gl_code.as_deref(), Some("4012"));
    assert_eq!(b.name, "Storage Plus");
    assert_eq!(b.tax_category, None);
    assert_eq!(
        changed_fields(&a, &b),
        vec!["gl_code".to_owned(), "name".to_owned()]
    ); // tax_category was already None
}
fn has(r: &crate::domain::validation::ValidationReport, subject: &str, code: &str) -> bool {
    r.violations()
        .iter()
        .any(|v| v.subject == subject && v.code == code)
}

fn resolved_answer() -> UsageRefAnswer {
    UsageRefAnswer::Catalog(UsageTypeAnswer::Resolved(
        crate::test_support::probe_binding(),
    ))
}
fn unresolved_answer() -> UsageRefAnswer {
    UsageRefAnswer::Catalog(UsageTypeAnswer::Unresolved)
}

#[test]
fn a_raw_ref_cannot_be_published() {
    let mut c = content(SkuType::Usage);
    c.usage_type_ref = Some("meter".into());
    c.unit = Some("GB".into());
    let report = validate_publish(
        &c,
        Some(&UsageRefAnswer::Catalog(UsageTypeAnswer::Unavailable)),
    );
    assert!(has(
        &report,
        "usage_type_ref",
        "DERIVED_USAGE_TYPE_REQUIRED"
    ));
    assert!(!has(&report, "usage_type_ref", "USAGE_TYPE_UNAVAILABLE"));
}

#[test]
fn patch_deserialization_distinguishes_omission_null_and_value() {
    // The wire contract is `dto::SkuPatchRequest` (RS-51): the domain patch is what it converts to.
    let patch = |body: serde_json::Value| -> SkuPatch {
        let request: crate::api::rest::dto::SkuPatchRequest = serde_json::from_value(body).unwrap();
        SkuPatch::try_from(request).unwrap()
    };
    let omitted = patch(serde_json::json!({}));
    let cleared = patch(
        serde_json::json!({"gl_code":null, "tax_category":null, "invoice_line_template":null, "billing_timing":null, "usage_type_ref":null, "unit":null}),
    );
    let valued = patch(
        serde_json::json!({"gl_code":"40", "tax_category":"tax", "invoice_line_template":"line", "billing_timing":"advance", "usage_type_ref":"meter", "unit":"GB", "type":"usage", "lifecycle":"deprecated"}),
    );
    assert_eq!(omitted.gl_code, None);
    assert_eq!(omitted.billing_timing, None);
    let mut original = content(SkuType::Recurring);
    original.gl_code = Some("old".into());
    assert_eq!(apply_patch(&original, &omitted), original);
    assert_eq!(apply_patch(&original, &cleared).gl_code, None);
    let updated = apply_patch(&original, &valued);
    assert_eq!(updated.billing_timing, Some(BillingTiming::Advance));
    assert_eq!(
        changed_fields(&original, &updated),
        [
            "billing_timing",
            "gl_code",
            "invoice_line_template",
            "tax_category",
            "type",
            "unit",
            "usage_type_ref"
        ]
    );
    let cleared_content = apply_patch(&updated, &cleared);
    assert_eq!(cleared_content.gl_code, None);
    assert_eq!(cleared_content.tax_category, None);
    assert_eq!(cleared_content.invoice_line_template, None);
    assert_eq!(cleared_content.billing_timing, None);
    assert_eq!(cleared_content.usage_type_ref, None);
    assert_eq!(cleared_content.unit, None);
}

#[test]
fn code_limit_counts_characters_and_blank_meter_fields_fail() {
    let n = NewSku {
        code: "\u{e9}".repeat(64),
        name: "ok".into(),
        r#type: SkuType::Usage,
        category_id: Some(Uuid::new_v4()),
        description: String::new(),
        sellable: true,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
    };
    assert!(validate_new(&n).is_empty());
    let mut c = content(SkuType::Usage);
    c.usage_type_ref = Some(" ".into());
    c.unit = Some(" ".into());
    let r = validate_publish(&c, Some(&resolved_answer()));
    assert!(has(&r, "usage_type_ref", "USAGE_NEEDS_METER"));
    assert!(!has(&r, "unit", "USAGE_NEEDS_METER"));
}

/// P-D-196: a patch's explicit null clears the category and an omitted one keeps it; clearing and
/// setting it again are both a changed `category_id`.
#[test]
fn a_patch_clears_or_sets_the_category_and_the_diff_names_it() {
    let c = content(SkuType::Recurring);
    assert!(c.category_id.is_some());
    let kept = apply_patch(&c, &SkuPatch::default());
    assert_eq!(kept.category_id, c.category_id);
    let cleared = apply_patch(
        &c,
        &SkuPatch {
            category_id: Some(None),
            ..SkuPatch::default()
        },
    );
    assert_eq!(cleared.category_id, None);
    assert_eq!(changed_fields(&c, &cleared), ["category_id"]);
    let set = Uuid::new_v4();
    let again = apply_patch(
        &cleared,
        &SkuPatch {
            category_id: Some(Some(set)),
            ..SkuPatch::default()
        },
    );
    assert_eq!(again.category_id, Some(set));
    assert_eq!(changed_fields(&cleared, &again), ["category_id"]);
}

/// P-D-232: a derived ref publishes on its stored version, whose output unit the SKU sells; it is
/// judged first, and no catalog answer, not even `Resolved`, binds it. A derived answer binds no
/// GTS ref either.
#[test]
fn a_derived_ref_publishes_on_its_version_and_never_on_a_catalog_answer() {
    use crate::domain::derived::DerivedPin;
    let pin = DerivedPin {
        meter: "products.derived/cloudlets@1".into(),
        output_unit: "cloudlet\u{b7}hour".into(),
    };
    let mut c = content(SkuType::Usage);
    c.usage_type_ref = Some(pin.meter.clone());
    c.unit = Some(pin.output_unit.clone());
    assert!(validate_publish(&c, Some(&UsageRefAnswer::Derived(pin.clone()))).is_empty());
    for answer in [
        None,
        Some(UsageRefAnswer::DerivedUnknown),
        Some(resolved_answer()),
        Some(UsageRefAnswer::Catalog(UsageTypeAnswer::Unavailable)),
        Some(UsageRefAnswer::Derived(DerivedPin {
            meter: "products.derived/cloudlets@2".into(),
            output_unit: pin.output_unit.clone(),
        })),
    ] {
        let report = validate_publish(&c, answer.as_ref());
        assert!(
            has(&report, "usage_type_ref", "DERIVED_USAGE_TYPE_UNKNOWN"),
            "{answer:?}"
        );
        assert_eq!(report.violations().len(), 1, "{answer:?}");
    }
    c.unit = Some("GB".into());
    assert!(has(
        &validate_publish(&c, Some(&UsageRefAnswer::Derived(pin.clone()))),
        "unit",
        "DERIVED_UNIT_MISMATCH"
    ));
    c.unit = None;
    assert!(
        validate_publish(&c, Some(&UsageRefAnswer::Derived(pin.clone()))).is_empty(),
        "the unit lives on the derived type"
    );
    let mut g = content(SkuType::Usage);
    g.usage_type_ref = Some("usage:storage".into());
    g.unit = Some("GB".into());
    assert!(has(
        &validate_publish(&g, Some(&UsageRefAnswer::Derived(pin))),
        "usage_type_ref",
        "DERIVED_USAGE_TYPE_REQUIRED"
    ));
}

#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
#[test]
fn prototype_l158_kind_derived_from_sku() {
    assert_eq!(
        charge_kind_for(SkuType::Recurring).unwrap(),
        ChargeKind::Recurring
    );
    assert_eq!(charge_kind_for(SkuType::Usage).unwrap(), ChargeKind::Usage);
    assert_eq!(
        charge_kind_for(SkuType::OneTime).unwrap(),
        ChargeKind::OneTime
    );
    assert_eq!(
        charge_kind_for(SkuType::Bundle).unwrap_err().code,
        "BUNDLE_SKU_NOT_PRICEABLE"
    );
}
#[test]
fn prototype_l159_kind_matches() {
    assert!(validate_entry_kind(ChargeKind::Recurring, SkuType::Recurring).is_ok());
}
#[test]
fn prototype_l160_kind_mismatch_renamed() {
    assert_eq!(
        validate_entry_kind(ChargeKind::OneTime, SkuType::Recurring)
            .unwrap_err()
            .code,
        "CHARGE_KIND_SKU_TYPE"
    );
}
#[test]
fn prototype_l161_bundle_refused() {
    assert_eq!(
        validate_entry_kind(ChargeKind::Recurring, SkuType::Bundle)
            .unwrap_err()
            .code,
        "BUNDLE_SKU_NOT_PRICEABLE"
    );
}
#[test]
fn matrix_6_every_kind_model_pair() {
    for &k in ChargeKind::ALL {
        for &m in Model::ALL {
            let expected = match k {
                ChargeKind::Usage => m != Model::Flat,
                _ => matches!(m, Model::Flat | Model::PerUnit),
            };
            assert_eq!(model_allowed(k, m), expected, "{k:?}/{m:?}");
        }
    }
}
#[test]
fn prototype_l178_template_allowed() {
    assert!(validate_template("{sku} - {period}").is_ok());
    assert!(validate_template("{sku_code}{unit}{plan}{dimension}").is_ok());
}
#[test]
fn prototype_l179_unknown_template() {
    assert_eq!(
        validate_template("{sku} - {periode}").unwrap_err().code,
        "LINE_TEMPLATE_INVALID"
    );
}
#[test]
fn prototype_l180_unbalanced_template() {
    for s in ["{sku - {period}", "}{sku}", "{{sku}}"] {
        assert_eq!(
            validate_template(s).unwrap_err().code,
            "LINE_TEMPLATE_INVALID"
        );
    }
}
#[test]
fn prototype_l181_empty_template() {
    assert_eq!(
        validate_template("").unwrap_err().code,
        "LINE_TEMPLATE_EMPTY"
    );
}
#[test]
fn matrix_8_phase_placeholder_dropped() {
    assert_eq!(
        validate_template("{phase}").unwrap_err().code,
        "LINE_TEMPLATE_INVALID"
    );
}
/// D-427: an entry without prices takes its charge kind's default model — the rule 000013's
/// backfill applies and an op stored before it resolves to. Each default is a model its kind allows.
#[test]
fn d427_the_charge_kinds_default_model_is_one_it_allows() {
    for (kind, model) in [
        (ChargeKind::Recurring, Model::Flat),
        (ChargeKind::OneTime, Model::Flat),
        (ChargeKind::Usage, Model::PerUnit),
    ] {
        assert_eq!(default_model(kind), model, "{kind:?}");
        assert!(model_allowed(kind, default_model(kind)), "{kind:?}");
    }
}

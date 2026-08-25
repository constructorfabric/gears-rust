use super::*;

const GRADUATED: &str = r#"
family     = "tier-boundary"
id         = "graduated-band-edge"
kind       = "evaluation"
provenance = ["AC#60", "PRD 17.2", "D-17"]

[snapshot]
model_kind              = "graduated"
charge_kind             = "usage"
currency                = "USD"
tier_aggregation_window = "invoice_period"
billing_granularity     = "whole_unit"

  [[snapshot.bands]]
  from_qty = 0
  to_qty   = 1000
  unit_amount_minor = 5

  [[snapshot.bands]]
  from_qty = 1000
  to_qty   = "open"
  unit_amount_minor = 3

[[assert]]
given  = { q = 1000 }
expect = { charge_minor = 5000 }
why    = "half-open [from,to): 1000 opens band 2, but graduated is marginal"
"#;

#[test]
fn parses_a_graduated_evaluation_case() {
    let case: EvaluationCase = toml::from_str(GRADUATED).expect("corpus case must parse");

    assert_eq!(case.family, Family::TierBoundary);
    assert_eq!(case.id, "graduated-band-edge");
    assert_eq!(case.kind, CaseKind::Evaluation);
    assert_eq!(case.snapshot.model_kind, ModelKind::Graduated);
    assert_eq!(case.snapshot.charge_kind, ChargeKind::Usage);
    assert_eq!(case.snapshot.bands.len(), 2);
    assert_eq!(case.snapshot.bands[0].to_qty, BandTop::Closed(1000));
    assert_eq!(case.snapshot.bands[1].to_qty, BandTop::Open);
    assert_eq!(case.assert.len(), 1);
    assert_eq!(case.assert[0].given.q, 1000);
    assert_eq!(
        case.assert[0].expect,
        Expect::Charge(ChargeExpect { charge_minor: 5000 })
    );
}

#[test]
fn rejects_an_unknown_snapshot_field() {
    let bad = GRADUATED.replace(
        "currency                = \"USD\"",
        "currency                = \"USD\"\nlocked_unit_price_minor = 7",
    );

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("unknown snapshot field must be rejected");

    // D-60: the trailing-tier lock is per subscription per period, so it is not
    // snapshot-frozen and must not be expressible in `[snapshot]`.
    assert!(
        err.to_string().contains("locked_unit_price_minor"),
        "error should name the offending field, got: {err}"
    );
}

#[test]
fn rejects_a_band_top_that_is_neither_integer_nor_open() {
    let bad = GRADUATED.replace("to_qty   = \"open\"", "to_qty   = \"unbounded\"");

    let err = toml::from_str::<EvaluationCase>(&bad).expect_err("bad band top must be rejected");

    assert!(
        err.to_string().contains("unbounded"),
        "error should quote the bad value, got: {err}"
    );
}

#[test]
fn rejects_a_snapshot_field_smuggled_into_runtime() {
    // The boundary is checked in both directions. A band or a model kind under
    // `[runtime]` is as wrong as a runtime value under `[snapshot]`: the split
    // is the gear ownership boundary, not a convenience.
    let bad = format!("{GRADUATED}\n[runtime]\nmodel_kind = \"volume\"\n");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a snapshot field under runtime is rejected");

    assert!(
        err.to_string().contains("model_kind"),
        "error should name the offending field, got: {err}"
    );
}

#[test]
fn an_empty_runtime_section_is_fine() {
    // Phase-1 families need no consumer-side context beyond the quantity, so an
    // empty (or absent) `[runtime]` must stay legal.
    let ok = format!("{GRADUATED}\n[runtime]\n");

    toml::from_str::<EvaluationCase>(&ok).expect("an empty runtime section must parse");
}

#[test]
fn rejects_a_tier_aggregation_window_no_document_defines() {
    // `billing_period` is the value four cases carried: a plausible synonym of
    // `invoice_period` that reads as obviously correct and appears in no
    // enumeration. While the field was an `Option<String>` it loaded silently and
    // was only ever noticed by a consumer that could not map it.
    let bad = GRADUATED.replace("\"invoice_period\"", "\"billing_period\"");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a window outside inst-tb-window's enumeration must not load");

    assert!(
        err.to_string().contains("billing_period"),
        "error should quote the bad value, got: {err}"
    );
}

#[test]
fn rejects_a_billing_granularity_no_document_defines() {
    // `per_unit` is a `modelKind`, not a granularity - which is exactly why it
    // was an easy thing to write in this field and a hard thing to see.
    let bad = GRADUATED.replace("\"whole_unit\"", "\"per_unit\"");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a granularity outside inst-tb-window's enumeration must not load");

    assert!(
        err.to_string().contains("per_unit"),
        "error should quote the bad value, got: {err}"
    );
}

#[test]
fn charge_kind_is_required() {
    // The axis has no default, and that is the point of the field. A
    // `#[serde(default)]` would put the deleted inference back one layer down:
    // a case that forgot to say would load as `recurring`, and the whole matrix
    // `inst-mk-chargekind` states would be judged against a row nobody authored.
    let bad = GRADUATED.replace("charge_kind             = \"usage\"\n", "");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a snapshot without charge_kind must not load");

    assert!(
        err.to_string().contains("charge_kind"),
        "error should name the missing field, got: {err}"
    );
}

#[test]
fn rejects_a_charge_kind_no_document_defines() {
    // The fourth pinned vocabulary, for the same reason as the other three: an
    // axis of the canonical scope key cannot gain a value on one side only.
    let bad = GRADUATED.replace("\"usage\"", "\"metered\"");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a chargeKind outside the scope-key axis must not load");

    assert!(
        err.to_string().contains("metered"),
        "error should quote the bad value, got: {err}"
    );
}

#[test]
fn provenance_is_required() {
    let bad = GRADUATED.replace(r#"provenance = ["AC#60", "PRD 17.2", "D-17"]"#, "");

    let err = toml::from_str::<EvaluationCase>(&bad)
        .expect_err("a case without provenance must not parse");

    // The field, as every sibling rejection in this file does: the mutation deletes
    // a whole line from a shared literal, so any later edit that makes the fixture
    // invalid for an unrelated reason would satisfy a bare `expect_err` while
    // `provenance` quietly became optional.
    assert!(
        err.to_string().contains("provenance"),
        "error should name the missing field, got: {err}"
    );
}

#[test]
fn every_family_states_whether_it_is_a_registry_variant() {
    // The families **are** the variants, so the mapping has to be total and it
    // has to be exhaustive over `Family::ALL` -- a family added without deciding
    // what it gates is the shape of the original hole, one axis over.
    //
    // Exactly two families map to no variant, and both for a stated reason:
    // `proration` is AC #61 and gates nothing deliberately; `trailing-tier` is
    // Slice 10's `inst-tt-fixture` and carries no case, so registering it would
    // shut the gate permanently for every `trailing_period` row rather than
    // gate one.
    let without: Vec<Family> = Family::ALL
        .into_iter()
        .filter(|f| f.variant().is_none())
        .collect();

    assert_eq!(without, vec![Family::Proration, Family::TrailingTier]);
}

#[test]
fn the_four_model_kind_families_share_one_variant() {
    // `tier-boundary`, `package`, `per-unit` and `flat` are the kinds' own
    // fixtures and are therefore one variant across four families -- which is
    // why the variant cannot simply be the family name.
    for family in [
        Family::TierBoundary,
        Family::Package,
        Family::PerUnit,
        Family::Flat,
    ] {
        assert_eq!(family.variant(), Some(Variant::ModelKind), "{family:?}");
    }

    assert_eq!(
        Family::LevelAggregation.variant(),
        Some(Variant::LevelAggregation)
    );
    assert_eq!(
        Family::SupersessionContinuity.variant(),
        Some(Variant::SupersessionContinuity)
    );
    assert_eq!(Family::Reserved.variant(), Some(Variant::Reserved));
}

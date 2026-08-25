use super::*;
use bss_fixtures::{
    Case, Corpus, Family, FamilyMeta, GateRole, IntegrityViolation, ModelKind, Variant,
};

fn corpus() -> Corpus {
    Corpus::load(&Corpus::corpus_root()).expect("corpus loads")
}

/// The evaluation cases a registry row's `oracle` flag is a claim about: cases
/// of the row's **own** kind, in a family that maps to the row's variant and
/// gates that kind.
///
/// Derived from the corpus rather than restated beside it. A row whose flag rests
/// on a sibling kind's run, or on a publish case's `successor.model_kind`, has
/// none of this evidence — and asserting the flag directly cannot tell the two
/// apart, because a blanket `oracle` expectation is satisfied by the family-wide
/// verdict that produced the row in the first place.
fn evidence_for(corpus: &Corpus, kind: ModelKind, variant: Variant) -> Vec<&str> {
    corpus
        .families
        .iter()
        .filter(|meta| meta.family.variant() == Some(variant) && meta.gates.contains(&kind))
        .flat_map(|meta| corpus.cases_for(meta.family))
        .filter(|case| matches!(case, Case::Evaluation(_)) && case.model_kind() == kind)
        .map(Case::id)
        .collect()
}

/// The committed file's freshness is asserted **once**, and not here.
///
/// This crate can only compute one of the two halves the file records, so an
/// expectation built here would be an expectation over a registry with the
/// publish half nailed shut. The authority is the gear, which sees both:
/// `gears/bss/pricing/pricing/tests/corpus_publish.rs`.
#[test]
fn every_gated_kind_of_a_built_family_earns_its_oracle_flag() {
    let corpus = corpus();
    let registry = build(&corpus, &[]).expect("registry builds");

    // Every catalog kind, not a hand-kept subset: `check_kind_coverage`
    // guarantees each one is gated on its own `model_kind` variant, so each must
    // also earn its flag there.
    for kind in ModelKind::ALL {
        let v = registry
            .variants
            .iter()
            .find(|v| v.kind == kind && v.variant == Variant::ModelKind)
            .unwrap_or_else(|| panic!("{kind:?} must be registered on its own variant"));
        let evidence = evidence_for(&corpus, kind, Variant::ModelKind);
        assert!(
            !evidence.is_empty(),
            "{kind:?} has no evaluation case on its own model_kind fixture"
        );
        assert!(
            v.oracle,
            "{kind:?} must have earned its oracle flag from {evidence:?}"
        );
    }
}

#[test]
fn a_row_is_written_per_kind_and_variant() {
    // The registry is keyed `(kind, variant)` -- S3 design 6's `model_kind` /
    // `variant` pair -- and the cross-cutting families are what put more than
    // one row under a kind. Keyed by kind alone, `level-aggregation`,
    // `supersession-continuity` and `reserved` gated nothing at all.
    let corpus = corpus();
    let registry = build(&corpus, &[]).expect("registry builds");

    let has = |kind: ModelKind, variant: Variant| {
        registry
            .variants
            .iter()
            .any(|v| v.kind == kind && v.variant == variant)
    };

    assert!(has(ModelKind::Graduated, Variant::ModelKind));
    assert!(has(ModelKind::Graduated, Variant::LevelAggregation));
    assert!(has(ModelKind::Graduated, Variant::SupersessionContinuity));
    assert!(has(ModelKind::Graduated, Variant::Reserved));
    // D-22 scopes the continuity fixture to the tiered usage kinds, so `volume`
    // has one and `package` does not.
    //
    // The row's existence is the family's claim; the case named beneath it is
    // what makes the claim answerable. `supersession-continuity` gates both
    // tiered kinds, so a row here with only the `graduated` case behind it would
    // be `graduated`'s scenario wearing `volume`'s key -- and under Variant A the
    // continued counter is priced by a different formula, which is the whole
    // reason the pair is two rows and not one.
    assert!(has(ModelKind::Volume, Variant::SupersessionContinuity));
    assert_eq!(
        evidence_for(&corpus, ModelKind::Volume, Variant::SupersessionContinuity),
        ["volume-counter-continues-across-supersession"]
    );
    assert_eq!(
        evidence_for(
            &corpus,
            ModelKind::Graduated,
            Variant::SupersessionContinuity
        ),
        ["counter-continues-across-supersession"]
    );
    assert!(!has(ModelKind::Package, Variant::SupersessionContinuity));
    // No case folds a level on a `volume` row, so the pair is absent -- and an
    // absent pair is never open, which is what refuses a `peak` volume row.
    assert!(!has(ModelKind::Volume, Variant::LevelAggregation));
}

/// A family that maps to no variant contributes no row — and the guard that
/// makes that so is [`bss_fixtures::check_integrity`]'s, not the generator's own
/// `continue`.
///
/// Over the committed corpus the property is true by construction twice over:
/// `Variant::ALL` enumerates every variant, so a membership assertion over the
/// built rows holds whatever the loop wrote, and `proration` declares no `gates`,
/// so removing the skip would still write nothing. What can fail is the pair of
/// refusals below — a variant-less family that gates a kind never reaches the
/// loop, because `build` refuses the corpus ahead of it.
#[test]
fn a_family_that_maps_to_no_variant_cannot_gate_a_kind() {
    // `proration` is AC #61 and gates nothing. `trailing-tier` is in the same
    // state for a different reason (Slice 10 owns `inst-tt-fixture`, and the
    // family carries no case at all).
    assert_eq!(Family::Proration.variant(), None);
    assert_eq!(Family::TrailingTier.variant(), None);

    // `conformance` is the one role a variant-less family may hold, and it may
    // hold no gates.
    let gating_conformance = family_corpus(GateRole::Conformance);
    assert!(
        matches!(
            build(&gating_conformance, &[]),
            Err(GenError::Integrity(ref found))
                if found.contains(&IntegrityViolation::ConformanceFamilyGates {
                    family: Family::Proration,
                })
        ),
        "a conformance family that gates a kind must be refused, not skipped"
    );

    // The other door into the same state: `publish` obliges a variant, so a
    // variant-less family taking that role is refused rather than gating a kind
    // the registry has no `(kind, variant)` key to write under.
    let gating_publish = family_corpus(GateRole::Publish);
    assert!(
        matches!(
            build(&gating_publish, &[]),
            Err(GenError::Integrity(ref found))
                if found.contains(&IntegrityViolation::GatingFamilyWithoutVariant {
                    family: Family::Proration,
                })
        ),
        "a publish family with no variant must be refused, not skipped"
    );
}

/// A one-family corpus in which `proration` — which maps to no variant — gates a
/// kind, which is the state neither role may reach.
fn family_corpus(role: GateRole) -> Corpus {
    Corpus {
        cases: Vec::new(),
        families: vec![FamilyMeta {
            family: Family::Proration,
            role,
            gates: vec![ModelKind::Flat],
            provenance: vec!["AC#61".to_owned()],
        }],
    }
}

#[test]
fn the_gate_opens_only_where_both_halves_are_earned() {
    // `FixtureGate` reads `oracle && publish`. Opening the gate for a
    // `(kind, variant)` pair therefore means two separate runs have passed: the
    // reference oracle reproduced the family's evaluation cases, and pricing's
    // `PublishValidator` reproduced the kind's publish cases. Neither half alone
    // is publishable, and the `rating` half is deliberately not consulted --
    // requiring it would block every publish at launch, since the rating gear
    // has no code.
    let corpus = corpus();
    let oracle_only = build(&corpus, &[]).expect("registry builds");

    for v in &oracle_only.variants {
        // Earned, not merely set. The expectation is derived from the corpus: a
        // blanket `assert!(v.oracle)` is satisfied by whatever the generator
        // computed, so it cannot distinguish a flag the row's own kind earned
        // from one credited to it by a family-wide verdict.
        let evidence = evidence_for(&corpus, v.kind, v.variant);
        assert!(
            !evidence.is_empty(),
            "{:?}/{} carries a row with no evaluation case of its own kind behind it",
            v.kind,
            v.variant.wire()
        );
        assert!(
            v.oracle,
            "{:?}/{} must have earned its oracle flag from {evidence:?}",
            v.kind,
            v.variant.wire()
        );
        assert!(!v.publish, "{:?} was handed no publish half", v.kind);
        assert!(!v.rating, "{:?} must not claim a rating half yet", v.kind);
        assert!(
            !oracle_only.gate_open_for(v.kind, v.variant),
            "{:?}/{} must not open the gate on the oracle flag alone",
            v.kind,
            v.variant.wire()
        );
    }
}

#[test]
fn the_publish_half_is_recorded_exactly_as_handed_in() {
    // The generator does not know who ran what, and must not decide it either:
    // it records the set it is given, kind for kind. A kind absent from the
    // earned set stays shut even though its oracle half is green.
    //
    // Per **kind**, across every variant of it: an outcome is attributed to
    // `successor.model_kind` and to nothing else, so a failing publish case
    // blocks every fixture the kind has.
    let earned = build(&corpus(), &[ModelKind::Volume]).expect("registry builds");

    assert!(
        earned.gate_open_for(ModelKind::Volume, Variant::ModelKind),
        "volume earned both halves and must open"
    );
    assert!(
        earned.gate_open_for(ModelKind::Volume, Variant::SupersessionContinuity),
        "the publish half is earned per kind, so it reaches every variant of it"
    );
    for kind in ModelKind::ALL
        .into_iter()
        .filter(|k| *k != ModelKind::Volume)
    {
        for variant in Variant::ALL {
            assert!(
                !earned.gate_open_for(kind, variant),
                "{kind:?}/{} earned no publish half and must stay shut",
                variant.wire()
            );
        }
    }
}

#[test]
fn the_generated_header_names_the_command_that_regenerates_it() {
    // The header is the only instruction a reader of `registry.toml` gets, and
    // the command moved when the gear took ownership of the file. A header
    // naming a target that no longer exists is worse than none.
    let rendered = render_for(&corpus(), &[]).expect("registry renders");

    assert!(
        rendered.contains("cargo run -p cf-gears-bss-pricing --example regen_registry"),
        "the header must name the real regeneration command"
    );
}

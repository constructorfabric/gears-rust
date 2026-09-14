//! The gear's half of the joint conformance corpus: the publish cases, and the
//! generated `registry.toml` they earn a flag in.
//!
//! This file is the **single authority** on the committed registry's content.
//! `bss-fixtures-conformance` can compute only one of the two halves the file
//! records — it cannot reach the gear, by the corpus's own invariant — so an
//! expectation built there would be an expectation over a registry with the
//! publish half nailed shut. Two crates each asserting a different expected
//! content is how a generated file starts flapping.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../examples/regen_registry/validator.rs"]
mod validator;

use bss_fixtures::{Corpus, ModelKind, Registry, Variant};
use bss_fixtures_conformance::registry_gen;

fn corpus() -> Corpus {
    Corpus::load(&Corpus::corpus_root()).expect("corpus loads")
}

#[test]
fn the_committed_registry_is_fresh() {
    let corpus = corpus();
    let earned = validator::publish_report(&corpus).earned_kinds();
    let expected = registry_gen::render_for(&corpus, &earned).expect("registry renders");
    let committed = std::fs::read_to_string(Corpus::corpus_root().join("registry.toml"))
        .expect("committed registry exists");

    assert_eq!(
        committed, expected,
        "registry.toml is stale -- run `cargo run -p cf-gears-bss-pricing --example regen_registry` \
         and commit the regeneration on its own"
    );
}

#[test]
fn the_committed_publish_flags_are_exactly_what_a_run_earned() {
    // No flag is set by hand. The committed file must agree, kind for kind, with
    // what running the validator over the corpus produces right now -- otherwise
    // a `true` in the file is a claim nothing backs.
    let corpus = corpus();
    let earned = validator::publish_report(&corpus).earned_kinds();
    let committed = Registry::load(&Corpus::corpus_root().join("registry.toml"))
        .expect("committed registry parses");

    // Every row, not one per kind: the publish half is earned per kind and is
    // written onto each of that kind's variants, so a hand-edited `true` on a
    // single cross-cutting row would slip past a per-kind spot check.
    for row in &committed.variants {
        assert_eq!(
            row.publish,
            earned.contains(&row.kind),
            "the committed publish flag for {:?}/{} is not what a validator run earns",
            row.kind,
            row.variant.wire()
        );
    }
    for kind in ModelKind::ALL {
        assert!(
            committed
                .variants
                .iter()
                .any(|v| v.kind == kind && v.variant == Variant::ModelKind),
            "{kind:?} must be registered on its own model_kind variant"
        );
    }
}

/// Every publish case the gear does not answer as the corpus states, pinned.
///
/// **The disagreement is the product.** The corpus and the gear are two
/// independent readings of one design set, so where they differ a human decides
/// which is wrong — the expected value is never adjusted to make a run pass, and
/// neither is the rule. This list therefore exists to be *loud*: it fails the
/// moment either side moves, which is exactly when the decision is due.
///
/// It is empty, and each entry was closed by deciding which side was wrong:
///
/// - `price-change-accepted`, `unit-change-rejected`, `kind-flip-rejected`,
///   `carry-allowance-change-rejected`, `package-size-change-rejected` — the
///   gear answered `EVAL_POLICY_MISSING`. **The gear was right and the corpus
///   was under-authored.** None of those successors carried
///   `tierAggregationWindow`, which `inst-tb-window` requires on every tiered
///   usage row and `inst-pk-window` (D-58) on every `package` usage row. They
///   were authored as *deltas*, but the publish question is asked of a whole
///   row, and a row that would not publish on an empty key does not publish on
///   an occupied one either. Fixed in the corpus: both sides of each pair now
///   carry the window, equal, so the guard's subject is unchanged.
/// - `consumption-on-level-rejected` — the gear could not assess it at all: the
///   snapshot carried the Slice-10 reservation pair and `PriceRow` was the
///   Slice-3 shape. **The corpus was right and the gear was not there yet**, so
///   neither side moved; the corpus said so with `declined_until` on the case —
///   the `trailing-tier` treatment at case granularity — and a decline was the
///   anticipated answer rather than a disagreement.
///
///   **Closed on 2026-08-08**, when Slice 10 landed `reserved_rate_minor` /
///   `reservation_flavor` on the row and `domain::rules::reservation`. The
///   marker came off in the same commit, because it is self-retiring: leaving it
///   would fire `stale_decline()` and the corpus would be asserting that a built
///   slice is unbuilt. The case is **answered** now, not declined — which is what
///   the two tests below hold, and this entry is kept as the record of a closed
///   disagreement rather than a description of the corpus as it stands.
#[test]
fn the_open_corpus_disagreements_are_exactly_these() {
    let report = validator::publish_report(&corpus());

    let mut failing: Vec<&str> = report.failures().map(|o| o.case_id.as_str()).collect();
    failing.sort_unstable();

    assert_eq!(
        failing,
        Vec::<&str>::new(),
        "the corpus/gear disagreement set moved; a human decides which side was wrong \
         before this list is edited"
    );
}

#[test]
fn every_kind_has_earned_its_publish_half() {
    // The consequence of the list above, stated where the gate reads it. The gate
    // is `oracle && publish`, and both halves are now earned for all five kinds
    // by two runs with different owners.
    //
    // `flat` and `per_unit` earn theirs from publish cases that did not exist
    // before: the corpus carried none for either kind, and since the flag is
    // earned per kind by a passing run, a kind with no case could never earn it
    // -- the gate stayed shut forever rather than failing. That is now a build
    // failure (`check_publish_case_coverage`), not a silence.
    let corpus = corpus();
    let earned = validator::publish_report(&corpus).earned_kinds();

    assert_eq!(
        earned,
        ModelKind::ALL.to_vec(),
        "a kind lost its publish half; the disagreement list above is stale"
    );

    let committed = Registry::load(&Corpus::corpus_root().join("registry.toml"))
        .expect("committed registry parses");
    for kind in ModelKind::ALL {
        assert!(
            committed.gate_open_for(kind, Variant::ModelKind),
            "{kind:?} earned both halves and must open its own fixture's gate"
        );
    }
}

#[test]
fn the_reserved_case_is_answered_rather_than_declined_since_slice_10() {
    // **This case was `declined_until = "slice-10-advanced-primitives"`.** The
    // marker existed because `LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN` is Slice
    // 10's code while the only row shape was Slice 3's, which could hold neither
    // `reservedRate` nor `reservationFlavor` -- so no subject could reach the
    // rule to agree or disagree with it, and the honest answer was "undecidable".
    //
    // Slice 10 landed the pair and `domain::rules::reservation`, the marker came
    // out of the case file, and this asserts the thing the marker stood in for:
    // the gear now **answers** the case, and answers it with D-53's code rather
    // than with some other rejection that would happen to be red-free.
    let report = validator::publish_report(&corpus());

    let reserved = report
        .outcomes
        .iter()
        .find(|o| o.case_id == "consumption-on-level-rejected")
        .expect("the reserved publish case is in the corpus");

    let answer = validator::describe_answer(&reserved.actual);
    assert!(
        !answer.starts_with("undecidable"),
        "the pair is representable since Slice 10; the gear owes this case a verdict: {answer}"
    );
    assert!(
        answer.contains("LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN"),
        "and the verdict must be D-53's refusal rather than any other rejection: {answer}"
    );
}

#[test]
fn the_corpus_now_declines_nothing_and_nothing_is_stale() {
    // The other half. `declined()` was a one-element list for as long as the
    // reservation pair was unrepresentable; it is empty now, and
    // `stale_declines()` -- a case the corpus calls unanswerable that the gear
    // answers correctly -- must stay at zero, which is what catches a marker
    // left behind after its slice lands.
    let report = validator::publish_report(&corpus());

    let declined: Vec<&str> = report.declined().map(|o| o.case_id.as_str()).collect();
    assert!(
        declined.is_empty(),
        "no case is authored against an unbuilt slice any more: {declined:?}"
    );
    assert_eq!(report.stale_declines().count(), 0);
    assert!(
        validator::publish_report(&corpus())
            .earned_kinds()
            .contains(&ModelKind::Graduated)
    );
}

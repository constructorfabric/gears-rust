use super::*;
use crate::{CorpusEvaluator, EvalError, EvalInput, Evaluated, PublishValidator, ReferenceOracle};
use bss_fixtures::{Corpus, Family, ModelKind, PublishVerdict, Snapshot};

/// Claims everything but always answers 1 — stands in for a wrong evaluator.
struct AlwaysOne;

impl CorpusEvaluator for AlwaysOne {
    fn evaluate(&self, _input: &EvalInput<'_>) -> Result<Evaluated, EvalError> {
        Ok(Evaluated::Charge(1))
    }
    fn supported_families(&self) -> Vec<Family> {
        vec![
            Family::TierBoundary,
            Family::Package,
            Family::PerUnit,
            Family::Flat,
            Family::Proration,
            Family::SupersessionContinuity,
            Family::LevelAggregation,
            Family::Reserved,
        ]
    }
}

/// Claims nothing at all.
struct ClaimsNothing;

impl CorpusEvaluator for ClaimsNothing {
    fn evaluate(&self, _input: &EvalInput<'_>) -> Result<Evaluated, EvalError> {
        Ok(Evaluated::Charge(0))
    }
    fn supported_families(&self) -> Vec<Family> {
        Vec::new()
    }
}

fn corpus() -> Corpus {
    Corpus::load(&Corpus::corpus_root()).expect("corpus loads")
}

#[test]
fn the_oracle_is_green_on_every_family_it_claims() {
    let report = run_evaluation_suite(&ReferenceOracle, &corpus());

    assert_eq!(
        report.failures().count(),
        0,
        "oracle must reproduce the corpus"
    );
    // Every family the oracle **claims**, read off the claim rather than
    // re-listed here, and `is_green_for` is what makes that an assertion: it
    // returns its `seen` flag, so a family whose corpus cases were deleted is
    // not green while producing no failing outcome for the count above to see.
    //
    // Deliberately not the families the report exercised. `run_evaluation_suite`
    // skips every case whose family the evaluator does not claim, so those are a
    // subset of this roster by construction — a loop over them cannot fail, and
    // a roster narrowed by one drops that family out of both.
    let claimed = ReferenceOracle.supported_families();
    assert!(
        !claimed.is_empty(),
        "an evaluator claiming nothing satisfies every assertion below"
    );
    for family in claimed {
        assert!(
            report.is_green_for(family),
            "the oracle claims {family:?} and the corpus must hold cases proving it"
        );
    }
}

#[test]
fn a_wrong_evaluator_is_not_green() {
    let report = run_evaluation_suite(&AlwaysOne, &corpus());

    assert!(report.failures().count() > 0);
    assert!(!report.is_green_for(Family::TierBoundary));
}

#[test]
fn declining_a_family_is_recorded_and_is_not_green() {
    let report = run_evaluation_suite(&ClaimsNothing, &corpus());

    // Silence is not available: declining shows up as a declined family, and a
    // declined family is never green.
    assert!(report.declined.contains(&Family::TierBoundary));
    assert!(!report.is_green_for(Family::TierBoundary));
}

#[test]
fn a_family_with_no_assertions_is_not_green() {
    // Guards the hole where an empty family would vacuously pass.
    let empty = Corpus {
        cases: Vec::new(),
        families: Vec::new(),
    };
    let report = run_evaluation_suite(&ReferenceOracle, &empty);

    assert!(!report.is_green_for(Family::TierBoundary));
}

/// Answers whatever the corpus asked for. Stands in for a perfect validator, so
/// the runner's own bookkeeping can be tested without a gear.
struct EchoesTheCorpus;

impl PublishValidator for EchoesTheCorpus {
    fn validate(
        &self,
        _predecessor: &Snapshot,
        successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError> {
        // The corpus's only accepted publish case is the pure price change, and
        // it is the only one whose successor changes no unit field. Rather than
        // reimplement that, this stand-in answers from the one thing it can read
        // without a gear: `package` is the corpus's rejected block-size case,
        // everything else the accepted shape. It is deliberately crude -- its
        // job is to make the runner's pass/fail bookkeeping observable.
        if successor.model_kind == ModelKind::Package {
            return Ok(PublishVerdict::Rejected {
                error_code: "SUPERSESSION_UNIT_MISMATCH".to_owned(),
            });
        }
        Ok(PublishVerdict::Accepted)
    }
}

/// Declines every case it is handed, for the one sanctioned reason: it cannot
/// hold the row at all.
struct CannotAssess;

impl PublishValidator for CannotAssess {
    fn validate(
        &self,
        _predecessor: &Snapshot,
        _successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError> {
        Err(EvalError::UnrepresentableField {
            field: "everything",
            value: "this subject holds no row shape at all".to_owned(),
        })
    }
}

/// Fails every case for a reason that is not a decline: it can hold the row, it
/// just could not process this one.
struct FailsForAnUnrelatedReason;

impl PublishValidator for FailsForAnUnrelatedReason {
    fn validate(
        &self,
        _predecessor: &Snapshot,
        _successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError> {
        Err(EvalError::MissingField("dimension_key"))
    }
}

#[test]
fn the_publish_suite_runs_every_publish_case_and_no_evaluation_case() {
    let corpus = corpus();
    let report = run_publish_suite(&EchoesTheCorpus, &corpus);

    let publish_assertions: usize = corpus
        .cases
        .iter()
        .filter_map(|c| match c {
            bss_fixtures::Case::Publish(p) => Some(p.assert.len()),
            bss_fixtures::Case::Evaluation(_) => None,
        })
        .sum();

    assert!(
        publish_assertions > 0,
        "the corpus must carry publish cases"
    );
    assert_eq!(report.outcomes.len(), publish_assertions);
}

/// Answers the corpus's one `declined_until` case the way the corpus states.
///
/// Stands in for the day Slice 10 lands: a subject that can represent the
/// reservation pair and applies `inst-rv-level` to it.
struct HasBuiltSliceTen;

impl PublishValidator for HasBuiltSliceTen {
    fn validate(
        &self,
        _predecessor: &Snapshot,
        successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError> {
        if successor.reservation_flavor == Some(bss_fixtures::ReservationFlavor::Consumption) {
            return Ok(PublishVerdict::Rejected {
                error_code: "LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN".to_owned(),
            });
        }
        Ok(PublishVerdict::Accepted)
    }
}

/// Only the publish cases the corpus does **not** declare undecidable.
fn answerable_publish_cases(corpus: &Corpus) -> usize {
    corpus
        .cases
        .iter()
        .filter_map(|c| match c {
            bss_fixtures::Case::Publish(p) if p.declined_until.is_none() => Some(p.assert.len()),
            bss_fixtures::Case::Publish(_) | bss_fixtures::Case::Evaluation(_) => None,
        })
        .sum()
}

/// The id of the synthesised case in [`corpus_with_one_declined_case`].
///
/// Deliberately not `consumption-on-level-rejected`: that case exists in the
/// committed corpus *without* a marker, and reusing its id here would read as if
/// the corpus still declined it.
const SYNTHESISED_DECLINED_ID: &str = "synthesised-declined-until";

/// The committed corpus plus one publish case that carries `declined_until`.
///
/// **The committed corpus declines nothing.** `consumption-on-level-rejected`
/// lost its `declined_until = "slice-10-advanced-primitives"` on 2026-08-08, when
/// Slice 10 landed `reserved_rate_minor` / `reservation_flavor` on the row and the
/// rules that judge them: the pair became representable, so no subject can
/// honestly answer "undecidable" any more. The gear side pins exactly that state
/// -- `corpus_publish.rs`'s `the_corpus_now_declines_nothing_and_nothing_is_stale`
/// -- and it is expected to stay true.
///
/// So every property *about the marker* is shown over a corpus that carries one by
/// construction. The alternative -- asserting that `declined()` and
/// `stale_declines()` are empty against the committed set -- would turn the tests
/// green by deleting the only coverage the two mechanisms have, and would then go
/// quiet rather than red the next time a marker comes or goes.
///
/// The marked case is the reserved pair cloned under its own id, so the whole
/// committed corpus still runs beside it. That is load-bearing: the mechanisms
/// must hold *among* unmarked cases -- a declined case recorded while its
/// neighbours are counted -- not merely in isolation, where "no failures at all"
/// would satisfy the same assertions vacuously.
fn corpus_with_one_declined_case() -> Corpus {
    let mut corpus = corpus();

    let mut marked = corpus
        .cases
        .iter()
        .find_map(|c| match c {
            bss_fixtures::Case::Publish(p) if p.id == "consumption-on-level-rejected" => {
                Some(p.clone())
            }
            bss_fixtures::Case::Publish(_) | bss_fixtures::Case::Evaluation(_) => None,
        })
        .expect("the reserved publish case is in the corpus");

    marked.id = SYNTHESISED_DECLINED_ID.to_owned();
    marked.declined_until = Some("slice-10-advanced-primitives".to_owned());
    corpus.cases.push(bss_fixtures::Case::Publish(marked));

    corpus
}

#[test]
fn a_declined_case_never_earns_a_kind() {
    // Failing rather than guessing is the sanctioned answer, and it is still not
    // evidence that the rule holds -- so it earns nothing.
    //
    // The corpus is synthesised because the committed one declines nothing -- see
    // `corpus_with_one_declined_case`. Without a marked case the count below
    // compares every publish case against every publish case, and the clause it
    // exists to check does not run.
    let corpus = corpus_with_one_declined_case();
    let report = run_publish_suite(&CannotAssess, &corpus);

    // Every case the corpus expects an answer to is a failure. The ones it
    // declares undecidable are not: refusing those is agreement.
    assert_eq!(answerable_publish_cases(&corpus), report.failures().count());
    assert!(report.earned_kinds().is_empty());
}

#[test]
fn a_case_the_corpus_declines_is_recorded_rather_than_counted() {
    // The `trailing-tier` reading at case granularity: an unbuilt slice reads as
    // declined -- recorded, never green, and never mistaken for a fault of the
    // subject that honestly could not represent the row.
    //
    // The committed corpus declines nothing since 2026-08-08, so the declining
    // case is synthesised -- see `corpus_with_one_declined_case`. This is a
    // property of the runner's bookkeeping, not of today's corpus contents.
    let report = run_publish_suite(&CannotAssess, &corpus_with_one_declined_case());

    let declined: Vec<&str> = report.declined().map(|o| o.case_id.as_str()).collect();

    assert_eq!(declined, vec![SYNTHESISED_DECLINED_ID]);
    // And the neighbours it ran beside were counted rather than recorded: the
    // marker suspends its own case and nothing else.
    assert!(report.failures().count() > 0);
    assert!(report.failures().all(|o| o.declined_until.is_none()));
}

#[test]
fn a_declined_case_answered_with_the_wrong_verdict_is_still_a_failure() {
    // `declined_until` suspends nothing about the verdict. `EchoesTheCorpus`
    // answers `accepted` where the corpus states a rejection, and that is a
    // disagreement whatever the corpus said about buildability -- otherwise the
    // marker would be a way to make a case unfalsifiable.
    //
    // Synthesised for the same reason as its neighbours: with no marked case in
    // the committed corpus this would assert only that a wrong answer is a
    // failure, which is a different and much weaker claim.
    let report = run_publish_suite(&EchoesTheCorpus, &corpus_with_one_declined_case());

    assert!(
        report
            .failures()
            .any(|o| o.case_id == SYNTHESISED_DECLINED_ID),
        "a declined case answered wrongly must stay red"
    );
}

#[test]
fn answering_a_declined_case_correctly_marks_the_declaration_stale() {
    // The marker is self-retiring. A subject that reproduces the case has built
    // the slice, so "nothing can answer this yet" has stopped being true and the
    // line owes the registry its evidence again.
    //
    // This is the mechanism that retired the real marker: `HasBuiltSliceTen`
    // stands in for the gear the day Slice 10 landed, and the committed corpus has
    // carried no marker since. The case is therefore synthesised -- see
    // `corpus_with_one_declined_case` -- so the self-retirement stays covered now
    // that the corpus it once fired on has moved on.
    let report = run_publish_suite(&HasBuiltSliceTen, &corpus_with_one_declined_case());

    let stale: Vec<&str> = report
        .stale_declines()
        .map(|o| o.case_id.as_str())
        .collect();

    assert_eq!(stale, vec![SYNTHESISED_DECLINED_ID]);
    assert_eq!(report.declined().count(), 0);
}

#[test]
fn a_kind_with_no_publish_case_earns_nothing() {
    // The same clause `is_green_for` carries, one axis over: absent coverage must
    // never read as success. The committed corpus no longer has such a kind --
    // `check_publish_case_coverage` refuses one -- so the property is shown over
    // a corpus cut down to a single `graduated` case.
    let full = corpus();
    let one_case = Corpus {
        cases: full
            .cases
            .iter()
            .filter(|c| c.id() == "price-change-accepted")
            .cloned()
            .collect(),
        families: Vec::new(),
    };
    let report = run_publish_suite(&EchoesTheCorpus, &one_case);
    let earned = report.earned_kinds();

    assert_eq!(earned, vec![ModelKind::Graduated]);
    assert!(!earned.contains(&ModelKind::Flat));
    assert!(!earned.contains(&ModelKind::PerUnit));
}

#[test]
fn an_empty_corpus_earns_nothing() {
    let empty = Corpus {
        cases: Vec::new(),
        families: Vec::new(),
    };
    let report = run_publish_suite(&EchoesTheCorpus, &empty);

    assert!(report.outcomes.is_empty());
    assert!(report.earned_kinds().is_empty());
}

#[test]
fn the_unbuilt_families_are_declined_not_green() {
    // `trailing-tier` is the one family left unbuilt, and deliberately: SEAMS
    // M12 is open, so rating has no counterpart *behaviour* for
    // `tierQualificationWindow`. A fixture would pin one side of a contract the
    // other has not accepted. It must read as declined, never as green.
    //
    // "No counterpart behaviour", not "no counterpart at all" — the stronger
    // phrasing was false by one site. `rating/docs/design/09-period-plan-change.md`
    // names the field, as one of the nine D-82/D-98/D-122 preserved unit fields
    // its carry check compares for equality. That is an opaque-value comparison,
    // not a rating-side reading of what the window means, so the conclusion here
    // is unaffected and the family is still correctly declined. M12's own
    // phrasing is the precise one: it scopes the zero-reference claim to rating's
    // PRD, slices 03/13/14 and its register, all four of which are genuinely 0.
    let report = run_evaluation_suite(&ReferenceOracle, &corpus());

    let f = Family::TrailingTier;
    assert!(report.declined.contains(&f), "{f:?} must be declined");
    assert!(!report.is_green_for(f), "{f:?} must not be green");
}

#[test]
fn only_an_unrepresentable_row_buys_the_decline() {
    // The hole this closes: a subject that has built the declined slice, decided
    // its rule the opposite way, and then trips on any unrelated defect in the
    // same row would -- if every error counted as a decline -- stay green
    // forever. Its disagreement is never answered, so the staleness check, which
    // fires only on an `Ok`, never fires either. Declining is "I cannot hold this
    // row"; anything else is a fault and is counted as one.
    //
    // The corpus is synthesised -- see `corpus_with_one_declined_case` -- because
    // the committed one declines nothing: over it both assertions below hold with
    // no marked case present at all, which is not the claim. The point is that a
    // case the corpus *does* declare undecidable is still counted as a failure
    // when the subject's error is not a decline.
    let report = run_publish_suite(&FailsForAnUnrelatedReason, &corpus_with_one_declined_case());

    assert_eq!(
        report.declined().count(),
        0,
        "an unrelated error is not a decline"
    );
    assert_eq!(
        report.failures().count(),
        report.outcomes.len(),
        "every case it could not answer is a failure, declared undecidable or not"
    );
}

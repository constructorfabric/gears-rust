//! Runs the whole corpus against a conformance subject.
//!
//! There is no per-case filter on purpose. The only way to decline work is to
//! omit a family from `supported_families`, which is recorded and is never
//! green — so a subject cannot quietly skip the family it finds inconvenient.
//!
//! Two suites, because the corpus asks two questions of two different subjects:
//! [`run_evaluation_suite`] drives a [`CorpusEvaluator`] over the evaluation
//! cases, and [`run_publish_suite`] drives a [`PublishValidator`] over the
//! publish cases. Both live here rather than with their subjects: the runner is
//! corpus machinery, and a subject that ran its own cases would be free to
//! choose which ones.

use crate::traits::{CorpusEvaluator, EvalError, EvalInput, Evaluated, PublishValidator};
use bss_fixtures::{Case, Corpus, Expect, Family, ModelKind, PublishVerdict};

#[derive(Debug)]
pub struct Outcome {
    pub case_id: String,
    pub family: Family,
    /// The snapshot's `modelKind` — the row the case prices, and therefore the
    /// registry kind this outcome is evidence about.
    ///
    /// The evaluation twin of [`PublishOutcome::kind`]. A family's `gates` list
    /// names several kinds, so a family-wide verdict cannot say which of them the
    /// corpus actually exercised.
    pub kind: ModelKind,
    pub index: usize,
    pub expected: Expect,
    pub actual: Result<Evaluated, EvalError>,
}

impl Outcome {
    #[must_use]
    pub fn passed(&self) -> bool {
        match (&self.actual, self.expected) {
            (Ok(Evaluated::Charge(got)), Expect::Charge(want)) => *got == want.charge_minor,
            (
                Ok(Evaluated::Units {
                    charged, in_basis, ..
                }),
                Expect::Units(want),
            ) => *charged == want.units_charged && *in_basis == want.units_in_basis,
            (Ok(Evaluated::Fold { q }), Expect::Fold(want)) => *q == want.folded_q,
            // A subject answering the wrong shape has misunderstood the case,
            // not merely got the number wrong.
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
    pub declined: Vec<Family>,
}

impl Report {
    pub fn failures(&self) -> impl Iterator<Item = &Outcome> {
        self.outcomes.iter().filter(|o| !o.passed())
    }

    /// Green means: claimed, non-empty, and every assertion reproduced.
    ///
    /// An empty family is **not** green. Without that clause absent coverage
    /// would report as success, which is the exact failure the corpus exists to
    /// prevent.
    #[must_use]
    pub fn is_green_for(&self, family: Family) -> bool {
        if self.declined.contains(&family) {
            return false;
        }
        let mut seen = false;
        for o in self.outcomes.iter().filter(|o| o.family == family) {
            seen = true;
            if !o.passed() {
                return false;
            }
        }
        seen
    }

    /// Green for one `(family, kind)` pair — the granularity a registry row has.
    ///
    /// A family gates several kinds and its cases exercise them one at a time, so
    /// a family-wide verdict credits every gated kind with whatever evidence any
    /// one of them produced. A row keyed `(kind, variant)` claims something
    /// narrower: that the oracle reproduced **this kind's** cases in this
    /// family's scenario.
    ///
    /// The same three clauses as [`Self::is_green_for`] — claimed, non-empty,
    /// every assertion reproduced — with non-empty read per kind. An empty pair
    /// is refused here as well as by the corpus's own `GatedKindUncovered`,
    /// because a caller may hold a partial corpus that check never sees.
    #[must_use]
    pub fn is_green_for_kind(&self, family: Family, kind: ModelKind) -> bool {
        if self.declined.contains(&family) {
            return false;
        }
        let mut seen = false;
        for o in self
            .outcomes
            .iter()
            .filter(|o| o.family == family && o.kind == kind)
        {
            seen = true;
            if !o.passed() {
                return false;
            }
        }
        seen
    }
}

pub fn run_evaluation_suite<E: CorpusEvaluator>(evaluator: &E, corpus: &Corpus) -> Report {
    let claimed = evaluator.supported_families();
    let mut outcomes = Vec::new();

    for case in &corpus.cases {
        if !claimed.contains(&case.family()) {
            continue;
        }
        // Publish cases are answered by a `PublishValidator`, which only the
        // pricing gear implements; they are not this suite's business.
        let Case::Evaluation(case) = case else {
            continue;
        };
        for (index, a) in case.assert.iter().enumerate() {
            let input = EvalInput {
                snapshot: &case.snapshot,
                runtime: &case.runtime,
                given: &a.given,
            };
            outcomes.push(Outcome {
                case_id: case.id.clone(),
                family: case.family,
                kind: case.snapshot.model_kind,
                index,
                expected: a.expect,
                actual: evaluator.evaluate(&input),
            });
        }
    }

    let declined = Family::ALL
        .iter()
        .copied()
        .filter(|f| !claimed.contains(f))
        .collect();

    Report { outcomes, declined }
}

/// One publish assertion answered by a [`PublishValidator`].
#[derive(Debug)]
pub struct PublishOutcome {
    pub case_id: String,
    pub family: Family,
    pub index: usize,
    /// The successor's `modelKind` — the row under test, and therefore the
    /// registry variant this outcome is evidence about.
    pub kind: ModelKind,
    pub expected: PublishVerdict,
    pub actual: Result<PublishVerdict, EvalError>,
    /// The case's own `declined_until`, carried through so the report can tell
    /// "no subject has built this slice" apart from "the subject got it wrong".
    pub declined_until: Option<String>,
}

impl PublishOutcome {
    /// Did the subject answer the verdict the corpus states?
    ///
    /// An [`EvalError`] never passes. Declining a case is a legitimate answer —
    /// a subject must fail rather than guess — but it is not evidence that the
    /// rule holds, so it cannot earn a flag.
    ///
    /// Unchanged by `declined_until`, deliberately: a case the corpus marks
    /// unanswerable is still judged the moment a subject answers it.
    #[must_use]
    pub fn passed(&self) -> bool {
        matches!(&self.actual, Ok(verdict) if *verdict == self.expected)
    }

    /// The corpus said nothing could answer this yet, and the subject declined
    /// it **for the sanctioned reason**.
    ///
    /// The anticipated state — the `trailing-tier` reading at case granularity.
    /// It is not a pass and never earns a flag; it is recorded so that absent
    /// evidence stays visible instead of reading as either success or fault.
    ///
    /// Only [`EvalError::UnrepresentableField`] buys the suspension, and the
    /// narrowness is the point. Accepting any error at all would let a subject
    /// hold a live disagreement with the case and stay green forever: build the
    /// declined slice, decide the case's rule the opposite way, and then fail on
    /// any unrelated defect in the same row — the disagreement is never
    /// answered, so the staleness check (which fires only on an `Ok`) never
    /// fires either, and the failure is filed as an anticipated decline. The
    /// only honest decline is "I cannot hold this row"; "I could not process it"
    /// is a fault and is reported as one.
    #[must_use]
    pub fn anticipated_decline(&self) -> bool {
        self.declined_until.is_some()
            && matches!(&self.actual, Err(EvalError::UnrepresentableField { .. }))
    }

    /// The corpus said nothing could answer this yet, and something did.
    ///
    /// Loud on purpose: the slice landed, so the declaration is stale and the
    /// case owes the registry its evidence again. A `declined_until` that
    /// outlives its slice would suppress a real result forever.
    #[must_use]
    pub fn stale_decline(&self) -> bool {
        self.declined_until.is_some() && self.actual.is_ok()
    }
}

#[derive(Debug)]
pub struct PublishReport {
    pub outcomes: Vec<PublishOutcome>,
}

impl PublishReport {
    /// Cases the subject answered differently from the corpus.
    ///
    /// An anticipated decline is not one: the corpus itself says the case is
    /// unanswerable, so refusing it is agreement, not disagreement. Everything
    /// else — including a *declined* case the subject answered with the wrong
    /// verdict — is here.
    pub fn failures(&self) -> impl Iterator<Item = &PublishOutcome> {
        self.outcomes
            .iter()
            .filter(|o| !o.passed() && !o.anticipated_decline())
    }

    /// Cases suspended because no subject has built their slice.
    pub fn declined(&self) -> impl Iterator<Item = &PublishOutcome> {
        self.outcomes.iter().filter(|o| o.anticipated_decline())
    }

    /// Declarations that have outlived their slice — see
    /// [`PublishOutcome::stale_decline`].
    pub fn stale_declines(&self) -> impl Iterator<Item = &PublishOutcome> {
        self.outcomes.iter().filter(|o| o.stale_decline())
    }

    /// The kinds whose `publish` half this run **earns**.
    ///
    /// Same rule as [`Report::is_green_for`], one axis over: non-empty and every
    /// assertion reproduced. A kind the corpus carries no publish case for earns
    /// nothing — absent coverage must never read as success, which is the whole
    /// reason the corpus exists.
    ///
    /// An anticipated decline is skipped entirely: it neither earns the kind nor
    /// blocks it, because it is not evidence in either direction. A kind whose
    /// *only* publish cases are declined is therefore still unearned — and
    /// `check_publish_case_coverage` refuses that corpus outright, so the state
    /// cannot be reached quietly.
    ///
    /// ## The `GateRole` coupling is deliberate
    ///
    /// A publish outcome is attributed to `successor.model_kind` and to nothing
    /// else. It is **not** filtered by the case's family, nor by whether that
    /// family's `GateRole` is `Publish`, nor by whether the family's `gates`
    /// list names the kind. So a failing case in a `Conformance` family would
    /// block the kind its successor carries, even though that family gates no
    /// publish at all.
    ///
    /// **That crossing does not happen in the committed corpus.** Every publish
    /// case sits in `supersession-continuity` or `reserved`, and both declare
    /// `role = "publish"`; the one `Conformance` family, `proration`, carries no
    /// publish case. So the coupling is inert today and the decision below is
    /// unexercised — which is the reason to state it here rather than to infer it
    /// from behaviour nothing currently produces.
    ///
    /// **This is the chosen behaviour: any failing publish case blocks its
    /// kind.** A failed case is a rule of the design set the gear does not
    /// reproduce, and which family file it was filed under does not make it less
    /// so. Attribution follows the row under test, which is what a `modelKind`
    /// flag is a claim about; a `graduated` supersession the gear gets wrong is
    /// evidence against publishing `graduated`, wherever the case sits.
    ///
    /// The alternative was to scope the earning to a kind's gating families —
    /// count an outcome only when some family with `GateRole::Publish` lists the
    /// kind in `gates` *and* the case belongs to it. That is more precise about
    /// what a gate means and strictly less safe: on the corpus as it stands it
    /// would earn **nothing** for any kind, because no `Publish` family carries
    /// a publish case, and it would let a real disagreement in
    /// `supersession-continuity` sit beside an open gate. Fail-closed and
    /// slightly over-broad beats precise and permissive here — the cost of the
    /// choice is that a gate can be held shut by a case about a rule its own
    /// family does not gate, and a reader who meets that should read it here
    /// rather than diagnose it.
    #[must_use]
    pub fn earned_kinds(&self) -> Vec<ModelKind> {
        ModelKind::ALL
            .into_iter()
            .filter(|kind| {
                let mut seen = false;
                for outcome in self
                    .outcomes
                    .iter()
                    .filter(|o| o.kind == *kind && !o.anticipated_decline())
                {
                    seen = true;
                    if !outcome.passed() {
                        return false;
                    }
                }
                seen
            })
            .collect()
    }
}

/// Drives `validator` over every publish case in `corpus`.
///
/// No family filter and no per-case filter: [`PublishValidator`] has no
/// `supported_families` to decline with, because the publish half has exactly
/// one implementor and a gear that could skip its own cases would be grading its
/// own paper.
pub fn run_publish_suite<V: PublishValidator>(validator: &V, corpus: &Corpus) -> PublishReport {
    let mut outcomes = Vec::new();

    for case in &corpus.cases {
        let Case::Publish(case) = case else {
            continue;
        };
        for (index, assertion) in case.assert.iter().enumerate() {
            outcomes.push(PublishOutcome {
                case_id: case.id.clone(),
                family: case.family,
                index,
                kind: case.successor.model_kind,
                expected: assertion.expect.clone(),
                actual: validator.validate(&case.predecessor, &case.successor),
                declined_until: case.declined_until.clone(),
            });
        }
    }

    PublishReport { outcomes }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod runner_tests;

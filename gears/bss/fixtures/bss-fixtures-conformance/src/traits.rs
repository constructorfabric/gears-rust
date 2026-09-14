//! What a conformance subject must implement.

use bss_fixtures::{Family, ModelKind, PublishVerdict, Runtime, Snapshot};

/// One evaluation point: the frozen row, the consumer-supplied context, and the
/// per-assertion input.
pub struct EvalInput<'a> {
    pub snapshot: &'a Snapshot,
    pub runtime: &'a Runtime,
    pub given: &'a bss_fixtures::Given,
}

/// What a subject answers.
///
/// Two shapes because the corpus asks two questions. The charge families ask
/// what something costs and get an integer minor amount. Proration asks what
/// share of a period is chargeable and gets an exact unit ratio — never money,
/// because rating emits prorated components at full precision and Billing does
/// the rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evaluated {
    Charge(i64),
    Units { charged: u64, in_basis: u64 },
    Fold { q: u64 },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EvalError {
    #[error("model kind {0:?} is not supported by this evaluator")]
    UnsupportedModelKind(ModelKind),
    #[error("required snapshot field `{0}` is absent")]
    MissingField(&'static str),
    /// A snapshot field the subject's own shape cannot hold at all — an
    /// out-of-vocabulary value, or a field belonging to a slice the subject has
    /// not built.
    ///
    /// Distinct from [`EvalError::MissingField`] on purpose: absent and
    /// unrepresentable are different failures. The first says the case did not
    /// supply something; the second says the subject cannot receive it, which is
    /// the only honest way to decline a case rather than answer it wrongly.
    #[error("snapshot field `{field}` holds {value}, which this subject cannot represent")]
    UnrepresentableField { field: &'static str, value: String },
    #[error("required given field `{0}` is absent")]
    MissingGiven(&'static str),
    #[error("no band covers quantity {0}")]
    NoBandCoversQuantity(u64),
    #[error("package_size must be greater than zero")]
    ZeroPackageSize,
    #[error("quantity {0} does not fit the money domain")]
    QuantityOutOfRange(u64),
    #[error("the period is empty or inverted")]
    DegeneratePeriod,
    /// The chargeable stretch is not inside the period it is apportioned over.
    ///
    /// Distinct from [`EvalError::DegeneratePeriod`] on purpose: the period is
    /// well-formed and the stretch is well-formed, and the pair is still
    /// unanswerable — apportioning a stretch that leaves the period yields a
    /// factor above 1, which is not a share of anything.
    #[error("the chargeable stretch is not contained in the period it is apportioned over")]
    StretchOutsidePeriod,
    /// A product or sum the evaluator's integer domain cannot hold, named by the
    /// computation that overflowed.
    ///
    /// A refusal rather than a wrap: this is the reference implementation of PRD
    /// §17.2 and the corpus is the contract a second evaluator must reproduce, so
    /// an answer nobody can derive by hand is worse than no answer. In a release
    /// build the alternative wraps to a positive-looking number; in a debug build
    /// it panics.
    #[error("`{what}` does not fit the evaluator's integer domain")]
    ArithmeticOverflow { what: &'static str },
    #[error("`sum` is not a granule fold")]
    SumIsNotAFold,
    #[error(
        "a fold of {level_seconds} level-seconds does not divide by {step} into a whole billable unit; rounding is Billing's, not this seam's"
    )]
    NonIntegralFold { level_seconds: u64, step: u64 },
    #[error(
        "a window of {span} seconds does not divide by {step} into whole granules; the trailing partial granule is not this seam's to drop"
    )]
    PartialGranuleWindow { span: u64, step: u64 },
}

/// Implemented by the reference oracle now and by the rating gear later. Both
/// must reproduce the same corpus; when they disagree the corpus goes red for
/// both rather than either side overriding the other.
pub trait CorpusEvaluator {
    /// Answers one assertion.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError`] when the case cannot be answered — an unsupported
    /// kind, an absent required field, or a quantity no band covers. A subject
    /// must fail rather than guess.
    fn evaluate(&self, input: &EvalInput<'_>) -> Result<Evaluated, EvalError>;

    /// Families this subject claims. Omitting a family is the only legitimate
    /// way to decline it — and is identical in effect to not being green, which
    /// blocks publish of the variants that family gates. Silence is not
    /// available.
    fn supported_families(&self) -> Vec<Family>;
}

/// Answers the corpus's publish cases: given an authored successor landing on a
/// predecessor's canonical scope key, what does publish do?
///
/// Implemented by the **pricing gear**, and by nothing else. The reference
/// oracle deliberately does not implement it: reproducing the gear's validation
/// surface would mean checking the gear against a copy of the gear, which tests
/// nothing and doubles the maintenance.
///
/// That asymmetry is why the registry's two halves are earned from different
/// places. The `oracle` half is earned here; the `publish` half is earned by the
/// gear running [`crate::run_publish_suite`] over its own validator, which it
/// does from an `example` target — the one build in which both this crate and
/// the gear are visible without this crate entering the gear's production graph.
pub trait PublishValidator {
    /// # Errors
    ///
    /// Returns [`EvalError`] only when the case cannot be assessed at all. A
    /// rejection by the rules under test is a [`PublishVerdict`], not an error.
    fn validate(
        &self,
        predecessor: &Snapshot,
        successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError>;
}

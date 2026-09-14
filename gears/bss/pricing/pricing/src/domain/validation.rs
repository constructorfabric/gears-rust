//! The fail-closed validation pipeline framework.
//!
//! The Foundation owns the *mechanism*; slices own the *rules*. A slice
//! registers [`ValidationRule`] implementations and the pipeline runs the whole
//! set, collecting every outcome rather than stopping at the first failure —
//! the aggregate report is what makes a plan remediable in one pass.
//!
//! Two properties are normative rather than convenient
//! (`design/01-foundation.md` §4.2):
//!
//! - **A single blocking violation blocks the publish.** There is no severity
//!   below "blocking" that still publishes; advisory findings are warnings and
//!   carry no veto.
//! - **The same rule set runs twice**: as a pre-check at submit, and again
//!   inside the publish-commit transaction. Approval approves *content*; the
//!   commit re-validates *state*, because the world moved between the two. A
//!   commit-time failure voids the approval and returns the subject to draft
//!   with the report — so a rule must be pure with respect to the state it is
//!   handed, and must not carry results between the two runs.

use std::fmt;

use toolkit_macros::domain_model;

/// When a violation can first be judged — D-312.
///
/// The rule set runs in full at the publish pre-check and again inside the
/// publish commit; that is unchanged and normative (§4.2). What this adds is a
/// second, *earlier* place a **subset** of the same violations is also judged:
/// the price authoring write.
///
/// The subset is not "the cheap rules" or "the important ones". A violation is
/// [`Stage::Write`] only when **every operand of its fault is present in the
/// request**: such a request is complete, self-inconsistent and knowably
/// unpublishable the instant it arrives, so refusing it at the write costs an
/// author nothing they could legitimately have wanted. The strongest case is a
/// fault one of whose operands is an immutable component of the scope key —
/// `chargeKind` above all, the key being uneditable after create — because then
/// the only call that resolves it retracts the field just sent rather than adding
/// anything.
///
/// Everything not stamped is [`Stage::Publish`], and that includes the family
/// that was never in question:
///
/// - **An absent operand** — no `model_kind` yet, no bands yet, no amount yet.
///   These are completed by a later call, which is the multi-call authoring
///   §4.2 exists to protect.
///
/// **The criterion has one half and no mechanical
/// replacement**, which is why the paragraph above is weaker than the one it
/// replaced. It read *"every operand of its fault is present in the request **and one of
/// them is an immutable component of the scope key**"*, and it named a second
/// publish-only family beside the absent operand: **content contradicting
/// content** — tier bands on a `flat` row, both operands mutable, resolved by
/// `model_kind: graduated` adding intent rather than by retracting.
///
/// D-312's amendment withdrew that exception and moved both arms of
/// `inst-mk-forbidden` to the write: bands on a `flat` row, and a
/// `quantitySource` on a non-usage row whose kind is not `per_unit`
/// ([`rules::model_kind`](crate::domain::rules::model_kind)). Neither is decided
/// by a frozen-key operand and neither resolves by retraction — a later
/// `model_kind` resolves both. The argument is that the state the exception
/// protected is not reachable and that the schema cannot be what decides: a probe
/// found `PATCH`ing bands onto a stored `flat` row answers 500 by the same trigger
/// that refuses the pair in one call, while the `quantitySource` pair was stored
/// happily and read back as authorable. The schema catches some invalid
/// combinations and not others, so the rule has to be what decides.
///
/// **What did not follow was a sweep, and a rule author has to know that.**
/// Present-operand completeness is necessary and is *not* sufficient here: several
/// content-against-content faults are still [`Stage::Publish`], each argued at its
/// own rule rather than by this criterion —
/// [`rules::level_aggregation`](crate::domain::rules::level_aggregation)'s
/// `aggregationGranularity` and `maxHold` on a usage `sum` row,
/// [`rules::package`](crate::domain::rules::package)'s `package_size = 0` and its
/// bands beside blocks, `manual_quantity` without `quantitySource = manual`, and
/// `AMOUNT_PLACEMENT_INVALID` for money beside a band ladder. Each rests on the
/// same "a later call adds the intent" reading the amendment rejected for the two
/// arms above; whether it reaches them is the owner's question, not one to settle
/// by reading a criterion. **So do not infer a stage from this section** — read
/// the rule, and if you are adding one, say at the call site which of the two it
/// is and why. That is what the two `model_kind` arms and
/// [`rules::allowance`](crate::domain::rules::allowance)'s saturating-offset arm
/// do, and it is the only account of the split this doc can keep true.
///
/// **The stage belongs to the violation, not to the rule** — with one exception,
/// named below because an unnamed exception to a rule stated this flatly is how the
/// next reader decides the code is wrong.
///
/// Most of the rules that emit a write-stage violation also emit publish-stage
/// ones, and `EVAL_POLICY_MISPLACED` is the code for both a key contradiction and
/// a content-against-content fault — so neither a marker on the rule nor a filter
/// keyed on the code can express the split without over-refusing.
///
/// # This census is derived, not transcribed
///
/// **A number in prose about code the reader can enumerate is the defect, not the
/// number.** Every count written here has been wrong, and the last one was wrong
/// twice over: the prescribed derivation is structurally blind to a whole plane
/// (below), so even a freshly measured figure understates the denominator.
///
/// So the roster of *sites* is derived at run time instead:
/// `validation_tests::every_write_stamping_site_is_accounted_for` scans the crate's
/// own sources for the three ways a violation acquires [`Stage::Write`] —
/// [`ValidationReport::violate_at_write`], [`ValidationReport::violate_at`], and a
/// hand-constructed `Violation { stage: Stage::Write }` — and holds the result
/// against a list carrying **the reason for each entry**. A new stamping site
/// reddens that test; the list cannot silently fall behind the code, which is the
/// only property this paragraph ever wanted.
///
/// The hand-constructed form is in that scan because leaving it out is how the
/// census went wrong the second time: `taxonomy.rs` builds its `Violation` literal
/// and never calls a `violate*` method, so a derivation over the two methods alone
/// cannot see it.
///
/// # What the derivation cannot see at all: the check-function plane
///
/// **`Stage` governs the pipeline plane and nothing else.** The second rule plane
/// — `overlay_rules`, `bundle_rules`, `window`, `supersession` — does not choose
/// its door with a stamp. It chooses by *which function the surface calls*, and
/// its write-door rules therefore stamp `Stage::Publish` or return a `Result` and
/// construct no [`Violation`] at all. Some of them refuse at an authoring
/// door:
///
/// | Rule | Door |
/// | --- | --- |
/// | `overlay_rules::check_authored_shape` | `api/rest/overlays.rs`, create and line-set `PATCH` |
/// | `overlay_rules::check_tax_basis_declared` | `api/rest/overlays.rs` |
/// | `bundle_rules::check_basis_declared` | `api/rest/bundles.rs` |
/// | `window::check_creation` | `infra/window.rs` |
/// | `window::check_cancellation` | `infra/window.rs` |
/// | `window::check_effective_to_adjustment` | `infra/window.rs` |
/// | `supersession::check_changeover_instant` | `api/rest/repricing_runs.rs`, submit |
///
/// A future author consulting this census to decide whether a new fault is
/// write-judgeable is reading a list that is complete for one plane and empty for
/// the other. `check_authored_shape` now stamps its violations
/// [`Stage::Write`] — they satisfy the doctrine above exactly, every operand being
/// in the authored document — which makes the stamp true there and, more to the
/// point, makes `write_stage_only()` **safe** on that report: it answered `None`
/// before, so routing an overlay authoring report through the same filter the
/// three plan/price doors use would have deleted the whole D-67 / D-42 / §1.7
/// save-time guard into `Ok(())` with nothing reddening.
///
/// **The exception: a rule may take its stage from the door that asks.** What a
/// marker cannot carry is *the* stage of a rule's violations; what a rule can carry
/// is a per-instance parameter, when **every** fault it emits is judgeable exactly
/// where any of them is — because then the answer is a property of the surface and
/// not of the fault. `inst-ph-row-attached` (D-342) and `inst-ph-graph` (
/// review) are that shape: both read the submitted phase set, which **is** the
/// request at the `phases` facet and is not an operand of the price-row write at all.
///
/// Three things keep the exception from becoming the marker this doc warns about:
/// the stage is a field on the instance rather than on the rule type, [`Default`] is
/// [`Stage::Publish`] so `plan_shape_rules` registers `::default()` and the aggregate
/// pipeline stays a publish pipeline, and each rule's own case asserts that the
/// pipeline's instance yields `write_stage_only() == None` over a subject the door's
/// instance refuses. Reach for it only with that third part: without it, "the door
/// decides" is indistinguishable from a rule that refuses everywhere.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stage {
    /// Judged at the authoring write **and** at publish.
    Write,
    /// Judged at publish only. The default: a new rule is publish-stage unless
    /// its author states otherwise, which is the safe direction — a rule wrongly
    /// left here refuses later than it could, and a rule wrongly moved to
    /// [`Stage::Write`] refuses an author's legitimate intermediate state.
    #[default]
    Publish,
}

/// A blocking rule failure. `code` is the machine-readable discriminator the
/// design set names (`TIER_BANDS_OVERLAP`, `SUPERSESSION_UNIT_MISMATCH`, …);
/// `subject` locates it for the author (a price id, a phase id, a scope key).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Machine-readable rule code.
    pub code: String,
    /// What the violation is about — the row, band, phase or key at fault.
    pub subject: String,
    /// Human-readable detail for the authoring surface.
    pub detail: String,
    /// The earliest surface that can judge this fault (D-312).
    pub stage: Stage,
}

/// An advisory finding. Surfaced to the author, never a veto — a warning that
/// could block publish would be a violation, and calling it a warning would
/// hide a fail-closed rule behind a soft word.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// Machine-readable advisory code.
    pub code: String,
    /// What the advisory is about.
    pub subject: String,
    /// Human-readable detail for the authoring surface.
    pub detail: String,
}

/// The aggregate outcome of one pipeline run: every blocking violation plus
/// every advisory finding, in rule-registration order.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Blocking failures. Non-empty means the publish does not happen.
    pub violations: Vec<Violation>,
    /// Advisory findings. Never affect the verdict.
    pub warnings: Vec<Advisory>,
}

impl ValidationReport {
    /// Does this report permit the publish to proceed?
    #[must_use]
    pub fn is_publishable(&self) -> bool {
        self.violations.is_empty()
    }

    /// Merge another report into this one, preserving order.
    pub fn absorb(&mut self, other: Self) {
        self.violations.extend(other.violations);
        self.warnings.extend(other.warnings);
    }

    /// Record a blocking violation.
    pub fn violate(
        &mut self,
        code: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.violations.push(Violation {
            code: code.into(),
            subject: subject.into(),
            detail: detail.into(),
            stage: Stage::Publish,
        });
    }

    /// Record a blocking violation the **authoring write** can already judge.
    ///
    /// Use only where every operand of the fault is in the request — see
    /// [`Stage`], whose section on the amendment is what a new arm has
    /// to argue against, because that condition is necessary and not sufficient.
    /// A frozen scope-key operand is the strongest case and is **not** required;
    /// naming it as one is the reading to avoid. Reaching for this on a fault whose
    /// operands can still change is
    /// how multi-call authoring breaks, and no gate would catch it: the row simply
    /// stops saving.
    pub fn violate_at_write(
        &mut self,
        code: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.violations.push(Violation {
            code: code.into(),
            subject: subject.into(),
            detail: detail.into(),
            stage: Stage::Write,
        });
    }

    /// Record a blocking violation stamped with `stage`.
    ///
    /// For the rules whose stage is a **per-instance parameter** rather than a
    /// property of the fault — `inst-ph-row-attached` (D-342) and `inst-ph-graph`, each
    /// emitting one sentence whichever door asks. Dispatching here rather than at a pair of
    /// `report.violate*` calls per fault is what keeps that true: the code, subject and detail
    /// an author reads cannot come to depend on which door refused them, and `inst-ph-graph`
    /// has three faults that would otherwise carry three copies of the dispatch.
    ///
    /// It is deliberately **not** a shorthand for [`Self::violate_at_write`]: a rule
    /// that knows its fault is write-judgeable says so directly, and passing a
    /// constant [`Stage`] here would hide that behind a parameter.
    pub fn violate_at(
        &mut self,
        stage: Stage,
        code: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        match stage {
            Stage::Write => self.violate_at_write(code, subject, detail),
            Stage::Publish => self.violate(code, subject, detail),
        }
    }

    /// The write-judgeable part of this report, or `None` when there is none.
    ///
    /// Warnings are dropped rather than carried: an advisory never blocks, and a
    /// write refusal that shipped one alongside its violations would suggest it
    /// might have. The publish pre-check keeps the whole report, this subset
    /// included — this is an earlier refusal of a part, never a replacement.
    #[must_use]
    pub fn write_stage_only(&self) -> Option<Self> {
        let violations: Vec<Violation> = self
            .violations
            .iter()
            .filter(|v| v.stage == Stage::Write)
            .cloned()
            .collect();
        if violations.is_empty() {
            return None;
        }
        Some(Self {
            violations,
            warnings: Vec::new(),
        })
    }

    /// Record an advisory finding.
    pub fn warn(
        &mut self,
        code: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.warnings.push(Advisory {
            code: code.into(),
            subject: subject.into(),
            detail: detail.into(),
        });
    }
}

impl fmt::Display for ValidationReport {
    /// Renders the blocking count — the detail belongs in the response
    /// envelope, not in a log line that would repeat it per rule.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.violations.len())
    }
}

/// One registered rule of the aggregate pipeline.
///
/// A rule reads the candidate state it is given and appends its findings; it
/// never short-circuits the run and never mutates the subject. Ownership
/// attribution is [`Self::name`] alone — the instruction id the rule answers to,
/// which is what `ValidationPipeline::rule_names` reports.
pub trait ValidationRule<S>: Send + Sync {
    /// Stable rule name for diagnostics and ownership attribution.
    fn name(&self) -> &'static str;

    /// Evaluate the rule against `subject`, appending to `report`.
    fn evaluate(&self, subject: &S, report: &mut ValidationReport);
}

/// The aggregate pipeline: an ordered set of rules over one subject type.
///
/// Rules run in registration order and every one of them runs — the report is
/// the point. A pipeline with no rules publishes everything, which is why the
/// Foundation registers the money and rounding rules itself rather than leaving
/// the base set to whichever slice happens to load first.
#[domain_model]
pub struct ValidationPipeline<S> {
    rules: Vec<Box<dyn ValidationRule<S>>>,
}

impl<S> ValidationPipeline<S> {
    /// An empty pipeline.
    #[must_use]
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Register a rule. Order is preserved and is the order findings appear in.
    #[must_use]
    pub fn with_rule(mut self, rule: Box<dyn ValidationRule<S>>) -> Self {
        self.rules.push(rule);
        self
    }

    /// The registered rule names, in order.
    #[must_use]
    pub fn rule_names(&self) -> Vec<&'static str> {
        self.rules.iter().map(|rule| rule.name()).collect()
    }

    /// Run every rule and return the aggregate report.
    #[must_use]
    pub fn run(&self, subject: &S) -> ValidationReport {
        let mut report = ValidationReport::default();
        for rule in &self.rules {
            rule.evaluate(subject, &mut report);
        }
        report
    }
}

impl<S> Default for ValidationPipeline<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod validation_tests;

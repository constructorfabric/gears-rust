//! The complete ordered tri-state outcome vector (03 §3.1, §3.4; DESIGN `#contract-03-3-7`
//! ordering and identity rules; D-195 blocking/primary selection; D-230 local refusals first).
use super::evidence::UpstreamDetail;
use super::predicates::PredicateId;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The tri-state result of one predicate (03 §3.1).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// The predicate held.
    Passed,
    /// The predicate was evaluated and did not hold.
    Failed,
    /// An input was unavailable, so the predicate could not be evaluated.
    Unevaluable,
}
/// How strongly an outcome binds each operation (D-195, D-227 item 6).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// Blocks every operation.
    Required,
    /// Blocks submit and amendment, never a preview (TCV is required, D-197).
    TcvOnly,
    /// Never blocks; informs a preview forecast only.
    PreviewForecast,
}
impl Verdict {
    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unevaluable => "unevaluable",
        }
    }
}
impl Applicability {
    /// The stored token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::TcvOnly => "tcv-only",
            Self::PreviewForecast => "preview-forecast",
        }
    }
}

/// The operation an assessment gates.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// A preview; may succeed with `tcvWithheld`.
    Preview,
    /// An order submission.
    Submit,
    /// An amendment of an existing order.
    Amendment,
}

/// Evaluation identity (DESIGN L6527-6538): `(predicate, line, item, selection presence, raw
/// dimension value)`. Selected NULL and literal `"default"` are different keys.
///
/// Its serde form (frozen vectors, D-231) is [`OutcomeKeyWire`]: the selection as the two flat
/// members `has_catalog_selection` and `catalog_scope_key`, byte-identical to the stored rows.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "OutcomeKeyWire", into = "OutcomeKeyWire")]
pub struct OutcomeKey {
    /// The predicate evaluated.
    pub predicate: PredicateId,
    /// The line, absent for order-scoped predicates.
    pub line_id: Option<Uuid>,
    /// The catalog item, absent when the identity is not item-scoped.
    pub item_id: Option<Uuid>,
    /// The catalog selection (selected NULL differs from not selected).
    pub selection: CatalogSelection,
}
/// The stored form of [`OutcomeKey`]: today's five members, nulls written out.
#[derive(Serialize, Deserialize)]
struct OutcomeKeyWire {
    predicate: PredicateId,
    line_id: Option<Uuid>,
    item_id: Option<Uuid>,
    has_catalog_selection: bool,
    catalog_scope_key: Option<String>,
}
impl TryFrom<OutcomeKeyWire> for OutcomeKey {
    type Error = KeyError;
    fn try_from(w: OutcomeKeyWire) -> Result<Self, Self::Error> {
        Ok(Self {
            predicate: w.predicate,
            line_id: w.line_id,
            item_id: w.item_id,
            selection: CatalogSelection::from_parts(w.has_catalog_selection, w.catalog_scope_key)
                .map_err(|_| KeyError::KeyWithoutSelection)?,
        })
    }
}
impl From<OutcomeKey> for OutcomeKeyWire {
    fn from(k: OutcomeKey) -> Self {
        let (has_catalog_selection, catalog_scope_key) = k.selection.into_parts();
        Self {
            predicate: k.predicate,
            line_id: k.line_id,
            item_id: k.item_id,
            has_catalog_selection,
            catalog_scope_key,
        }
    }
}
/// One entry of the failure list (F03 §3.1): a non-passed `(line, reason)`, first occurrence.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// The line, absent for an order-level failure.
    pub line_id: Option<Uuid>,
    /// The registered reason.
    pub reason: Reason,
    /// The first predicate in declared order that reported it.
    pub predicate: PredicateId,
}
/// A broken `OutcomeKey` identity rule (DESIGN L6527-6538).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// An item, selection or scope key is present without a line.
    #[error("a component or key requires a line")]
    ComponentWithoutLine,
    /// A selection is present without an item.
    #[error("a selection requires an item")]
    SelectionWithoutItem,
    /// A scope key is present without a selection (refused when the two stored members are
    /// decoded into a [`CatalogSelection`]).
    #[error("a scope key requires a selection")]
    KeyWithoutSelection,
}
impl OutcomeKey {
    /// The third DESIGN L6527 rule (a key requires a selection) is the type of
    /// [`CatalogSelection`]; decoding refuses it with [`KeyError::KeyWithoutSelection`].
    ///
    /// # Errors
    /// The identity breaks one of the other two DESIGN L6527 rules.
    pub fn validate(&self) -> Result<(), KeyError> {
        let selected = self.selection.has_catalog_selection();
        if self.line_id.is_none() && (self.item_id.is_some() || selected) {
            return Err(KeyError::ComponentWithoutLine);
        }
        if selected && self.item_id.is_none() {
            return Err(KeyError::SelectionWithoutItem);
        }
        Ok(())
    }
}
fn null_first<T: Ord + ?Sized>(a: Option<&T>, b: Option<&T>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y),
    }
}
impl Ord for OutcomeKey {
    fn cmp(&self, o: &Self) -> Ordering {
        // Exhaustive on both sides: a new identity field fails to compile until it is ordered
        // here (D-220 item 6).
        let Self {
            predicate,
            line_id,
            item_id,
            selection,
        } = self;
        let Self {
            predicate: o_predicate,
            line_id: o_line_id,
            item_id: o_item_id,
            selection: o_selection,
        } = o;
        predicate
            .index()
            .cmp(&o_predicate.index())
            .then_with(|| {
                null_first(
                    line_id.as_ref().map(Uuid::as_bytes),
                    o_line_id.as_ref().map(Uuid::as_bytes),
                )
            })
            .then_with(|| {
                null_first(
                    item_id.as_ref().map(Uuid::as_bytes),
                    o_item_id.as_ref().map(Uuid::as_bytes),
                )
            })
            // Not selected < selected NULL < selected value by bytes: the order of the
            // `(has_catalog_selection, catalog_scope_key)` pair frozen vectors were sorted by.
            .then_with(|| selection.cmp(o_selection))
    }
}
impl PartialOrd for OutcomeKey {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// One typed producer subresult (D-195 "producer result fields"); `detail` is allowlisted.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerResult {
    /// The producing gear.
    pub producer: String,
    /// The producer's profile that answered.
    pub profile: String,
    /// The producer's own predicate name.
    pub predicate: String,
    /// The producer's verdict.
    pub verdict: Verdict,
    /// The producer's reason, when not passed.
    pub reason: Option<String>,
    /// Allowlisted evidence from the producer.
    pub detail: Option<UpstreamDetail>,
}

/// One evaluated predicate: its identity, verdict, reason and evidence.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The evaluation identity.
    pub key: OutcomeKey,
    /// The tri-state result.
    pub verdict: Verdict,
    /// Required when the verdict is not `Passed` (schema check `ck_…gate_outcome__2`).
    pub reason: Option<Reason>,
    /// Which operations this outcome blocks.
    pub applicability: Applicability,
    /// The typed producer subresults behind the verdict.
    pub producer_results: Vec<ProducerResult>,
    /// Allowlisted evidence for the outcome as a whole.
    pub upstream_detail: Option<UpstreamDetail>,
}

/// Why a vector could not be built.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VectorError {
    /// Two outcomes share one evaluation identity.
    #[error("duplicate evaluation identity")]
    DuplicateIdentity(OutcomeKey),
    /// A non-passed outcome carries no reason.
    #[error("verdict {1:?} without a reason")]
    ReasonMissing(OutcomeKey, Verdict),
    /// An outcome key breaks an identity rule.
    #[error("invalid identity: {1}")]
    InvalidIdentity(OutcomeKey, KeyError),
    /// A passed outcome carries a reason (`StoredOutcome::reason` is `None` exactly when passed).
    #[error("passed outcome with a reason")]
    ReasonOnPass(OutcomeKey),
}

/// The complete vector, sorted by `OutcomeKey`, identities unique and reasons present. Its
/// serde form is the outcome list; deserialization re-validates it through [`Vector::build`], so
/// a frozen vector (a D-188 attempt's, D-231) never bypasses the identity rules.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Outcome>", into = "Vec<Outcome>")]
pub struct Vector {
    outcomes: Vec<Outcome>,
}
impl TryFrom<Vec<Outcome>> for Vector {
    type Error = VectorError;
    fn try_from(outcomes: Vec<Outcome>) -> Result<Self, Self::Error> {
        Self::build(outcomes)
    }
}
impl From<Vector> for Vec<Outcome> {
    fn from(vector: Vector) -> Self {
        vector.outcomes
    }
}
impl Vector {
    /// Rebuild the vector with the authoritative predicate-9 failures for blocked lines.
    /// # Errors
    /// A rebuilt outcome violates the vector rules.
    pub fn replace_inflight(
        &self,
        lines: &std::collections::BTreeSet<Uuid>,
    ) -> Result<Self, VectorError> {
        let mut outcomes = self.outcomes.clone();
        for o in &mut outcomes {
            if o.key.predicate == PredicateId::InFlightPerKey
                && o.key.line_id.is_some_and(|id| lines.contains(&id))
            {
                o.verdict = Verdict::Failed;
                o.reason = Some(Reason::OrderInFlightForKey);
                o.producer_results.clear();
                o.upstream_detail = None;
            }
        }
        Self::build(outcomes)
    }

    /// # Errors
    /// Duplicate identity, missing reason, a reason on a passed outcome, invalid identity.
    pub fn build(mut outcomes: Vec<Outcome>) -> Result<Self, VectorError> {
        for o in &outcomes {
            o.key
                .validate()
                .map_err(|e| VectorError::InvalidIdentity(o.key.clone(), e))?;
            match (o.verdict, o.reason) {
                (Verdict::Passed, Some(_)) => {
                    return Err(VectorError::ReasonOnPass(o.key.clone()));
                }
                (v, None) if v != Verdict::Passed => {
                    return Err(VectorError::ReasonMissing(o.key.clone(), v));
                }
                _ => {}
            }
        }
        outcomes.sort_by(|a, b| a.key.cmp(&b.key));
        if let Some(w) = outcomes.windows(2).find(|w| w[0].key == w[1].key) {
            return Err(VectorError::DuplicateIdentity(w[0].key.clone()));
        }
        Ok(Self { outcomes })
    }
    /// The outcomes in declared order.
    #[must_use]
    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }
    /// Every non-passed outcome in declared order (F03: "the failure list").
    pub fn failures(&self) -> impl Iterator<Item = &Outcome> {
        self.outcomes
            .iter()
            .filter(|o| o.verdict != Verdict::Passed)
    }
    /// Every non-passed row once per `(line, reason)`, in vector order: the one failure dedup
    /// `assess` stores and [`Self::report`] and the writer's rebuilt vector project.
    #[must_use]
    pub fn failure_list(&self) -> Vec<Failure> {
        let mut seen = HashSet::new();
        self.failures()
            .filter_map(|o| Some((o, o.reason?)))
            .filter(|(o, r)| seen.insert((o.key.line_id, *r)))
            .map(|(o, reason)| Failure {
                line_id: o.key.line_id,
                reason,
                predicate: o.key.predicate,
            })
            .collect()
    }
    /// The stored projection of every outcome, in declared order (S3-10): the
    /// `bss_orders__gate_outcome` columns the diagnostics writer maps 1:1, every enum as its
    /// stored token, producer results as a JSON array (empty for a wholly local result) and the
    /// allowlisted upstream detail as JSON or absent.
    ///
    /// # Errors
    /// Producer results or upstream detail do not serialize to JSON.
    pub fn stored_projection(&self) -> Result<Vec<StoredOutcome>, serde_json::Error> {
        self.outcomes
            .iter()
            .map(|o| {
                Ok(StoredOutcome {
                    predicate: o.key.predicate.token(),
                    line_id: o.key.line_id,
                    item_id: o.key.item_id,
                    has_catalog_selection: o.key.selection.has_catalog_selection(),
                    catalog_scope_key: o.key.selection.catalog_scope_key().map(str::to_owned),
                    verdict: o.verdict.token(),
                    reason: o.reason.map(Reason::token),
                    applicability: o.applicability.token(),
                    producer_results: serde_json::to_value(&o.producer_results)?,
                    upstream_detail: o
                        .upstream_detail
                        .as_ref()
                        .map(serde_json::to_value)
                        .transpose()?,
                })
            })
            .collect()
    }
    /// The primary Problem (DESIGN L5093; D-230 then D-195), within the blocking set in
    /// declared order: the first failed outcome of a definitive local predicate
    /// (`PredicateSpec::definitive_local`), or any outcome carrying predicate 1's reason (the
    /// D-228 item 6 withheld payer-dependent rows rank with it); otherwise the first unevaluable;
    /// otherwise the first failed. `None` when nothing blocks.
    #[must_use]
    pub fn primary(&self, op: Operation) -> Option<&Outcome> {
        let blocking: Vec<&Outcome> = self.outcomes.iter().filter(|o| blocking(op, o)).collect();
        blocking
            .iter()
            .copied()
            .find(|o| definitive_local(o))
            .or_else(|| {
                blocking
                    .iter()
                    .copied()
                    .find(|o| o.verdict == Verdict::Unevaluable)
            })
            .or_else(|| blocking.first().copied())
    }
}
/// One outcome as `bss_orders__gate_outcome` stores it (S3-10): identity verbatim, every enum
/// as its stored token. Never public: producer results and upstream detail are diagnostics.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredOutcome {
    /// The predicate token.
    pub predicate: &'static str,
    /// The line, absent for order-scoped predicates.
    pub line_id: Option<Uuid>,
    /// The catalog item, absent when the identity is not item-scoped.
    pub item_id: Option<Uuid>,
    /// Whether a catalog selection is present.
    pub has_catalog_selection: bool,
    /// The raw dimension value of the selection; `None` for a selected NULL.
    pub catalog_scope_key: Option<String>,
    /// The verdict token.
    pub verdict: &'static str,
    /// The reason token; `None` exactly when the verdict is `passed`.
    pub reason: Option<&'static str>,
    /// The applicability token.
    pub applicability: &'static str,
    /// The ordered typed producer subresults as a JSON array; empty for a local result.
    pub producer_results: serde_json::Value,
    /// The allowlisted upstream detail as JSON, if any.
    pub upstream_detail: Option<serde_json::Value>,
}

/// D-230: a definitive local business refusal: a failed outcome of a `definitive_local`
/// predicate, or an outcome carrying predicate 1's reason (`axis-invalid`,
/// `payer-relationship-unconfirmed`), which the D-228 item 6 withheld rows carry as unevaluable.
/// Unsupported authored terms or inconsistent cycles are also definitive local refusals.
/// The assessment reads it on the primary to present every composite-gate input `Resolved`
/// (D-230 engine presentation).
pub(crate) fn definitive_local(o: &Outcome) -> bool {
    (o.verdict == Verdict::Failed && o.key.predicate.spec().definitive_local)
        || matches!(
            o.reason,
            Some(
                Reason::AxisInvalid
                    | Reason::PayerRelationshipUnconfirmed
                    | Reason::BillingTermsUnsupported
            )
        )
}

/// D-195 / D-227 item 6: `tcv-only` and `preview-forecast` outcomes never block a Preview; on
/// submit and amendment `tcv-only` blocks (TCV is required, D-197).
#[must_use]
pub fn blocking(op: Operation, o: &Outcome) -> bool {
    if o.verdict == Verdict::Passed {
        return false;
    }
    !matches!(
        (op, o.applicability),
        (
            Operation::Preview,
            Applicability::TcvOnly | Applicability::PreviewForecast
        ) | (_, Applicability::PreviewForecast)
    )
}

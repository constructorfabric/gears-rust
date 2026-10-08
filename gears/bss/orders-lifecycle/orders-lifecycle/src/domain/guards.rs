//! Guard registry (DESIGN §3.2 *Guard registry*, §3.3 `interface-guard-registration`;
//! Foundation §4.1, §4.5; D-113).
//!
//! Slices declare named guards against transition-table rows with their failure reasons and
//! declared inputs. The engine owns *when* and *in what order* they run; it never contains
//! their predicate logic (Foundation §4.5: "the engine holds their guard registrations and
//! their reason entries, never their logic"). Declarations are compiled at startup: a guard on a
//! nonexistent row, an unregistered or engine-owned reason, a duplicate guard on a row, or a
//! reason with no owner fails startup.
//!
//! The declaration order below is the registration order and therefore the refusal precedence
//! of each row's guard set. It reproduces the orders the feature contracts state (02 §2.2-2.3,
//! 03 §3.6, 04 §3.1-3.2, 05 §3.3 and *Record Acceptance*, 06 §2.4, §3.2-3.5, 07 §2.1-2.3, §3.1).
//! The owning packages bind predicates to these names; S4-01 and the S5 packages may refine a
//! declaration together with its contract, never around the engine.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bss_orders_lifecycle_sdk::catalog::Reason;
use serde::de::DeserializeOwned;

use super::state_table::{RowId, StateTable};

/// An external guard input resolved before the transaction (Foundation §3.6 step 2), with the
/// registered reasons its unavailability may carry (step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputPort {
    /// Identity operation: tenant-axis validity and the payer's commercial profile (03 step 2).
    Identity,
    /// Seller-scoped Pricing revision per line (03 step 3).
    PricingRevision,
    /// Catalog/Products read predicates (03 step 5).
    CatalogPredicates,
    /// Pin composition from accepted bindings (03 step 10).
    PinComposition,
    /// Subscriptions SUB-G1 overlap key (03 step 4).
    OverlapKey,
    /// Subscriptions occupancy (03 step 5).
    Occupancy,
    /// Rating total/TCV and BillingTerms-dependent evaluation (03 step 5).
    Evaluation,
    /// Contract status and party eligibility (03 step 5).
    ContractResolution,
    /// Live acceptance requirement (05 §3.3 step 1, *Record Acceptance* step 2).
    AcceptanceRequirement,
}
impl InputPort {
    /// Registered reasons an unresolvable value of this input may carry.
    #[must_use]
    pub fn unevaluable_reasons(self) -> &'static [Reason] {
        match self {
            Self::Identity => &[Reason::IdentityPartyUnavailable],
            Self::PricingRevision => &[Reason::PricingRevisionUnavailable],
            Self::CatalogPredicates => &[Reason::CatalogPredicatesUnavailable],
            Self::PinComposition => &[Reason::CatalogPinCompositionUnavailable],
            Self::OverlapKey => &[Reason::OverlapKeyUnavailable],
            Self::Occupancy => &[Reason::OverlapPresenceUnevaluable],
            Self::Evaluation => &[Reason::EvaluationUnavailable],
            Self::ContractResolution => &[Reason::ContractResolutionUnavailable],
            Self::AcceptanceRequirement => &[Reason::AcceptanceRequirementUnevaluable],
        }
    }
}

/// One declared guard: unique name, owning package, rows, refusal reasons (tokens) and inputs.
#[derive(Debug, Clone, Copy)]
pub struct GuardDecl {
    pub name: &'static str,
    /// The implementation package that binds the predicate (engine holds only the declaration).
    pub owner: &'static str,
    pub rows: &'static [u8],
    pub reasons: &'static [&'static str],
    pub inputs: &'static [InputPort],
}

/// Assessment-producing guard: its refusals carry the complete gate assessment (Foundation
/// *Diagnostic settlement contract*).
pub const GATE_COMPOSITE: &str = "gate.sellability-composite";

const SUBMIT: &[u8] = &[4];
const AMEND: &[u8] = &[18, 19, 20];
const SUBMIT_AMEND: &[u8] = &[4, 18, 19, 20];
const STRUCTURAL: &[u8] = &[2, 4, 18, 19, 20];

use InputPort as P;

/// Registration order is precedence order within each row.
pub const GUARDS: &[GuardDecl] = &[
    // --- Capture (02 §2.1-2.3, §3.1; Foundation step-19 shared structural guards) ---
    GuardDecl {
        name: "capture.draft-line-membership",
        owner: "S2-09",
        rows: &[2],
        reasons: &["line-not-found"],
        inputs: &[],
    },
    GuardDecl {
        name: "capture.mixed-field-classes",
        owner: "S2-09",
        rows: &[2],
        reasons: &["mixed-field-classes"],
        inputs: &[],
    },
    GuardDecl {
        name: "capture.seller-fixed",
        owner: "S2-09",
        rows: &[2],
        reasons: &["tenant-axis-immutable"],
        inputs: &[],
    },
    // Submit's at-least-one-line guard is declared first on row 4 (03 step 1).
    GuardDecl {
        name: "gate.has-lines",
        owner: "S3-12",
        rows: SUBMIT,
        reasons: &["no-lines"],
        inputs: &[],
    },
    // --- Versioning amendment order (04 §3.1 step 1), ahead of the shared structural guards ---
    GuardDecl {
        name: "versioning.amendment-cap",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["amendment-cap-exhausted"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.amendment-nonempty",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["amendment-empty"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.amendment-reason",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["amendment-reason-invalid"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.no-administrative-field",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["administrative-field-in-amendment"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.no-frozen-field",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["tenant-axis-immutable"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.payer-within-seller",
        owner: "S4-01",
        rows: AMEND,
        reasons: &["payer-rebinding-requires-seller"],
        inputs: &[P::Identity],
    },
    // Shared structural guards, registered once and reused (Foundation step 19).
    GuardDecl {
        name: "capture.category-admitted",
        owner: "S2-09",
        rows: &[1, 2, 4, 18, 19, 20],
        reasons: &["category-not-admitted"],
        inputs: &[],
    },
    GuardDecl {
        name: "capture.currency-consistent",
        owner: "S2-09",
        rows: STRUCTURAL,
        reasons: &["currency-mixed"],
        inputs: &[],
    },
    GuardDecl {
        name: "capture.line-cap",
        owner: "S2-09",
        rows: STRUCTURAL,
        reasons: &["line-cap-exceeded"],
        inputs: &[],
    },
    // Accepted-binding deadline and aggregate capacity precede the date check (03 step 15,
    // Foundation *PriceBook admission guards*, D-152/D-158/D-191).
    GuardDecl {
        name: "gate.binding-admission",
        owner: "S3-12",
        rows: SUBMIT_AMEND,
        reasons: &["order-binding-expired", "purchase-capacity-exceeded"],
        inputs: &[],
    },
    GuardDecl {
        name: "capture.date-basis",
        owner: "S2-10",
        rows: SUBMIT_AMEND,
        reasons: &["date-cascade-invalid"],
        inputs: &[],
    },
    // The composite gate is registered after every non-gate guard (diagnostic contract).
    GuardDecl {
        name: GATE_COMPOSITE,
        owner: "S3-12",
        rows: SUBMIT_AMEND,
        reasons: &[
            "axis-invalid",
            "contract-not-active",
            "contract-party-ineligible",
            "quantity-below-floor",
            "market-inconsistent",
            "reference-unresolvable",
            "reference-duplicated",
            "currency-mixed",
            "overlap-cardinality-exceeded",
            "overlap-key-unresolvable",
            "pin-unresolvable",
            "pricing-revision-absent",
            "order-binding-policy-missing",
            "catalog-predicate-failed",
            "catalog-predicate-unevaluable",
            "date-cascade-invalid",
        ],
        inputs: &[
            P::Identity,
            P::PricingRevision,
            P::CatalogPredicates,
            P::OverlapKey,
            P::Occupancy,
            P::ContractResolution,
            P::Evaluation,
            P::PinComposition,
        ],
    },
    // --- Administrative edit (04 §3.2 step 2-3) ---
    GuardDecl {
        name: "versioning.admin-line-membership",
        owner: "S4-05",
        rows: &[3],
        reasons: &["line-not-found"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.admin-field-classification",
        owner: "S4-05",
        rows: &[3],
        reasons: &["commercial-field-immutable"],
        inputs: &[],
    },
    GuardDecl {
        name: "versioning.admin-changed",
        owner: "S4-05",
        rows: &[3],
        reasons: &["administrative-edit-unchanged"],
        inputs: &[],
    },
    // --- Expiry and auto-void (07 §3.1 steps 7-8) ---
    GuardDecl {
        name: "expiry.candidate-fresh",
        owner: "S5-13",
        rows: &[6, 24],
        reasons: &["expiry-candidate-stale"],
        inputs: &[],
    },
    GuardDecl {
        name: "expiry.due",
        owner: "S5-13",
        rows: &[6, 24],
        reasons: &["expiry-not-due"],
        inputs: &[],
    },
    GuardDecl {
        name: "expiry.prehold-exempt",
        owner: "S5-13",
        rows: &[24],
        reasons: &["expiry-exempt-prehold"],
        inputs: &[],
    },
    // --- Workflow approval reflections (06 *Reflect Verdict* step 1) ---
    GuardDecl {
        name: "seam.verdict-authority",
        owner: "S5-03",
        rows: &[7, 8, 9, 10],
        reasons: &["verdict-authority-missing"],
        inputs: &[],
    },
    GuardDecl {
        name: "seam.denial-reason",
        owner: "S5-03",
        rows: &[10],
        reasons: &["denial-reason-missing"],
        inputs: &[],
    },
    // --- Begin fulfillment (05 §3.3 composed by 06 §3.2) ---
    GuardDecl {
        name: "preconditions.acceptance-recorded",
        owner: "S4-10",
        rows: &[11],
        reasons: &["acceptance-required-not-recorded"],
        inputs: &[P::AcceptanceRequirement],
    },
    GuardDecl {
        name: "preconditions.payment-authorization",
        owner: "S4-10",
        rows: &[11],
        reasons: &["authorization-pending", "authorization-failed"],
        inputs: &[],
    },
    GuardDecl {
        name: "gate.binding-deadline",
        owner: "S5-04",
        rows: &[11],
        reasons: &["order-binding-expired"],
        inputs: &[],
    },
    // --- Spawn signal (06 §3.2 step 3) ---
    GuardDecl {
        name: "seam.spawn-signal-null",
        owner: "S5-04",
        rows: &[12],
        reasons: &["spawn-signal-already-recorded"],
        inputs: &[],
    },
    // --- Completion acknowledgement (06 §3.3) ---
    GuardDecl {
        name: "seam.acknowledgement-lines",
        owner: "S5-10",
        rows: &[13],
        reasons: &["acknowledgement-lines-incomplete"],
        inputs: &[],
    },
    GuardDecl {
        name: "seam.acknowledgement-subscriptions",
        owner: "S5-10",
        rows: &[13],
        reasons: &[
            "acknowledgement-subscription-missing",
            "acknowledgement-subscription-duplicated",
        ],
        inputs: &[],
    },
    // --- Cancel reason first on every cancel row (07 §2.2 step 2; 06 §2.4 step 3) ---
    GuardDecl {
        name: "cancel.reason-required",
        owner: "S5-11",
        rows: &[5, 15, 16, 17, 23, 27],
        reasons: &["cancel-reason-required"],
        inputs: &[],
    },
    // --- Failed acknowledgement (06 §3.4 step 2) ---
    GuardDecl {
        name: "seam.failure-reason",
        owner: "S5-10",
        rows: &[14, 26],
        reasons: &["failure-reason-missing"],
        inputs: &[],
    },
    // Workflow cancel: pre-hold state precedes evidence (06 §2.4 step 3).
    GuardDecl {
        name: "seam.workflow-cancel-prehold",
        owner: "S5-10",
        rows: &[27],
        reasons: &["prehold-not-in-fulfillment"],
        inputs: &[],
    },
    GuardDecl {
        name: "seam.compensation-evidence",
        owner: "S5-10",
        rows: &[14, 16, 26, 27],
        reasons: &[
            "compensation-evidence-missing",
            "compensation-evidence-incomplete",
        ],
        inputs: &[],
    },
    // Failed acknowledgement: pre-hold state last, if held (06 §3.4 step 2).
    GuardDecl {
        name: "seam.acknowledge-prehold",
        owner: "S5-10",
        rows: &[26],
        reasons: &["prehold-not-in-fulfillment"],
        inputs: &[],
    },
    // Shared direct-cancel window (06 §3.5): ordinary and mediated cancels from fulfillment,
    // including a fulfillment hold (row 23 evaluates the pre-hold state's own cancel guard).
    GuardDecl {
        name: "seam.cancel-window",
        owner: "S5-11",
        rows: &[15, 16, 23, 27],
        reasons: &["direct-cancel-window-closed"],
        inputs: &[],
    },
    // --- Hold/resume (07 §2.1): the resume cap precedes the engine's stored-target check ---
    GuardDecl {
        name: "hold.resume-cap",
        owner: "S5-11",
        rows: &[22],
        reasons: &["resume-cap-exhausted"],
        inputs: &[],
    },
    // --- Record acceptance: already-recorded before recording-party (05 *Record Acceptance*) ---
    GuardDecl {
        name: "preconditions.acceptance-not-recorded",
        owner: "S4-08",
        rows: &[25],
        reasons: &["acceptance-already-recorded"],
        inputs: &[],
    },
    // The live requirement source is resolved for the contribution (05 *Record Acceptance* step
    // 2); declaring it on the last guard keeps both resolved local refusals ahead of an outage
    // (D-113), as that algorithm's guard order requires.
    GuardDecl {
        name: "preconditions.recording-party",
        owner: "S4-08",
        rows: &[25],
        reasons: &["acceptance-recording-party-barred"],
        inputs: &[P::AcceptanceRequirement],
    },
    // --- Two-person forced failure, in the 07 §2.3 refusal order (D-182) ---
    GuardDecl {
        name: "force.reason-required",
        owner: "S5-15",
        rows: &[28, 29],
        reasons: &["forced-failure-reason-required"],
        inputs: &[],
    },
    GuardDecl {
        name: "force.prehold",
        owner: "S5-15",
        rows: &[29],
        reasons: &["prehold-not-in-fulfillment"],
        inputs: &[],
    },
    GuardDecl {
        name: "force.spawn-recorded",
        owner: "S5-15",
        rows: &[28, 29],
        reasons: &["spawn-signal-not-recorded"],
        inputs: &[],
    },
    GuardDecl {
        name: "force.overdue-window",
        owner: "S5-15",
        rows: &[28, 29],
        reasons: &["overdue-window-not-elapsed"],
        inputs: &[],
    },
    GuardDecl {
        name: "force.second-approver",
        owner: "S5-15",
        rows: &[28, 29],
        reasons: &["second-approver-required"],
        inputs: &[],
    },
];

/// Reasons the engine itself returns (DESIGN §3.3 *Error surface*); no slice may register a
/// second name for these conditions, nor return them from a guard.
pub const ENGINE_REASONS: [Reason; 9] = [
    Reason::NotAdmissible,
    Reason::VersionConflict,
    Reason::IdempotencyMismatch,
    Reason::StillProcessing,
    Reason::AuthorizationContextChanged,
    Reason::ExpectedVersionRequired,
    Reason::RequestInvalid,
    // Step 15 stored-target check and the step-17 authoritative collision are engine steps.
    Reason::ResumeTargetMissing,
    Reason::OrderInFlightForKey,
];

/// Reasons owned outside the transition guard registry: authorization (shared PEP), reads,
/// the D-201 internal writer's closed guard order, Workflow-received acknowledgement failure
/// reasons and Preview-only inputs. Each is mapped, so none is an unowned reason.
pub const NON_GUARD_REASONS: [Reason; 14] = [
    Reason::OrderNotFound,
    Reason::OperationNotPermittedForActor,
    Reason::DelegationProofRequired,
    Reason::DelegationProofInvalid,
    Reason::PageSizeExceeded,
    Reason::FilterInvalid,
    Reason::CursorInvalid,
    Reason::ReadStoreUnavailable,
    Reason::VersionNotFound,
    Reason::GrantSourceMismatch,
    Reason::GrantPredecessorUnsettled,
    Reason::GrantGenerationExhausted,
    Reason::MarketDivergence,
    Reason::OverlapCollision,
];

/// Preview-only unavailable inputs (03 §3.7): never part of a transition guard set.
pub const PREVIEW_ONLY_REASONS: [Reason; 1] = [Reason::IndicativeTaxUnavailable];

/// A compiled guard specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardSpec {
    pub name: &'static str,
    pub owner: &'static str,
    pub reasons: Vec<Reason>,
    pub inputs: Vec<InputPort>,
}
impl GuardSpec {
    #[must_use]
    pub fn admits_reason(&self, reason: Reason) -> bool {
        self.reasons.contains(&reason)
    }
    /// Whether this is the assessment-producing composite gate guard.
    #[must_use]
    pub fn is_gate(&self) -> bool {
        self.name == GATE_COMPOSITE
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardRegistryError {
    #[error("guard `{0}` is declared twice")]
    DuplicateName(&'static str),
    #[error("guard `{0}` names nonexistent row {1}")]
    UnknownRow(&'static str, u8),
    #[error("guard `{0}` names row {1} twice")]
    DuplicateRow(&'static str, u8),
    #[error("guard `{0}` has an unregistered reason `{1}`")]
    UnknownReason(&'static str, &'static str),
    #[error("guard `{0}` registers engine-owned reason {1:?}")]
    EngineReason(&'static str, Reason),
    #[error("guard `{0}` declares no refusal reason")]
    NoReason(&'static str),
    #[error("guard `{0}` registers create but create admits only its category guard")]
    CreateGuard(&'static str),
    #[error("reason {0:?} has no owner")]
    UnownedReason(Reason),
    #[error("the composite gate must be the last guard of row {0}")]
    GateNotLast(u8),
}

/// Compiled per-row guard sets in registration order.
#[derive(Debug, Clone)]
pub struct GuardRegistry {
    by_row: BTreeMap<RowId, Vec<Arc<GuardSpec>>>,
}

fn reason(name: &'static str, token: &'static str) -> Result<Reason, GuardRegistryError> {
    parse(token).ok_or(GuardRegistryError::UnknownReason(name, token))
}
fn parse<T: DeserializeOwned>(token: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(token.to_owned())).ok()
}

impl GuardRegistry {
    /// Compile the shipped declarations against the compiled state table.
    ///
    /// # Errors
    /// Never for the shipped declarations (a unit test proves it); see [`Self::compile`].
    pub fn registered(table: &StateTable) -> Result<Self, GuardRegistryError> {
        Self::compile(table, GUARDS)
    }

    /// Compile and validate a declaration set.
    ///
    /// # Errors
    /// Any [`GuardRegistryError`]; the gear refuses to start.
    pub fn compile(table: &StateTable, decls: &[GuardDecl]) -> Result<Self, GuardRegistryError> {
        let mut names = BTreeSet::new();
        let mut by_row: BTreeMap<RowId, Vec<Arc<GuardSpec>>> = BTreeMap::new();
        let mut owned: BTreeSet<usize> = BTreeSet::new();
        for decl in decls {
            if !names.insert(decl.name) {
                return Err(GuardRegistryError::DuplicateName(decl.name));
            }
            if decl.reasons.is_empty() {
                return Err(GuardRegistryError::NoReason(decl.name));
            }
            let reasons = decl
                .reasons
                .iter()
                .map(|t| reason(decl.name, t))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(r) = reasons.iter().find(|r| ENGINE_REASONS.contains(r)) {
                return Err(GuardRegistryError::EngineReason(decl.name, *r));
            }
            owned.extend(reasons.iter().map(|r| reason_index(*r)));
            let spec = Arc::new(GuardSpec {
                name: decl.name,
                owner: decl.owner,
                reasons,
                inputs: decl.inputs.to_vec(),
            });
            let mut rows = BTreeSet::new();
            for number in decl.rows {
                if !rows.insert(*number) {
                    return Err(GuardRegistryError::DuplicateRow(decl.name, *number));
                }
                let id = RowId::of(*number);
                if table.row(id).is_none() {
                    return Err(GuardRegistryError::UnknownRow(decl.name, *number));
                }
                if id == table.create_row().id && decl.name != "capture.category-admitted" {
                    return Err(GuardRegistryError::CreateGuard(decl.name));
                }
                by_row.entry(id).or_default().push(Arc::clone(&spec));
            }
        }
        for (row, guards) in &by_row {
            if let Some(position) = guards.iter().position(|g| g.is_gate())
                && position + 1 != guards.len()
            {
                return Err(GuardRegistryError::GateNotLast(row.number()));
            }
        }
        // Every registered reason has exactly one kind of owner: engine, guard, input port,
        // or one of the explicitly non-guard owners.
        owned.extend(ENGINE_REASONS.iter().map(|r| reason_index(*r)));
        owned.extend(NON_GUARD_REASONS.iter().map(|r| reason_index(*r)));
        owned.extend(PREVIEW_ONLY_REASONS.iter().map(|r| reason_index(*r)));
        for port in ALL_PORTS {
            owned.extend(port.unevaluable_reasons().iter().map(|r| reason_index(*r)));
        }
        if let Some(unowned) = Reason::ALL
            .iter()
            .find(|r| !owned.contains(&reason_index(**r)))
        {
            return Err(GuardRegistryError::UnownedReason(*unowned));
        }
        Ok(Self { by_row })
    }

    /// The row's guard set in registration order (empty for an unguarded row).
    #[must_use]
    pub fn row_guards(&self, row: RowId) -> &[Arc<GuardSpec>] {
        self.by_row.get(&row).map_or(&[], Vec::as_slice)
    }

    /// Every declared input port of the row, in guard order.
    #[must_use]
    pub fn row_inputs(&self, row: RowId) -> BTreeSet<InputPort> {
        self.row_guards(row)
            .iter()
            .flat_map(|g| g.inputs.iter().copied())
            .collect()
    }
}

/// Every input port.
pub const ALL_PORTS: [InputPort; 9] = [
    InputPort::Identity,
    InputPort::PricingRevision,
    InputPort::CatalogPredicates,
    InputPort::PinComposition,
    InputPort::OverlapKey,
    InputPort::Occupancy,
    InputPort::Evaluation,
    InputPort::ContractResolution,
    InputPort::AcceptanceRequirement,
];

fn reason_index(reason: Reason) -> usize {
    Reason::ALL
        .iter()
        .position(|r| *r == reason)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "guards_tests.rs"]
mod tests;

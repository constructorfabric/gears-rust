//! In-flight overlap claim rules (Foundation §3.6 step 17, §3.2; DESIGN §3.7
//! `orders_inflight_overlap_claim`; D-83, D-86, D-179, D-182).
//!
//! Pure partitioning and directive mapping. The transactional repository
//! (`infra::storage::repo::claims`) owns acquisition, exact-ID release and the authoritative
//! conflict; the S2-04 engine calls it at step 17, before any version or document write.
//!
//! Orders claims enforce only the fixed one-in-flight-order rule of PRD §6.1(g). They never
//! count or enforce Subscriptions' configurable concurrent-subscription cardinality (§6.1(f),
//! predicate 7) and never hold, release or reconcile receiver capacity (D-182, D-198).
use std::collections::BTreeSet;

use bss_orders_lifecycle_sdk::catalog::{OrderState, Reason, Trigger};
use uuid::Uuid;

/// Terminal set of Foundation §4.3. Every transition into it releases all local claims (17.1).
pub const TERMINAL_STATES: [OrderState; 5] = [
    OrderState::Completed,
    OrderState::Rejected,
    OrderState::Cancelled,
    OrderState::FulfillmentFailed,
    OrderState::Expired,
];

#[must_use]
pub fn is_terminal(state: OrderState) -> bool {
    TERMINAL_STATES.contains(&state)
}

/// The only rows whose contribution carries resolved overlap keys (step 17.2).
pub const ACQUIRING_TRIGGERS: [Trigger; 2] = [Trigger::Submit, Trigger::Amendment];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRuleError {
    #[error("overlap scope key must be non-empty owner text without NUL")]
    InvalidKey,
    #[error("an acquiring transition must offer at least one resolved overlap key")]
    EmptyProposal,
    #[error("a terminal transition carries no resolved overlap keys")]
    TerminalWithKeys,
    #[error("only submit and amendment contributions carry resolved overlap keys")]
    KeysOnNonAcquiringRow,
    #[error("submit and amendment must reach step 17 with resolved overlap keys")]
    AcquiringRowWithoutKeys,
    #[error("held claims repeat a live tuple")]
    DuplicateHeldTuple,
}

/// Subscriptions' registry-owned `catalogSubscriptionProductKey` (SUB-G1, D-153, D-163).
///
/// Stored exactly as received: no trimming, case folding or derivation. Only values
/// PostgreSQL `text` cannot hold, or an empty key that would never identify a product, are
/// refused before they reach storage.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlapScopeKey(String);
impl OverlapScopeKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for OverlapScopeKey {
    type Error = ClaimRuleError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.contains('\0') {
            return Err(ClaimRuleError::InvalidKey);
        }
        Ok(Self(value))
    }
}

/// Full claim identity. Field order makes the derived `Ord` the normative total order: payer
/// UUID bytes, then resource-tenant UUID bytes, then overlap-key bytes (`Uuid` compares its 16
/// bytes; `String` compares UTF-8 bytes, independent of any database collation).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimTuple {
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Uuid,
    pub overlap_scope_key: OverlapScopeKey,
}

/// Distinct proposed tuples of one submit/amendment: the proposed version's payer and the
/// order's own resource tenant (D-179). Duplicate line keys collapse to one offered claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedClaims {
    payer_tenant_id: Uuid,
    resource_tenant_id: Uuid,
    tuples: BTreeSet<ClaimTuple>,
}
impl ProposedClaims {
    /// # Errors
    /// [`ClaimRuleError::EmptyProposal`] when no key was offered.
    pub fn new(
        payer_tenant_id: Uuid,
        resource_tenant_id: Uuid,
        keys: impl IntoIterator<Item = OverlapScopeKey>,
    ) -> Result<Self, ClaimRuleError> {
        let tuples: BTreeSet<_> = keys
            .into_iter()
            .map(|overlap_scope_key| ClaimTuple {
                payer_tenant_id,
                resource_tenant_id,
                overlap_scope_key,
            })
            .collect();
        if tuples.is_empty() {
            return Err(ClaimRuleError::EmptyProposal);
        }
        Ok(Self {
            payer_tenant_id,
            resource_tenant_id,
            tuples,
        })
    }
    #[must_use]
    pub fn payer_tenant_id(&self) -> Uuid {
        self.payer_tenant_id
    }
    #[must_use]
    pub fn resource_tenant_id(&self) -> Uuid {
        self.resource_tenant_id
    }
    /// Distinct tuples in the normative total order.
    pub fn tuples(&self) -> impl ExactSizeIterator<Item = &ClaimTuple> {
        self.tuples.iter()
    }
}

/// What step 17 does for one transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimDirective {
    /// 17.1: the effective target is terminal; release every live claim of this order.
    ReleaseAll,
    /// 17.2: no resolved keys; the claim set stays untouched.
    Retain,
    /// 17.3–17.6: retain held, acquire missing, release superseded after full acquisition.
    Replace(ProposedClaims),
}
impl ClaimDirective {
    /// Engine contribution adapter: map the row's trigger, the resolved effective target
    /// (step 14) and the contribution's resolved keys to the step-17 directive.
    ///
    /// The terminal check comes first and is target-based, so every terminal row — including
    /// D-182 `force-fail-unreconciled` — releases Orders claims, while non-terminal rows
    /// (hold, resume, approval reflections, fulfillment) retain them.
    ///
    /// # Errors
    /// Contract violations between the engine row and its contribution; the engine maps them
    /// to an infrastructure abort, never to a business refusal.
    pub fn for_transition(
        trigger: Trigger,
        effective_target: OrderState,
        proposal: Option<ProposedClaims>,
    ) -> Result<Self, ClaimRuleError> {
        if is_terminal(effective_target) {
            return match proposal {
                None => Ok(Self::ReleaseAll),
                Some(_) => Err(ClaimRuleError::TerminalWithKeys),
            };
        }
        let acquiring = ACQUIRING_TRIGGERS.contains(&trigger);
        match (acquiring, proposal) {
            (true, Some(proposal)) => Ok(Self::Replace(proposal)),
            (true, None) => Err(ClaimRuleError::AcquiringRowWithoutKeys),
            (false, Some(_)) => Err(ClaimRuleError::KeysOnNonAcquiringRow),
            (false, None) => Ok(Self::Retain),
        }
    }
}

/// One live claim this order holds on entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldClaim {
    pub claim_id: Uuid,
    pub tuple: ClaimTuple,
}

/// Step 17.3 partition of held versus proposed full tuples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimPlan {
    /// Held tuples that are also proposed; never re-offered.
    pub retained: Vec<Uuid>,
    /// Proposed tuples not held, in the normative total order.
    pub missing: Vec<ClaimTuple>,
    /// Held tuples absent from the proposal; released only after complete acquisition.
    pub superseded: Vec<Uuid>,
}

/// Partition on full tuples, never on overlap keys alone: a payer change with an unchanged
/// key is one missing and one superseded tuple.
///
/// # Errors
/// [`ClaimRuleError::DuplicateHeldTuple`] if the live set repeats a tuple, which the partial
/// unique index forbids; the caller aborts rather than guessing which row to keep.
pub fn plan(held: &[HeldClaim], proposed: &ProposedClaims) -> Result<ClaimPlan, ClaimRuleError> {
    let mut held_tuples = BTreeSet::new();
    let mut retained = Vec::new();
    let mut superseded = Vec::new();
    for claim in held {
        if !held_tuples.insert(&claim.tuple) {
            return Err(ClaimRuleError::DuplicateHeldTuple);
        }
        if proposed.tuples.contains(&claim.tuple) {
            retained.push(claim.claim_id);
        } else {
            superseded.push(claim.claim_id);
        }
    }
    let missing = proposed
        .tuples
        .iter()
        .filter(|tuple| !held_tuples.contains(tuple))
        .cloned()
        .collect();
    Ok(ClaimPlan {
        retained,
        missing,
        superseded,
    })
}

/// A missing tuple another order holds live. `visible_holder` names that order only when the
/// caller's read scope can see it (D-179: never name an order the caller cannot read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedTuple {
    pub tuple: ClaimTuple,
    pub visible_holder: Option<Uuid>,
}

/// Authoritative step-17.5 collision. Provisional claims are already released by exact ID and
/// every claim held on entry is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapConflict {
    /// Blocked tuples in the normative total order (never empty).
    pub blocked: Vec<BlockedTuple>,
    /// Exact IDs this attempt inserted and released before the refusal commits.
    pub provisional_released: Vec<Uuid>,
}
impl OverlapConflict {
    /// A failed slice guard in the seven-class taxonomy; no eighth class (DESIGN §3.7).
    pub const REASON: Reason = Reason::OrderInFlightForKey;
    /// Stage 3 hand-off: the reached assessment's advisory predicate 9 is replaced by this
    /// authoritative result before diagnostic persistence and settlement (03 §3 step 8;
    /// Foundation step 17.5). Each blocked tuple becomes a failed predicate-9 outcome with
    /// `order-in-flight-for-key`; the conflicting order is carried only when visible.
    #[must_use]
    pub fn predicate_nine(&self) -> PredicateNineReplacement<'_> {
        PredicateNineReplacement {
            reason: Self::REASON,
            blocked: &self.blocked,
        }
    }
}

/// Authoritative predicate-9 verdict the Stage 3 diagnostic writer persists in place of the
/// advisory pre-check. Gate-outcome row construction (run metadata, mapping version) remains
/// the Stage 3 assessment owner's; S2-07 supplies only the authoritative replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateNineReplacement<'a> {
    pub reason: Reason,
    pub blocked: &'a [BlockedTuple],
}

/// Committed result of a newly acquired claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquiredClaim {
    pub claim_id: Uuid,
    pub tuple: ClaimTuple,
}

/// Step-17 outcome. Every variant except `Conflict` continues to step 18.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// 17.1: these live claims were released at database time.
    Released(Vec<Uuid>),
    /// 17.2: untouched.
    Retained,
    /// 17.4/17.6: complete acquisition, then superseded release.
    Admitted {
        retained: Vec<Uuid>,
        acquired: Vec<AcquiredClaim>,
        superseded_released: Vec<Uuid>,
    },
    /// 17.5: settle `order-in-flight-for-key` as a committed business refusal with no version,
    /// line, total, pin, acceptance or event contribution.
    Conflict(OverlapConflict),
}

#[cfg(test)]
#[path = "overlap_tests.rs"]
mod tests;

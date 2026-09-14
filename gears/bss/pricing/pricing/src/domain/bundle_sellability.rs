//! `inst-bc-sellability` — a bundle's sellability is the **conjunction over its
//! components** (`design/08-bundles.md` §3, D-94, D-46).
//!
//! A bundle is sellable at `t` iff **every** referenced component key passes the
//! Slice-7 gate's predicates (1)–(5) at `t`, plus the bundle's own
//! `availableFrom`/`availableTo`. One unsellable component makes the bundle
//! unsellable, never partially sellable — which is D-94 applied one level up.
//!
//! # Predicate (6) is excluded, and excluding it is the point
//!
//! The registry `sellable` flag (D-46) applies to the **bundle SKU itself**, not
//! to component references: `sellable = false` components are exactly the
//! composition-only SKUs bundles exist to package, so folding (6) into the
//! component conjunction would make every such bundle permanently unsellable —
//! the flag would refuse the one use it was introduced to enable.
//!
//! The exclusion lives in [`component_verdict`] rather than in the callers,
//! because a caller that simply "did not pass predicate 6" and a caller that
//! forgot to look it up are indistinguishable from here. Passing the whole
//! outcome list and filtering it in one place makes the exemption a fact of this
//! module that a probe can reach.
//!
//! # `sum_of_parts` and `own_price` differ in exactly one input
//!
//! For `sum_of_parts` there are **no own rows** (`inst-bb-rowless`), so the
//! components are the only key inputs. For `own_price` the bundle's own rows must
//! pass **and** the component keys too — the matching-currency component set is
//! part of the offer (`inst-bb-own`, Slice 4 case iii). One parameter, and the
//! basis decides whether it is present.
//!
//! # A bundle referencing nothing is `NotSellable`, not vacuously sellable
//!
//! An empty conjunction is `true` in logic and wrong here, for
//! [`PlanMarketVerdict::NotSellable`]'s own stated reason: *"or the plan binds no
//! key on this market at all"*. A `sum_of_parts` bundle with no components sums
//! nothing and has nothing to sell; publish refuses it anyway
//! ([`crate::domain::bundle_rules`]), and the gate must not disagree with the
//! validator about a state the store can still hold.
//!
//! # The frozen component set is narrowed the same way coverage is
//!
//! The component keys the gate ranges over span `priceEligibility =
//! all_subscriptions` (`cohort = none`) keys **only** — grandfathered generations
//! are never gate inputs. That narrowing is the caller's, exactly as
//! [`crate::domain::bundle_rules`]'s coverage set is, and for the same reason.
//!
//! # This module has no caller, and the operand it waits for is named here
//!
//! Stated at the top of the thing rather than only in the register, because the
//! reader who arrives here is the one who would otherwise spend the afternoon
//! looking for the call. `nothing_in_this_crate_reaches_the_bundle_conjunction`
//! (in `bundle_sellability_tests.rs`) is what makes it a checked fact: it walks
//! this crate's code with the comments and literals blanked and asserts that no
//! site outside this module names [`bundle_verdict`], [`component_verdict`] or
//! [`ComponentSellability`]. It reddens the day a caller lands, which is when the
//! sentence above stops being true and has to be deleted.
//!
//! **What the caller — [`crate::domain::sellability`] — is missing is one member
//! of the pin.** That surface answers from a frozen
//! [`PlanSubjectDelta`](crate::domain::projection::PlanSubjectDelta), and the
//! payload carries no bundle member: not the composition, not the
//! [`PriceBasis`], not the fact that the plan is a bundle. Three operands would
//! have to become projected facts before the conjunction below can be evaluated at
//! a pin:
//!
//! 1. **the discriminator** — is this plan-subject a bundle. Today only
//!    `pricing_bundle` answers it, which is a truth row and not a pinned one;
//! 2. **the basis** — which of the two arms of [`bundle_verdict`] applies. It
//!    lives on `pricing_bundle.price_basis`, which D-206 records as **not
//!    revision-scoped**: it is mutated in place, so even reading it at the
//!    delta's revision would make a pinned answer swing on an un-versioned column;
//! 3. **the frozen component key set** — `inst-sg-bundle`'s own words. The
//!    component *rows* are revision-scoped and trigger-frozen
//!    (`pricing_bundle_component`, D-92/D-105), so the set is stable at a
//!    revision; what is absent is any path by which it reaches a
//!    `CatalogVersion`. `infra::bundle::publish_composition` records no
//!    `PendingVersionRow`, so a composition change advances no version and
//!    re-projects no subject — `inst-ba-return`'s *"composition frozen into the
//!    read model"* is unbuilt, and it is what would have to land first.
//!
//! The module is kept for the reason D-321 clause (3) kept `rev_share_change_set`
//! (which has since been given the caller that clause anticipated):
//! the rule is what Slice 7 owes Slice 8, it is the design set's `inst-bc-sellability`
//! written out, and deleting it would leave that instruction with nothing to
//! attach to and the walk to be re-derived by whoever lands the projector member.

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::bundle::PriceBasis;
use crate::domain::sellability::{PlanMarketVerdict, Predicate, PredicateAnswer, PredicateOutcome};

/// One component's contribution to the conjunction.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentSellability {
    /// The component's plan.
    pub component_plan_id: Uuid,
    /// Its verdict over predicates (1)–(5) — see [`component_verdict`].
    pub verdict: PlanMarketVerdict,
}

/// A component's verdict over the predicates a **component reference** answers
/// to: (1)–(5), never (6).
///
/// Takes the whole outcome list and filters it here; see the module doc for why
/// that is not the caller's job.
#[must_use]
pub fn component_verdict(outcomes: &[PredicateOutcome]) -> PlanMarketVerdict {
    fold(
        outcomes
            .iter()
            .filter(|outcome| outcome.predicate != Predicate::RegistrySellable)
            .map(|outcome| &outcome.answer),
    )
}

/// Fold a set of predicate answers into one verdict.
///
/// `Failed` dominates `NotEvaluable` dominates `Satisfied`. The ordering is
/// forced rather than chosen: `NotEvaluable` means *"the gate is not yet a gate"*
/// and a consumer must not read it as sellable, while a `Failed` beside it is a
/// fact that will not improve by waiting — reporting `NotEvaluable` there would
/// tell an operator to wait for a slice that will not change the answer.
fn fold<'a>(answers: impl Iterator<Item = &'a PredicateAnswer>) -> PlanMarketVerdict {
    let mut unevaluable = false;
    for answer in answers {
        match answer {
            PredicateAnswer::Failed { .. } => return PlanMarketVerdict::NotSellable,
            PredicateAnswer::NotEvaluable { .. } => unevaluable = true,
            PredicateAnswer::Satisfied => {}
        }
    }
    if unevaluable {
        PlanMarketVerdict::NotEvaluable
    } else {
        PlanMarketVerdict::Sellable
    }
}

/// The bundle's verdict on one market at one instant.
///
/// `own` is the bundle's **own** key verdict and is `Some` for `own_price` and
/// `None` for `sum_of_parts`. The pairing is not expressible in the arguments, so
/// both mismatches are a caller error the type cannot refuse and **both are
/// answered here, in the fail-closed direction each needs**:
///
/// - `sum_of_parts` **carrying** a verdict: ignored rather than folded in, which
///   is what the design set states — a `sum_of_parts` bundle has no own rows, so
///   there is nothing for the verdict to be about. Harmless, because dropping a
///   fact about rows that do not exist cannot make an unsellable bundle sell.
/// - `own_price` **carrying none**: [`PlanMarketVerdict::NotEvaluable`], because
///   the opposite reading is the fail-**open** one. Dropping the bundle's own
///   rows out of the conjunction would let a bundle whose own rows fail
///   predicates (1)–(5) answer `Sellable`, on the strength of its components
///   alone. `NotEvaluable` is the honest answer: the gate has an input it was not
///   given, which is exactly what that verdict means. It enters the conjunction
///   as that verdict rather than short-circuiting it, so a `Failed` availability
///   answer or an unsellable component still dominates, per `fold`'s ordering.
///
/// `bundle_available` is the bundle's own `availableFrom`/`availableTo` answer,
/// which §3 adds to the conjunction explicitly and which no component can carry.
#[must_use]
pub fn bundle_verdict(
    basis: PriceBasis,
    own: Option<PlanMarketVerdict>,
    bundle_available: &PredicateAnswer,
    components: &[ComponentSellability],
) -> PlanMarketVerdict {
    // An empty conjunction is not vacuously true here; see the module doc.
    if components.is_empty() {
        return PlanMarketVerdict::NotSellable;
    }

    let mut verdicts: Vec<PlanMarketVerdict> = components.iter().map(|c| c.verdict).collect();
    let mut unevaluable = matches!(bundle_available, PredicateAnswer::NotEvaluable { .. });
    match (basis, own) {
        (PriceBasis::OwnPrice, Some(own)) => verdicts.push(own),
        // The fail-closed half of the caller error; see the doc. It joins the
        // conjunction as the `NotEvaluable` it is rather than answering it
        // outright, because an early return here would outrank the definite
        // failures below — the ordering `fold` documents as forced.
        (PriceBasis::OwnPrice, None) => unevaluable = true,
        (PriceBasis::SumOfParts, _) => {}
    }

    if matches!(bundle_available, PredicateAnswer::Failed { .. }) {
        return PlanMarketVerdict::NotSellable;
    }

    for verdict in verdicts {
        match verdict {
            PlanMarketVerdict::NotSellable => return PlanMarketVerdict::NotSellable,
            PlanMarketVerdict::NotEvaluable => unevaluable = true,
            PlanMarketVerdict::Sellable => {}
        }
    }
    if unevaluable {
        PlanMarketVerdict::NotEvaluable
    } else {
        PlanMarketVerdict::Sellable
    }
}

#[cfg(test)]
#[path = "bundle_sellability_tests.rs"]
mod bundle_sellability_tests;

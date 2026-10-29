//! What one assessment contributes to the gate's run-level signals (DESIGN 03 §3.8; D-233
//! item 3). Pure: the infra sink turns this into counters. Dedup is per run per
//! `(predicate, origin, reason)` so a wide basket is one signal, not one per line.
use super::assess::Assessment;
use super::predicates::{Origin, PredicateId};
use super::pricing_map::PinOutcome;
use super::vector::Verdict;
use bss_orders_sdk::catalog::Reason;
use toolkit_macros::domain_model;

/// One refused or unevaluable predicate, with its origin and reason.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredicateSignal {
    /// The predicate.
    pub predicate: PredicateId,
    /// Where the predicate comes from (adopted, delta, pin).
    pub origin: Origin,
    /// The reason on the outcome row.
    pub reason: Reason,
}

/// The run-level signals of one assessment.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunSignals {
    /// The assessment carries a primary outcome.
    pub blocked: bool,
    /// Failed rows, once per `(predicate, origin, reason)`, in vector order.
    pub refused: Vec<PredicateSignal>,
    /// Unevaluable rows, same dedup.
    pub unevaluable: Vec<PredicateSignal>,
    /// Lines whose pin is unresolvable.
    pub pins_unresolvable: u32,
    /// Lines whose pin is unevaluable.
    pub pins_unevaluable: u32,
    /// Any unevaluable row with `OverlapPresenceUnevaluable` (an incomplete occupancy answer, D-126).
    pub occupancy_incomplete: bool,
}

impl RunSignals {
    /// Extract the signals from one assessment.
    #[must_use]
    pub fn of(a: &Assessment) -> Self {
        let mut s = Self {
            blocked: a.primary.is_some(),
            ..Self::default()
        };
        for o in a.vector.outcomes() {
            let Some(reason) = o.reason else { continue };
            let sig = PredicateSignal {
                predicate: o.key.predicate,
                origin: o.key.predicate.origin(),
                reason,
            };
            let bucket = match o.verdict {
                Verdict::Passed => continue,
                Verdict::Failed => &mut s.refused,
                Verdict::Unevaluable => &mut s.unevaluable,
            };
            if !bucket.contains(&sig) {
                bucket.push(sig);
            }
            if o.verdict == Verdict::Unevaluable && reason == Reason::OverlapPresenceUnevaluable {
                s.occupancy_incomplete = true;
            }
        }
        for pin in a.pins.values() {
            match pin {
                PinOutcome::Composed { .. } => {}
                PinOutcome::Unresolvable(_) => s.pins_unresolvable += 1,
                PinOutcome::Unevaluable(_) => s.pins_unevaluable += 1,
            }
        }
        s
    }
}

//! Rule (c): overlap uniqueness and the in-flight row per line over the overlap-key and
//! occupancy answers (SUB-G1, #5131 `getOverlapOccupancy`, D-180).
use super::super::pricing_map::{LineCatalog, PinOutcome};
use super::{AssessmentInput, Eval, PASS, failed, unevaluable, unevaluable_with};
use crate::domain::gate::answers::{
    MaxConcurrentActive, Occupancy, OccupancyCountScope, OccupancyObservation, OccupancyQuery,
};
use crate::domain::gate::evidence::UpstreamDetail;
use crate::domain::gate::protected::{ProtectedBatch, ProtectedInvalid, ValidatedProtected};
use bss_orders_sdk::catalog::Reason;
use bss_pricing_sdk::read::ChargeKind;
use std::collections::{BTreeMap, BTreeSet};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Rule (c) state: each line's overlap key, the basket's count per key and occupancy per key.
#[domain_model]
pub(super) struct Overlap<'a> {
    /// Lines whose composed selected bindings contain no paid recurring SKU.
    keyless: BTreeSet<Uuid>,
    /// The line's key; `None` when the answer has none or several for the line.
    keys: BTreeMap<Uuid, BTreeSet<String>>,
    /// Basket lines per key.
    proposed: BTreeMap<String, u64>,
    /// Occupancy per key; `None` when the answer is ambiguous for the key.
    occupancy: BTreeMap<&'a str, Option<&'a Occupancy>>,
    /// Shared whole-batch protected validation, before capacity arithmetic.
    protected: Result<ProtectedBatch, ProtectedInvalid>,
    /// The orchestrator read occupancy and the in-flight lookups: the key answer passed
    /// [`read_keys`](crate::domain::gate::item_keys::read_keys), the one validation both share.
    read: bool,
}

/// The `upstream_code` of a keyed line's rows 7 and 9 when the key answer was rejected before
/// reads, so neither occupancy nor the in-flight lookup ran.
const KEY_ANSWER_REJECTED: &str = "overlap-key-answer-rejected";
/// The overlap-presence row carrying the protected refusal's Orders-authored token.
fn presence_unevaluable(cause: ProtectedInvalid) -> Eval {
    unevaluable_with(Reason::OverlapPresenceUnevaluable, cause.token())
}
impl<'a> Overlap<'a> {
    /// Rule (c) state over the lines `assess` mapped once (`mapped`, keyed by line id).
    pub(super) fn new(input: &'a AssessmentInput, mapped: &BTreeMap<Uuid, LineCatalog>) -> Self {
        let bindings: BTreeMap<Uuid, Vec<_>> = mapped
            .iter()
            .filter_map(|(&id, m)| match &m.pin {
                PinOutcome::Composed { bindings, .. } => Some((id, bindings.clone())),
                _ => None,
            })
            .collect();
        let expected =
            crate::domain::gate::orchestrate::overlap_key_requests(&input.lines, &bindings);
        let relevant: Vec<_> = input
            .overlap_keys
            .answered()
            .into_iter()
            .flatten()
            .filter(|key| expected.iter().any(|e| e.line_id == key.line_id))
            .cloned()
            .collect();
        let read = input
            .overlap_keys
            .answered()
            .is_some_and(|all| crate::domain::gate::item_keys::read_keys(&expected, all).is_some());
        let valid = crate::domain::gate::item_keys::validated_line_keys(&expected, &relevant);
        let keys: BTreeMap<Uuid, BTreeSet<String>> = valid
            .into_iter()
            .map(|(id, keys)| (id, keys.into_iter().map(|k| k.overlap_scope_key).collect()))
            .collect();
        let keyless: BTreeSet<Uuid> = bindings
            .iter()
            .filter(|(_, bound)| !bound.iter().any(|b| b.kind == ChargeKind::Recurring))
            .map(|(&id, _)| id)
            .collect();
        let mut proposed = BTreeMap::new();
        for l in input.lines.iter().filter(|l| !keyless.contains(&l.line_id)) {
            if let Some(scopes) = keys.get(&l.line_id) {
                for k in scopes {
                    *proposed.entry(k.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut occupancy: BTreeMap<&'a str, Option<&'a Occupancy>> = BTreeMap::new();
        for (q, o) in input.occupancy.answered().into_iter().flatten() {
            let OccupancyObservation::LegacyActiveV1(o) = o else {
                continue;
            };
            let slot = occupancy
                .entry(q.overlap_scope_key.as_str())
                .or_insert(Some(o));
            if (*slot).is_some_and(|prev| prev != o) {
                *slot = None;
            }
        }
        let mut expected = BTreeMap::new();
        if let Some(axes) = input.occupancy_axes {
            for (line, scopes) in &keys {
                for scope in scopes {
                    let entry = expected.entry(scope.clone()).or_insert_with(|| {
                        (
                            OccupancyQuery {
                                own_context: None,
                                payer_tenant_id: axes.payer,
                                resource_tenant_id: Some(axes.resource),
                                overlap_scope_key: scope.clone(),
                            },
                            BTreeSet::new(),
                        )
                    });
                    entry.1.insert(*line);
                }
            }
        }
        let protected = crate::domain::gate::protected::validate_batch(
            &expected,
            input.occupancy.answered().map_or(&[], Vec::as_slice),
        );
        Self {
            keyless,
            keys,
            proposed,
            occupancy,
            protected,
            read,
        }
    }

    pub(super) fn is_keyless(&self, line: Uuid) -> bool {
        self.keyless.contains(&line)
    }

    /// `(OverlapUniqueness, InFlightPerKey)` for one line with both ports answered. The
    /// in-flight row is an advisory pre-check; the engine claim remains authoritative
    /// (DESIGN §3.7).
    pub(super) fn rows(&self, line: Uuid, input: &AssessmentInput) -> (Eval, Eval) {
        if self.is_keyless(line) {
            let na = (
                PASS.0,
                None,
                Some(UpstreamDetail {
                    upstream_code: Some("not-applicable-no-paid-recurring-sku".into()),
                    ..UpstreamDetail::default()
                }),
            );
            return (na.clone(), na);
        }
        let Some(scopes) = self.keys.get(&line) else {
            let r = Reason::OverlapKeyUnresolvable;
            return (failed(r), unevaluable(r));
        };
        if !self.read {
            return (
                unevaluable_with(Reason::OverlapPresenceUnevaluable, KEY_ANSWER_REJECTED),
                unevaluable_with(Reason::OverlapKeyUnresolvable, KEY_ANSWER_REJECTED),
            );
        }
        let in_flight = if scopes.iter().any(|k| input.inflight_same_tuple.contains(k)) {
            failed(Reason::OrderInFlightForKey)
        } else {
            PASS
        };
        let batch = match &self.protected {
            Ok(batch) => batch,
            Err(cause) => return (presence_unevaluable(*cause), in_flight),
        };
        let mut unique = PASS;
        for k in scopes {
            if let ProtectedBatch::Protected(protected) = batch {
                match protected.get(k).map(ValidatedProtected::admits) {
                    Some(Ok(true)) => {}
                    Some(Ok(false)) => unique = failed(Reason::OverlapCardinalityExceeded),
                    Some(Err(cause)) => return (presence_unevaluable(cause), in_flight),
                    None => return (unevaluable(Reason::OverlapPresenceUnevaluable), in_flight),
                }
                continue;
            }
            let Some(Some(o)) = self.occupancy.get(k.as_str()) else {
                return (unevaluable(Reason::OverlapPresenceUnevaluable), in_flight);
            };
            if let MaxConcurrentActive::Limit(n) = o.max_concurrent_active {
                let proposed = self.proposed.get(k).copied().unwrap_or(0);
                let others = match o.count_scope {
                    OccupancyCountScope::PerPayer => {
                        input.inflight_other_orders.get(k).copied().unwrap_or(0)
                    }
                    OccupancyCountScope::FullTuple => 0,
                };
                if u64::from(o.active_count) + proposed + u64::from(others) > u64::from(n) {
                    unique = failed(Reason::OverlapCardinalityExceeded);
                }
            }
        }
        (unique, in_flight)
    }
}

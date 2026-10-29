//! The activation re-check's pure decision (03 §3.6 "Re-check Activation Preconditions";
//! D-127 outcomes and retry budget; D-144 fulfillment inputs).
//!
//! Workflow executes the algorithm: it reads the order through the composed read (step 1),
//! calls the identity and occupancy ports (steps 3 and 6) and owns the clock (step 9). This
//! module decides over what those reads returned and makes no call. The frozen vectors in
//! `tests/fixtures/activation-recheck-v1.json` are the conformance set Workflow #4787 adopts.
use super::answers::{
    MaxConcurrentActive, Occupancy, OccupancyObservation, OccupancyQuery, PayerProfile,
};
use super::ports::PortAnswer;
use bss_orders_sdk::catalog::{OrderState, Reason};
use std::time::Duration;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// One stored order line's fulfillment inputs (D-144).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLine {
    /// The line.
    pub line_id: Uuid,
    /// The legacy scalar overlap key (`orders_order_line.overlap_scope_key`), set only on rows
    /// committed before item keys (D-236 item 6); `None` on every row that stores
    /// `overlap_item_keys`, and on a keyless line.
    pub overlap_scope_key: Option<String>,
    /// Legacy scalar product identity, retained when supplied.
    pub catalog_subscription_product_key: Option<String>,
    /// The authoritative `item_keys_v1` collection (`orders_order_line.overlap_item_keys`): its
    /// distinct `overlap_scope_key` values are the line's stored keys; `Some([])` is proven
    /// keyless, `None` is a legacy row read through the scalar.
    pub overlap_item_keys: Option<Vec<super::item_keys::ItemOverlapKey>>,
    /// The accepted-binding deadline; exclusive, so `deadline <= now` is expired (D-191).
    pub binding_deadline: Option<OffsetDateTime>,
}

impl StoredLine {
    /// One stored `orders_order_line` row's re-check inputs (D-144): the scalar key, the product
    /// key and the item-key collection exactly as stored, and the accepted-binding deadline
    /// (the line's `OrderPin::activation_deadline()`; `None` for a line without a pin). The
    /// composed read (S6-01) and Workflow decode through this, never the scalar column alone.
    ///
    /// # Errors
    /// [`Reason::OverlapPresenceUnevaluable`] for a collection that does not decode as
    /// `item_keys_v1` or for mixed or invalid evidence ([`Self::scopes`]).
    pub fn from_stored(
        line_id: Uuid,
        overlap_scope_key: Option<&str>,
        catalog_subscription_product_key: Option<&str>,
        overlap_item_keys: Option<&serde_json::Value>,
        binding_deadline: Option<OffsetDateTime>,
    ) -> Result<Self, Reason> {
        let overlap_item_keys = overlap_item_keys
            .map(|v| serde_json::from_value::<Vec<super::item_keys::ItemOverlapKey>>(v.clone()))
            .transpose()
            .map_err(|_| Reason::OverlapPresenceUnevaluable)?;
        let line = Self {
            line_id,
            overlap_scope_key: overlap_scope_key.map(str::to_owned),
            catalog_subscription_product_key: catalog_subscription_product_key.map(str::to_owned),
            overlap_item_keys,
            binding_deadline,
        };
        line.scopes()?;
        Ok(line)
    }

    /// Distinct scopes after rejecting malformed or mixed persisted evidence.
    /// # Errors
    /// Returns the existing unevaluable outcome for invalid stored evidence.
    pub fn scopes(&self) -> Result<std::collections::BTreeSet<&str>, Reason> {
        if let Some(items) = &self.overlap_item_keys {
            if self.overlap_scope_key.is_some()
                || self.catalog_subscription_product_key.is_some()
                || items.iter().enumerate().any(|(i, item)| {
                    !item.valid()
                        || item.selection.line_id != self.line_id
                        || items[..i].iter().any(|other| {
                            other.selection.item_id == item.selection.item_id
                                && other.selection.dimension_value == item.selection.dimension_value
                        })
                })
            {
                return Err(Reason::OverlapPresenceUnevaluable);
            }
            Ok(super::item_keys::distinct_scopes(items))
        } else if self
            .overlap_scope_key
            .as_ref()
            .is_none_or(|s| super::item_keys::valid_text(s))
            && self
                .catalog_subscription_product_key
                .as_ref()
                .is_none_or(|s| super::item_keys::valid_text(s))
        {
            Ok(self.overlap_scope_key.as_deref().into_iter().collect())
        } else {
            Err(Reason::OverlapPresenceUnevaluable)
        }
    }
}

/// The current order version as step 1's composed read returns it: the D-144 fulfillment
/// inputs plus what step 2 reads.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredVersion {
    /// Trusted stored order identity; absent in legacy v1 vectors.
    pub order_id: Option<Uuid>,
    /// Trusted stored resource identity; absent in legacy v1 vectors.
    pub resource_tenant_id: Option<Uuid>,
    /// The order's current state.
    pub state: OrderState,
    /// The order's current version.
    pub current_version: u32,
    /// The market currency frozen at submit.
    pub market_currency: String,
    /// The market region frozen at submit.
    pub market_region: String,
    /// The payer at submit/amendment (the occupancy query's payer).
    pub payer_tenant_id: Uuid,
    /// The version's lines, in line order.
    pub lines: Vec<StoredLine>,
}

/// Everything the decision reads: the stored version, the two port answers and Workflow's clock.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecheckInput {
    /// Workflow's `current_version` argument: the version whose fulfillment began.
    pub requested_version: u32,
    /// Step 1's read.
    pub stored: StoredVersion,
    /// Step 3: the payer's current commercial profile.
    pub identity: PortAnswer<PayerProfile>,
    /// Step 6: occupancy per stored overlap key (the distinct scopes of [`StoredLine::scopes`]), keyed by `overlap_scope_key`; conflicting duplicate rows defer. Not read when no stored line carries an overlap key.
    pub occupancy: PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>>,
    /// Step 9: the live Workflow clock.
    pub now: OffsetDateTime,
}

/// Why the order is not dispatchable (step 2).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotDispatchable {
    /// `on_hold`: wait for resume, then re-run from step 1.
    Held,
    /// Terminal: end the attempt. Workflow re-reads the observed state itself (03 §3.6 step 2),
    /// so the variant carries none.
    Terminal,
    /// The current version moved past the requested one.
    Superseded {
        /// The observed current version.
        current_version: u32,
    },
}

/// The re-check's one outcome (D-127).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    /// Early-abort pass; not an admission guarantee.
    Proceed,
    /// The first rejecting step's affected lines, in line order, each with that step's reason
    /// (`market-divergence`, `overlap-collision` or `order-binding-expired`).
    Reject {
        /// `(line_id, reason)` per affected line.
        lines: Vec<(Uuid, Reason)>,
    },
    /// The order is held, terminal or superseded.
    NotDispatchable(NotDispatchable),
    /// A port could not be evaluated: `identity-party-unavailable` or
    /// `overlap-presence-unevaluable`.
    Defer(Reason),
}

/// `activation-recheck-retry-budget`: the maximum number of re-check attempts and the
/// wall-clock bound across them.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    /// Maximum re-check attempts.
    pub attempts: u32,
    /// Wall-clock bound across the attempts.
    pub within: Duration,
}

/// The D-127 baseline: 3 attempts over at most 60 s. Workflow enforces it; nothing here does.
pub const RETRY_BUDGET: RetryBudget = RetryBudget {
    attempts: 3,
    within: Duration::from_secs(60),
};

/// Decide the re-check (03 §3.6 steps 2–9) over the stored version and the port answers. The
/// first returning step wins; a `Reject` carries only that step's lines and reason.
///
/// Step 6 is skipped entirely when no stored line carries an overlap key: no occupancy read is
/// needed, so an unavailable occupancy answer cannot defer the re-check (Workflow need not call
/// the port at all, and whatever it passes is ignored).
#[must_use]
pub fn recheck(input: &RecheckInput) -> Recheck {
    let stored = &input.stored;
    // Step 2: held, then terminal, then superseded.
    if stored.state == OrderState::OnHold {
        return Recheck::NotDispatchable(NotDispatchable::Held);
    }
    if OrderState::TERMINAL.contains(&stored.state) {
        return Recheck::NotDispatchable(NotDispatchable::Terminal);
    }
    if stored.current_version != input.requested_version {
        return Recheck::NotDispatchable(NotDispatchable::Superseded {
            current_version: stored.current_version,
        });
    }
    // Steps 3–5: market re-derived from the payer's current profile.
    let profile = match &input.identity {
        PortAnswer::Answered(profile) => profile,
        PortAnswer::Unavailable(_) | PortAnswer::Skipped(_) => {
            return Recheck::Defer(Reason::IdentityPartyUnavailable);
        }
    };
    if profile.currency != stored.market_currency || profile.region != stored.market_region {
        return reject_all(stored.lines.iter(), Reason::MarketDivergence);
    }
    // Steps 6–8: occupancy per stored key; an absent answer is unevaluable, never a default.
    // Without any stored key there is nothing to read, so the occupancy answer is not consulted.
    if stored.lines.iter().any(|l| l.scopes().is_err()) {
        return Recheck::Defer(Reason::OverlapPresenceUnevaluable);
    }
    let keyed = stored
        .lines
        .iter()
        .any(|l| l.scopes().is_ok_and(|s| !s.is_empty()));
    if keyed && let Some(rejected) = overlap(stored, &input.occupancy) {
        return rejected;
    }
    // Step 9: an elapsed accepted-binding deadline (exclusive).
    let expired: Vec<&StoredLine> = stored
        .lines
        .iter()
        .filter(|l| l.binding_deadline.is_some_and(|d| d <= input.now))
        .collect();
    if !expired.is_empty() {
        return reject_all(expired.into_iter(), Reason::OrderBindingExpired);
    }
    Recheck::Proceed
}

/// Steps 6–8 over the stored keys: `Some` defers (unavailable answer or a key without a row) or
/// rejects the colliding lines; `None` passes. Conflicting duplicate rows defer.
fn overlap(
    stored: &StoredVersion,
    occupancy: &PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>>,
) -> Option<Recheck> {
    let PortAnswer::Answered(occupancy) = occupancy else {
        return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
    };
    let protected = if occupancy
        .iter()
        .any(|(_, o)| matches!(o, OccupancyObservation::ProtectedFullTupleV1(_)))
    {
        let Ok(expected) = protected_requests(stored) else {
            return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
        };
        if expected.values().any(|(q, _)| q.own_context.is_none()) {
            return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
        }
        match super::protected::validate_batch(&expected, occupancy) {
            Ok(super::protected::ProtectedBatch::Protected(map)) => Some(map),
            Ok(super::protected::ProtectedBatch::Legacy) => None,
            Err(_) => return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable)),
        }
    } else {
        None
    };
    let mut colliding = Vec::new();
    for line in &stored.lines {
        let scopes = line.scopes().ok()?;
        let mut collision = false;
        for key in scopes {
            if let Some(protected) = &protected {
                match protected.get(key).and_then(|p| p.admits().ok()) {
                    Some(admits) => collision |= !admits,
                    None => return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable)),
                }
                continue;
            }
            let rows: Vec<_> = occupancy
                .iter()
                .filter(|(k, _)| k.overlap_scope_key == key)
                .map(|(_, a)| a)
                .collect();
            let Some(answer) = rows.first() else {
                return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
            };
            if rows.iter().any(|other| other != answer) {
                return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
            }
            if let OccupancyObservation::LegacyActiveV1(answer) = answer {
                collision |= collides(answer, pending(&stored.lines, key));
            } else {
                return Some(Recheck::Defer(Reason::OverlapPresenceUnevaluable));
            }
        }
        if collision {
            colliding.push(line);
        }
    }
    if colliding.is_empty() {
        None
    } else {
        Some(reject_all(colliding.into_iter(), Reason::OverlapCollision))
    }
}

/// This order's lines carrying `key` (all are pending before the first activation).
/// Both explicitly declared count scopes use only this pending basket; the D-180
/// other-resource Orders addend belongs to submit assessment, not the activation recheck.
fn pending(lines: &[StoredLine], key: &str) -> u64 {
    let count = lines
        .iter()
        .filter(|l| l.scopes().is_ok_and(|scopes| scopes.contains(key)))
        .count();
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// `activeCount + pending > maxConcurrentActive`; `Unlimited` never collides.
fn collides(answer: &Occupancy, pending: u64) -> bool {
    match answer.max_concurrent_active {
        MaxConcurrentActive::Limit(n) => {
            u64::from(answer.active_count).saturating_add(pending) > u64::from(n)
        }
        MaxConcurrentActive::Unlimited => false,
    }
}

fn reject_all<'a>(lines: impl Iterator<Item = &'a StoredLine>, reason: Reason) -> Recheck {
    Recheck::Reject {
        lines: lines.map(|l| (l.line_id, reason)).collect(),
    }
}

/// Trusted occupancy requests for a future Workflow adapter. No caller association can
/// override the stored identities. Missing legacy identities cannot authorize conversion.
/// # Errors
/// Refuses malformed persisted keys; absent identities yield legacy requests without context.
pub fn occupancy_queries(stored: &StoredVersion) -> Result<Vec<OccupancyQuery>, Reason> {
    protected_requests(stored)
        .map(|m| m.into_values().map(|(q, _)| q).collect())
        .map_err(|()| Reason::OverlapPresenceUnevaluable)
}

type ExpectedOccupancy =
    std::collections::BTreeMap<String, (OccupancyQuery, std::collections::BTreeSet<Uuid>)>;
fn protected_requests(stored: &StoredVersion) -> Result<ExpectedOccupancy, ()> {
    let mut expected: ExpectedOccupancy = std::collections::BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for line in &stored.lines {
        if !seen.insert(line.line_id) {
            return Err(());
        }
        for scope in line.scopes().map_err(|_| ())? {
            let entry = expected.entry(scope.to_owned()).or_insert_with(|| {
                (
                    OccupancyQuery {
                        own_context: None,
                        payer_tenant_id: stored.payer_tenant_id,
                        resource_tenant_id: stored.resource_tenant_id,
                        overlap_scope_key: scope.to_owned(),
                    },
                    std::collections::BTreeSet::new(),
                )
            });
            entry.1.insert(line.line_id);
        }
    }
    for (query, lines) in expected.values_mut() {
        if let (Some(order_id), Some(resource)) = (stored.order_id, stored.resource_tenant_id) {
            query.own_context = Some(super::protected::OwnContext {
                order_id,
                version: stored.current_version,
                payer_tenant_id: stored.payer_tenant_id,
                resource_tenant_id: resource,
                line_ids: lines.iter().copied().collect(),
            });
        }
    }
    Ok(expected)
}

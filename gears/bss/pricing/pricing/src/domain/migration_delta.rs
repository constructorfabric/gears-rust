//! Migration safety deltas — what a migration must not do quietly
//! (`inst-ms-deltas`, `inst-md-locks`, `inst-md-entitlements`, `inst-md-addons`,
//! `inst-md-boundary`, `inst-md-published`, `inst-cl-exclude`, `inst-cl-source`,
//! D-36, D-108, K3).
//!
//! The `DeltaAnalyzer` of §1.7. Every function here is pure: it decides over
//! facts a caller has already read, because the reads are queries against plans
//! and subscriptions and the judgement is the part that must be identical at
//! schedule time and at execution start (D-36 re-resolves the same rule against
//! fresher state, and a second spelling of it would let the two answer
//! differently).
//!
//! # Two weights, and the difference is the whole design
//!
//! **Blocking** deltas stop the schedule: entitlement overflow, invalid or
//! missing-required add-ons, and a broken `(currency, region, frequency)`
//! boundary. The operator resolves them — change the target, scope the
//! subscription out, or fix the add-on — and an unresolved blocking delta
//! **never persists a schedule** (§7), which is why
//! [`DeltaReport::ensure_schedulable`] refuses rather than records.
//!
//! **Excluded** is not blocking and is the opposite posture: a contract-locked
//! subscription is *reported and left alone* (`inst-md-locks`,
//! `inst-cl-exclude`). Nothing is owed by the operator, because the lock is
//! Contracts' and the correct outcome is that this subscriber does not move. A
//! design that made locks blocking would let one contracted account veto a whole
//! plan's consolidation.
//!
//! **Informational** is neither: a subscriber on an `existing_grandfathered` row
//! is surfaced so the operator sees the price impact before confirm
//! (`inst-md-boundary`), and that is all.
//!
//! # The lock registry does not exist, so every subscription reads locked
//!
//! `inst-cl-source` sources the lock set from Contracts "at validation time and
//! re-resolved at execution start", and makes a lock-registry outage fail closed.
//! **There is no lock registry in the built system** — no client, no contract
//! type, no counterpart gear, exactly as the D-79 in-flight lane is absent one
//! slice over. [`ContractLockSet::fail_closed`] is therefore the only value this
//! module is handed today, and under it **every** in-scope subscription reads as
//! locked and is excluded.
//!
//! That is the same shape and the same sign as `domain::retirement`'s
//! [`PresenceMap`](crate::domain::retirement::PresenceMap) under D-182: the act
//! still runs, and it *declines to move anyone* rather than refusing outright.
//! It is chosen over the literal reading of `inst-cl-source`'s "fails the
//! mutation closed" — which would refuse every schedule and make the whole
//! migration plane unreachable — because both readings keep the hard guarantee
//! ("the lock is never broken") absolutely, and only this one leaves an operator
//! able to see a delta report at all. **The divergence is reported rather than
//! smoothed over** and is in the hand-back's documentation register: it wants a
//! decision of D-182's kind, stating once which of the two an absent registry is.
//!
//! [`analyze`] is written against the set and not against its absence, so the day
//! a Contracts client lands a [`ContractLockSet::resolved`] flows through the same
//! code and the behaviour becomes what `inst-md-locks` describes with no edit here.

use std::collections::BTreeSet;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::contracts::{EntitlementGrants, GrantSet};
use crate::domain::error::DomainError;
use crate::domain::plan_shape::PlanPhase;

/// Which subscriptions an active contract binds (`inst-cl-source`, D-36).
///
/// [`crate::domain::retirement::PresenceMap`]'s shape, deliberately: it is the
/// same question — "does this cross-gear lane say this thing is spoken for" — and
/// giving it a second shape would give the two lanes two fail-closed postures to
/// keep true.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractLockSet {
    /// The subscriptions Contracts reported an active lock on. Meaningless when
    /// `fail_closed` is set, and empty there so a reader that ignores the flag
    /// still cannot conclude "unlocked" from a lookup.
    locked: BTreeSet<Uuid>,
    /// The registry could not be asked. Every lookup then reads locked.
    fail_closed: bool,
}

impl ContractLockSet {
    /// Contracts answered: exactly `locked` are bound.
    #[must_use]
    pub fn resolved(locked: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            locked: locked.into_iter().collect(),
            fail_closed: false,
        }
    }

    /// The registry is absent, out, or timed out (`inst-cl-source`'s outage
    /// clause; the built system's only case — see the module doc).
    #[must_use]
    pub fn fail_closed() -> Self {
        Self {
            locked: BTreeSet::new(),
            fail_closed: true,
        }
    }

    /// Is this subscription bound by an active contract?
    ///
    /// `true` for **every** subscription when the set failed closed, which is the
    /// whole of the fail-closed posture, stated once here rather than at each
    /// caller.
    #[must_use]
    pub fn is_locked(&self, subscription_ref: Uuid) -> bool {
        self.fail_closed || self.locked.contains(&subscription_ref)
    }

    /// Did the registry answer at all?
    ///
    /// The delta report carries this, because "excluded because Contracts says
    /// so" and "excluded because nobody could be asked" are different facts for
    /// the operator confirming a migration that is about to move nobody.
    #[must_use]
    pub const fn is_unresolved(&self) -> bool {
        self.fail_closed
    }
}

/// A billing boundary a subscription is frozen onto (K3).
///
/// Currency, region and frequency together: `inst-md-boundary` requires the
/// target to publish a row on the subscription's frozen `(currency, region)` pair
/// **of matching frequency**, and a mismatch on any of the three is the same
/// blocking fact. They travel as one value because checking two of the three is
/// the bug the delta exists to prevent.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Boundary {
    /// ISO currency the subscription is frozen on.
    pub currency: String,
    /// The region axis of its canonical scope key.
    pub region: String,
    /// Its billing frequency.
    pub frequency: String,
}

/// What the analyzer knows about one in-scope subscription.
///
/// Read from the **published** read model of the source plan
/// (`inst-md-published`): a draft revision is nobody's truth, and a delta report
/// computed against one would describe a migration that cannot happen.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionFacts {
    /// The subscription this is about.
    pub subscription_ref: Uuid,
    /// Its frozen billing boundary (K3).
    pub boundary: Boundary,
    /// The add-on SKUs it currently carries.
    pub addon_sku_ids: Vec<Uuid>,
    /// The entitlement grant totals it resolves on the source, by grant key.
    pub grants: Vec<(String, i64)>,
    /// Whether it is bound to an `existing_grandfathered` row — informational,
    /// because a migration takes it off legacy pricing and the operator should
    /// see the price impact before confirm.
    pub on_grandfathered_row: bool,
}

/// What the analyzer knows about the migration target.
///
/// Also from the published read model, and for the same reason.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetShape {
    /// Every `(currency, region, frequency)` the target publishes a row on.
    pub covered: Vec<Boundary>,
    /// Add-on SKUs the target offers at all.
    pub offered_addon_sku_ids: Vec<Uuid>,
    /// Add-on SKUs the target **requires**.
    pub required_addon_sku_ids: Vec<Uuid>,
    /// The entitlement grant totals the target resolves, by grant key.
    pub grants: Vec<(String, i64)>,
}

/// The prefix a **quota** grant key carries in the analyzer's vocabulary.
///
/// See [`grant_totals`] for why the two key spaces are namespaced rather than
/// merged.
pub const GRANT_KEY_QUOTA: &str = "quota:";

/// The prefix a **feature-flag** grant key carries in the analyzer's vocabulary.
pub const GRANT_KEY_FLAG: &str = "flag:";

/// Flatten a plan revision's entitlement grants into the analyzer's
/// `(key, total)` vocabulary — D-253, and the half of D-252 that was not
/// plumbing.
///
/// # Why the keys are namespaced
///
/// [`GrantSet`] holds two maps, `feature_flags` and `quotas`, and **nothing
/// makes their key spaces disjoint**. Merging them flat would let a quota and a
/// flag of the same name silently become one entry, and the loser would be
/// whichever this function wrote second — a wrong answer with no symptom. The
/// prefixes also survive into the delta report, where `flag:beta-access` reads
/// to an operator exactly as well as the bare name did.
///
/// # Why a flag is `1` and `0`
///
/// [`TargetShape::grant_of`] reads absent as **zero**, and the overflow rule is
/// `target < source`. Encoding a granted flag as `1` and a withheld or absent
/// one as `0` makes those two rules say the true thing about flags with no new
/// vocabulary: losing a capability the source granted is `0 < 1`, which is the
/// overflow `inst-md-entitlements` exists to surface, and gaining one is not.
///
/// # Why the **minimum** across phases
///
/// A phased plan grants a different set per phase and the analyzer has no phase
/// to select by — [`SubscriptionFacts`] carries none, because the subject side
/// comes from Subscriptions and there is no lane. The minimum is the fail-closed
/// reading and the only one that cannot under-report: a migrated subscriber
/// occupies **every** phase of the target in turn, so a phase granting less than
/// the source did is a loss they will actually meet, whether it is the trial they
/// enter first or the terminal phase they end in. Taking the plan-level set alone
/// would hide exactly that, and taking the maximum would hide it louder.
///
/// An **unphased** plan has no phase map by construction (`phase_map` returns
/// empty below two phases, D-19), and its grants are the plan-level set — so
/// that is what is measured, rather than an empty minimum that would read as
/// "grants nothing".
#[must_use]
pub fn grant_totals(grants: &EntitlementGrants, schedule: &[PlanPhase]) -> Vec<(String, i64)> {
    let phase_map = grants.phase_map(schedule);
    let sets: Vec<&GrantSet> = if phase_map.is_empty() {
        vec![&grants.plan_level]
    } else {
        phase_map.values().collect()
    };

    let mut keys: BTreeSet<String> = BTreeSet::new();
    for set in &sets {
        keys.extend(set.quotas.keys().map(|k| format!("{GRANT_KEY_QUOTA}{k}")));
        keys.extend(
            set.feature_flags
                .keys()
                .map(|k| format!("{GRANT_KEY_FLAG}{k}")),
        );
    }

    keys.into_iter()
        .map(|key| {
            // Absent in a set is zero, for `grant_of`'s reason: a phase that does
            // not mention the key is a phase in which the subscriber does not
            // have it.
            let total = sets
                .iter()
                .map(|set| {
                    key.strip_prefix(GRANT_KEY_QUOTA).map_or_else(
                        || {
                            key.strip_prefix(GRANT_KEY_FLAG)
                                .and_then(|name| set.feature_flags.get(name))
                                .copied()
                                .map_or(0, i64::from)
                        },
                        |name| set.quotas.get(name).copied().unwrap_or(0),
                    )
                })
                .min()
                .unwrap_or(0);
            (key, total)
        })
        .collect()
}

impl TargetShape {
    /// The target's grant total for one key, or zero where it grants none.
    ///
    /// Absent is **zero and not "unconstrained"**: a grant key the target does
    /// not mention is a grant the subscriber stops having, which is exactly the
    /// overflow `inst-md-entitlements` is about.
    #[must_use]
    fn grant_of(&self, key: &str) -> i64 {
        self.grants
            .iter()
            .find(|(k, _)| k == key)
            .map_or(0, |(_, total)| *total)
    }
}

/// Why one subscription cannot, or should not silently, be migrated.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeltaKind {
    /// An active contract binds this subscription: **excluded, never blocking**
    /// (`inst-md-locks`, `inst-cl-exclude`). The lock is never broken and the
    /// operator is owed nothing.
    ContractLocked {
        /// `true` when the exclusion is the fail-closed posture rather than a
        /// positive answer from Contracts.
        unresolved: bool,
    },
    /// The target grants less of a key than the source does (`inst-md-entitlements`).
    /// **Blocking** — the subscriber would silently lose capacity.
    EntitlementOverflow {
        /// The grant key.
        key: String,
        /// What the source resolves.
        source_total: i64,
        /// What the target resolves; zero when the target does not mention it.
        target_total: i64,
    },
    /// An add-on the subscriber carries is not offered by the target
    /// (`inst-md-addons`). **Blocking.**
    AddOnInvalidOnTarget {
        /// The add-on SKU that has nowhere to land.
        addon_sku_id: Uuid,
    },
    /// The target requires an add-on the subscriber does not carry
    /// (`inst-md-addons`). **Blocking.**
    AddOnMissingRequired {
        /// The add-on SKU the target requires.
        addon_sku_id: Uuid,
    },
    /// The target publishes no row on the subscription's frozen boundary
    /// (`inst-md-boundary`, K3). **Blocking**: a cross-currency, cross-region or
    /// cross-frequency move is cancel + new, never an in-place `PlanLink`.
    BoundaryUncovered {
        /// The boundary the subscription is frozen onto.
        boundary: Boundary,
    },
    /// The subscriber sits on an `existing_grandfathered` row
    /// (`inst-md-boundary`). **Informational**: migration takes them off legacy
    /// pricing, and the operator sees the price impact before confirm.
    LeavesGrandfatheredRow,
}

impl DeltaKind {
    /// Does this delta stop the schedule?
    ///
    /// Exactly the three classes §5 types `MIGRATION_BLOCKED`. A contract lock is
    /// **not** one, which is the asymmetry `inst-cl-exclude` exists to state.
    #[must_use]
    pub const fn is_blocking(&self) -> bool {
        matches!(
            self,
            Self::EntitlementOverflow { .. }
                | Self::AddOnInvalidOnTarget { .. }
                | Self::AddOnMissingRequired { .. }
                | Self::BoundaryUncovered { .. }
        )
    }

    /// Is this subscription excluded from the run rather than blocking it?
    #[must_use]
    pub const fn is_exclusion(&self) -> bool {
        matches!(self, Self::ContractLocked { .. })
    }

    /// A token for the report and the refusal.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ContractLocked { .. } => "contract_locked",
            Self::EntitlementOverflow { .. } => "entitlement_overflow",
            Self::AddOnInvalidOnTarget { .. } => "addon_invalid_on_target",
            Self::AddOnMissingRequired { .. } => "addon_missing_required",
            Self::BoundaryUncovered { .. } => "boundary_uncovered",
            Self::LeavesGrandfatheredRow => "leaves_grandfathered_row",
        }
    }
}

/// One subscription and one thing true about migrating it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    /// The subscription.
    pub subscription_ref: Uuid,
    /// What is true about it.
    pub kind: DeltaKind,
}

/// The `delta_report` of §6, and the confirm screen's content.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaReport {
    /// Every delta, of all three weights, in subscription order.
    pub deltas: Vec<Delta>,
    /// Whether the lock registry answered at all (`inst-cl-source`).
    pub locks_unresolved: bool,
}

impl DeltaReport {
    /// The deltas that stop the schedule.
    #[must_use]
    pub fn blocking(&self) -> Vec<&Delta> {
        self.deltas
            .iter()
            .filter(|d| d.kind.is_blocking())
            .collect()
    }

    /// The subscriptions excluded from the run (`inst-cl-exclude`).
    ///
    /// A `BTreeSet` because this is the set the executor honours and the
    /// executor's copy must not depend on the order the analyzer happened to
    /// read subscriptions in.
    #[must_use]
    pub fn excluded(&self) -> BTreeSet<Uuid> {
        self.deltas
            .iter()
            .filter(|d| d.kind.is_exclusion())
            .map(|d| d.subscription_ref)
            .collect()
    }

    /// Refuse a schedule that carries unresolved blocking deltas
    /// (`inst-ms-deltas`, §5's `MIGRATION_BLOCKED`).
    ///
    /// The deltas are **enumerated**: "blocked" alone is not an actionable
    /// sentence when the remedy is to change the target, scope a named
    /// subscription out, or fix a named add-on.
    ///
    /// # Errors
    /// [`DomainError::MigrationBlocked`] listing every blocking delta.
    pub fn ensure_schedulable(&self) -> Result<(), DomainError> {
        let blocking = self.blocking();
        if blocking.is_empty() {
            return Ok(());
        }
        let enumerated = blocking
            .iter()
            .map(|d| format!("{} on subscription {}", d.kind.as_str(), d.subscription_ref))
            .collect::<Vec<_>>()
            .join(", ");
        Err(DomainError::MigrationBlocked(format!(
            "{} blocking delta(s) must be resolved or explicitly scoped out before this migration \
             can be scheduled: {enumerated}",
            blocking.len()
        )))
    }
}

/// Compute every delta for one migration (`inst-ms-deltas`).
///
/// # The order the classes are asked in, and why a lock short-circuits
///
/// A contract-locked subscription is **excluded and nothing else is asked of
/// it**. That is not an optimisation: a locked subscriber is not going to move,
/// so an entitlement overflow on the target they will never reach is not a fact
/// an operator should have to resolve, and reporting it would make a schedule
/// blocked on a subscription the schedule already agreed to leave alone. Under
/// the fail-closed posture this means a migration reports exclusions and no
/// blocking deltas at all, which is the honest description of a run that will
/// move nobody.
#[must_use]
pub fn analyze(
    subscriptions: &[SubscriptionFacts],
    target: &TargetShape,
    locks: &ContractLockSet,
) -> DeltaReport {
    let covered: BTreeSet<&Boundary> = target.covered.iter().collect();
    let offered: BTreeSet<Uuid> = target.offered_addon_sku_ids.iter().copied().collect();
    let mut deltas = Vec::new();

    for subscription in subscriptions {
        let subscription_ref = subscription.subscription_ref;

        if locks.is_locked(subscription_ref) {
            deltas.push(Delta {
                subscription_ref,
                kind: DeltaKind::ContractLocked {
                    unresolved: locks.is_unresolved(),
                },
            });
            continue;
        }

        // K3. Asked first among the blocking classes because it is the one that
        // makes the whole move impossible rather than merely wrong.
        if !covered.contains(&subscription.boundary) {
            deltas.push(Delta {
                subscription_ref,
                kind: DeltaKind::BoundaryUncovered {
                    boundary: subscription.boundary.clone(),
                },
            });
        }

        for addon in &subscription.addon_sku_ids {
            if !offered.contains(addon) {
                deltas.push(Delta {
                    subscription_ref,
                    kind: DeltaKind::AddOnInvalidOnTarget {
                        addon_sku_id: *addon,
                    },
                });
            }
        }
        for required in &target.required_addon_sku_ids {
            if !subscription.addon_sku_ids.contains(required) {
                deltas.push(Delta {
                    subscription_ref,
                    kind: DeltaKind::AddOnMissingRequired {
                        addon_sku_id: *required,
                    },
                });
            }
        }

        for (key, source_total) in &subscription.grants {
            let target_total = target.grant_of(key);
            // Strictly less: a target granting **more** is not an overflow risk,
            // and reporting it would make every generous consolidation blocking.
            if target_total < *source_total {
                deltas.push(Delta {
                    subscription_ref,
                    kind: DeltaKind::EntitlementOverflow {
                        key: key.clone(),
                        source_total: *source_total,
                        target_total,
                    },
                });
            }
        }

        if subscription.on_grandfathered_row {
            deltas.push(Delta {
                subscription_ref,
                kind: DeltaKind::LeavesGrandfatheredRow,
            });
        }
    }

    DeltaReport {
        deltas,
        locks_unresolved: locks.is_unresolved(),
    }
}

#[cfg(test)]
#[path = "migration_delta_tests.rs"]
mod migration_delta_tests;

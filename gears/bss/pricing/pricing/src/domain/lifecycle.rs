//! The row lifecycle state machine.
//!
//! [`LifecycleState::transition`] is the **single source of truth** for what is
//! legal. The database trigger's column whitelist and the authoring surfaces
//! both restate the machine in their own vocabulary; if the legal edge set
//! lived in three places, the one that drifted would be discovered as a
//! published row in a state nothing can move it out of.
//!
//! Legal edges, and nothing else:
//!
//! ```text
//! draft      -> published
//! draft      -> abandoned
//! published  -> superseded
//! published  -> retired
//! ```
//!
//! `draft -> abandoned` is how a revision is **discarded**, and it exists
//! because the obvious alternative does not survive the key (D-145). Deleting
//! the row frees its `revision`, `(plan_id, revision)` is a durable name — the
//! grant table is keyed by it, every revision-scoped child copies under it, the
//! audit trail records it — and the next draft mints the number again. A client
//! holding the discarded `plan/2`'s row version then submits against the *new*
//! `plan/2` and its precondition **passes**, most reachably at the initial
//! version every freshly minted revision carries: a lost update arriving through
//! the key instead of the version.
//!
//! `published -> superseded` has exactly **two** sanctioned producers: the
//! supersession unit, and the grandfathering cutover commit (D-100) whose
//! `all_subscriptions` successor lands on the predecessor's own key. Both set
//! `supersedes_price_id` on the successor, which is what gives the unit guard
//! its comparison referent (D-127). No third path may produce this flip.
//!
//! There is deliberately **no self-edge**: re-publishing a published row is not
//! a no-op, it is a request whose intent the machine cannot infer, and treating
//! it as one would let a retry silently pass through the same guard twice.

use std::fmt;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// The lifecycle state of a plan revision or price row.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifecycleState {
    /// Authoring state. The only state whose content may still change.
    #[default]
    Draft,
    /// A discarded draft revision, kept as a tombstone. **Terminal**, and a
    /// **plan-revision** state only: `pricing_price` draft rows stay deletable
    /// (§4.3, `inst-ps-nodelete`), so its `CHECK` does not admit this token.
    ///
    /// The tombstone is the whole mechanism: it holds the `revision` number the
    /// discarded draft consumed, so minting never hands that number out twice
    /// (D-145).
    Abandoned,
    /// Live and consumer-visible: projected into the read model, pinnable,
    /// covered by the append-only discipline.
    Published,
    /// Replaced on its canonical scope key by a successor. The row is retained
    /// as history and its window may still be active until the changeover;
    /// what it is not is the **current** row on its key.
    Superseded,
    /// Ended. **Terminal**, and the terminality is load-bearing: a retired plan
    /// can never publish again, so nothing would ever re-project it. That is
    /// precisely why retirement had to become a publish unit of its own
    /// (D-128) — validation, a pending `CatalogVersion` ref, a plan-subject
    /// re-projection, a warm — rather than an in-place edit of a frozen
    /// version. Had it stayed an edit, the read model would have advertised a
    /// retired plan as sellable **permanently**, with no later publish able to
    /// correct it.
    Retired,
}

impl LifecycleState {
    /// Every state, stable order.
    pub const ALL: &'static [Self] = &[
        Self::Draft,
        Self::Abandoned,
        Self::Published,
        Self::Superseded,
        Self::Retired,
    ];

    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Abandoned => "abandoned",
            Self::Published => "published",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
        }
    }

    /// May this row's **content** still change?
    ///
    /// Only in `draft`. A published row is immutable except for the two
    /// whitelisted column moves the trigger allows — the state flips this
    /// machine sanctions, and monotonic tightening of `grandfather_until` —
    /// neither of which is content.
    ///
    /// `abandoned` is **not** mutable, and it is the answer that matters most
    /// here: a tombstone that could be edited back into a shape, or re-abandoned
    /// under a tag a caller still holds, would be a second row living at a number
    /// the store promises names one thing forever. What a caller who reaches for
    /// one gets is the frozen refusal, naming the state — the same answer a
    /// published revision gives, for the same reason.
    #[must_use]
    pub const fn is_content_mutable(self) -> bool {
        matches!(self, Self::Draft)
    }

    /// Is this the **current** revision of its subject?
    ///
    /// `published` **or `retired`** (D-128). The wider set is not a convenience:
    /// the projector sources a plan subject from its current revision, and
    /// after retirement flips the only published one there would otherwise be
    /// no referent at all — a re-warm or a degraded re-drive would project an
    /// empty plan subject and break resolution for exactly the in-flight
    /// subscribers a retired plan must keep pricing. `superseded` is excluded
    /// because a successor has taken the key.
    ///
    /// `abandoned` is excluded too, and that exclusion is what lets the state be
    /// added at all: a tombstone never published, so nothing was ever projected
    /// from it and nothing may resolve through it. It is also the predicate the
    /// `uq_pricing_plan_current` partial index restates, so counting a tombstone
    /// as current would collide the plan's real current revision with every
    /// draft it ever discarded.
    #[must_use]
    pub const fn is_current_revision(self) -> bool {
        matches!(self, Self::Published | Self::Retired)
    }

    /// Has this revision's content **ever been live** to a consumer?
    ///
    /// A different question from [`Self::is_current_revision`] and the difference is
    /// `superseded`: a superseded revision is not the current one, and its content
    /// *was* published — a consumer resolved through it until its successor took the
    /// key. That is what makes it the right referent for *"what was live before this
    /// publish"*, which is the operand `infra::bundle::declared_act` diffs against.
    ///
    /// `draft` and `abandoned` are the two that answer `false`, and both matter.
    /// A draft's content has never been anybody's but its author's. A tombstone's
    /// never published either (`is_current_revision`'s own note says why it is
    /// excluded there), and it holds a revision **number** — so a walk backwards
    /// through the chain that counted one would diff against content no approver
    /// ever saw and no consumer ever received.
    ///
    /// Written as a `match` over the closed enum rather than as `!is_content_mutable()`
    /// so the day a sixth state arrives the compiler asks this question too; the
    /// negation would have answered for it, silently and probably wrongly.
    #[must_use]
    pub const fn has_ever_been_live(self) -> bool {
        match self {
            Self::Published | Self::Superseded | Self::Retired => true,
            Self::Draft | Self::Abandoned => false,
        }
    }

    /// Is a move from `self` to `next` legal?
    #[must_use]
    pub const fn can_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Draft, Self::Published | Self::Abandoned)
                | (Self::Published, Self::Superseded | Self::Retired)
        )
    }

    /// Assert a transition, refusing every edge the machine does not sanction.
    ///
    /// One refused edge answers in its own words. Re-publishing a **retired**
    /// subject is `PLAN_RETIRED_NO_SUCCESSOR` (D-146), which **replaced** that
    /// clause of the `LIFECYCLE_FORBIDDEN` gloss rather than joining it: the
    /// operator has no next action at all, where every other refusal here is
    /// either a caller bug or a request that can be reshaped. Told the same code
    /// as the rest, a consumer would have to read prose to learn that this one
    /// can never succeed.
    ///
    /// # Errors
    ///
    /// [`DomainError::PlanRetiredNoSuccessor`] for `retired -> published`;
    /// [`DomainError::LifecycleForbidden`] for every other edge outside the
    /// four legal ones — including every self-edge, every move out of a terminal
    /// state (`abandoned` and `retired` both), and every attempt to walk the
    /// machine backwards.
    pub fn transition(self, next: Self) -> Result<(), DomainError> {
        if self.can_transition(next) {
            return Ok(());
        }
        if matches!((self, next), (Self::Retired, Self::Published)) {
            return Err(DomainError::PlanRetiredNoSuccessor(format!(
                "{self} -> {next}"
            )));
        }
        Err(DomainError::LifecycleForbidden(format!("{self} -> {next}")))
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

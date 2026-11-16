//! The consume scope each member holds.
//!
//! The scope is kept on the member, so it goes away with the member on every
//! removal path. Each write, a store, a swap or a clear, is a
//! compare-and-swap on the scope's generation under the coordinator lock, so
//! a writer never overwrites a scope it did not read. The coordinator numbers
//! the generations itself and never reuses one for a member, also after a
//! clear, so a generation read once names one scope only.
//!
//! # Lock order
//!
//! The coordinator lock first, then a member's watch channel's own lock:
//! `write_scope` and `scope_of` take the scope channel's lock inside the
//! coordinator lock, as the coordinator does for `generations`, and nothing
//! takes the two the other way round. A receiver's `borrow()` takes that
//! channel lock too (tokio's internal `RwLock`), so receivers are not
//! lock-free. Clone the value out of `borrow()` and never hold the returned
//! `Ref` across a call into the coordinator: against a concurrent
//! `write_scope` that deadlocks.

use std::collections::HashSet;
use std::sync::{Arc, PoisonError};

use gts::GtsTypeId;
use tokio::sync::watch;
use tokio::time::Instant;
use toolkit::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ConsumerGroupCoordinator, CoordinatorState, MemberEntry};
use crate::domain::join_scope::GroupKey;
use crate::domain::streaming::scope::CompiledScope;

/// The caller a scope was evaluated for: every part of its identity the PDP
/// sees.
///
/// The bearer token is left out on purpose: two contexts that differ only in
/// their token give equal principals. Tying a cached decision to the token is
/// left to the `AuthZ` protocol (#5502).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePrincipal {
    subject_id: Uuid,
    subject_type: Option<String>,
    subject_tenant_id: Uuid,
    token_scopes: Vec<String>,
}

/// The version of a member's scope that a write compares.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeGeneration(u64);

impl ScopeGeneration {
    /// No generation: what a member has before its first store.
    pub const NONE: Self = Self(0);

    /// The generation of a member's `n`-th applied store, for a test that
    /// asserts a whole scope from outside this module.
    #[cfg(test)]
    pub(crate) const fn nth(n: u64) -> Self {
        Self(n)
    }

    /// The generation after this one. It saturates instead of wrapping, and
    /// the bound is out of reach: at one store per millisecond, `u64` lasts
    /// about 584 million years.
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A member's consume scope: one authorized group per interest group.
///
/// A scope a member holds is always built by
/// [`ConsumerGroupCoordinator::write_scope`], which assigns its generation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedScope {
    /// The version [`ConsumerGroupCoordinator::write_scope`] compares,
    /// assigned by the coordinator: 1 for the member's first store, plus 1
    /// for each applied store, never reused after a clear.
    pub generation: ScopeGeneration,
    /// The caller the scope was evaluated for.
    pub principal: ScopePrincipal,
    /// The authorized groups, in interest-group order.
    pub groups: Arc<[AuthorizedGroup]>,
}

/// What a writer stores: a scope without its generation, which the
/// coordinator assigns.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeContent {
    /// The caller the scope was evaluated for.
    pub principal: ScopePrincipal,
    /// The authorized groups, in interest-group order.
    pub groups: Arc<[AuthorizedGroup]>,
}

/// The PDP's consume answer for one interest group, until it expires.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGroup {
    /// The interest group the answer is for.
    pub key: GroupKey,
    /// The compiled answer.
    pub scope: Arc<CompiledScope>,
    /// The event types registered on the topic that the group's patterns
    /// match.
    pub candidates: Arc<HashSet<GtsTypeId>>,
    /// The candidates could not be refreshed and are the previous ones.
    pub candidates_stale: bool,
    /// When the answer stops serving.
    pub expires_at: Instant,
}

/// What a scope write expects the member to hold.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expected {
    /// No scope.
    Absent,
    /// A scope of this generation.
    Generation(ScopeGeneration),
}

/// A compare-and-swap on a member's scope.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeWrite {
    /// Hold a scope built from `content`, under the next generation, from
    /// now on.
    Store {
        expected: Expected,
        content: ScopeContent,
    },
    /// Hold no scope from now on.
    Clear { expected: Expected },
}

/// What a scope write did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ScopeWriteOutcome {
    /// The member held what the write expected and now holds what it wrote.
    Applied,
    /// The subscription is no longer a member; nothing was written.
    MemberGone,
    /// The member holds something other than what the write expected;
    /// nothing was written.
    Superseded,
}

impl From<&SecurityContext> for ScopePrincipal {
    fn from(ctx: &SecurityContext) -> Self {
        Self {
            subject_id: ctx.subject_id(),
            subject_type: ctx.subject_type().map(str::to_owned),
            subject_tenant_id: ctx.subject_tenant_id(),
            token_scopes: ctx.token_scopes().to_vec(),
        }
    }
}

impl CachedScope {
    /// Whether this scope can serve `principal` at `now`: it was evaluated for
    /// that principal and none of its groups has expired.
    #[must_use]
    pub fn serves(&self, principal: &ScopePrincipal, now: Instant) -> bool {
        self.principal == *principal && self.groups.iter().all(|group| group.expires_at > now)
    }
}

impl ScopeWrite {
    fn expected(&self) -> Expected {
        match self {
            Self::Store { expected, .. } | Self::Clear { expected } => *expected,
        }
    }
}

impl CoordinatorState {
    fn member(&self, sub_id: Uuid) -> Option<&MemberEntry> {
        let group_id = self.index.get(&sub_id)?;
        self.groups.get(group_id)?.members.get(&sub_id)
    }

    fn member_mut(&mut self, sub_id: Uuid) -> Option<&mut MemberEntry> {
        let group_id = self.index.get(&sub_id)?;
        self.groups.get_mut(group_id)?.members.get_mut(&sub_id)
    }
}

impl ConsumerGroupCoordinator {
    /// The scope the member holds; `None` when it holds none or is not a
    /// member.
    #[must_use]
    pub fn scope_of(&self, sub_id: Uuid) -> Option<Arc<CachedScope>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.member(sub_id)?.scope.borrow().clone()
    }

    /// A receiver of the member's scope, for a session to wait on; `None`
    /// when the subscription is not a member.
    ///
    /// It wakes on every applied `Store`, also one whose content is
    /// unchanged, and on a `Clear` of a held scope. When the member is
    /// removed, on any path, the sender is dropped with it and the receiver
    /// sees the channel close: that close is the removal signal. Clone the
    /// value out of `borrow()`; see the module's lock order.
    #[must_use]
    pub fn watch_scope(&self, sub_id: Uuid) -> Option<watch::Receiver<Option<Arc<CachedScope>>>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        Some(state.member(sub_id)?.scope.subscribe())
    }

    /// Writes the member's scope if it holds what the write expects.
    ///
    /// | member holds | `expected` | write | outcome |
    /// |---|---|---|---|
    /// | not a member | any | any | `MemberGone` |
    /// | no scope | `Absent` | `Store(c)` | `Applied`, holds `c` |
    /// | no scope | `Absent` | `Clear` | `Applied`, still none, no wake |
    /// | generation `g` | `Generation(g)` | `Store(c)` | `Applied`, holds `c` |
    /// | generation `g` | `Generation(g)` | `Clear` | `Applied`, holds none |
    /// | no scope | `Generation(_)` | any | `Superseded` |
    /// | a scope | `Absent` | any | `Superseded` |
    /// | generation `g` | `Generation(h)`, `h != g` | any | `Superseded` |
    ///
    /// An applied `Store` holds `c` under a generation the coordinator
    /// assigns: 1 for the member's first store, plus 1 for each applied store,
    /// never reused after a `Clear`. `MemberGone` and `Superseded` write
    /// nothing and assign no generation. Receivers wake on every applied
    /// `Store` and on a `Clear` of a held scope. The compare, the write and
    /// the generation assignment run under the coordinator lock, so of
    /// several writers expecting the same state, exactly one applies.
    pub fn write_scope(&self, sub_id: Uuid, write: ScopeWrite) -> ScopeWriteOutcome {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(MemberEntry {
            scope,
            scope_generation,
            ..
        }) = state.member_mut(sub_id)
        else {
            return ScopeWriteOutcome::MemberGone;
        };
        let mut outcome = ScopeWriteOutcome::Superseded;
        scope.send_if_modified(|current| {
            let holds_expected = match (write.expected(), current.as_deref()) {
                (Expected::Absent, None) => true,
                (Expected::Generation(generation), Some(held)) => generation == held.generation,
                _ => false,
            };
            if !holds_expected {
                return false;
            }
            outcome = ScopeWriteOutcome::Applied;
            match write {
                ScopeWrite::Store {
                    content: ScopeContent { principal, groups },
                    ..
                } => {
                    let generation = scope_generation.next();
                    *scope_generation = generation;
                    *current = Some(Arc::new(CachedScope {
                        generation,
                        principal,
                        groups,
                    }));
                    true
                }
                ScopeWrite::Clear { .. } => current.take().is_some(),
            }
        });
        outcome
    }
}

#[cfg(test)]
#[path = "member_scope_tests.rs"]
mod member_scope_tests;

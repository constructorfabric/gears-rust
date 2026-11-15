//! Whether an event belongs to a subscription, and whether delivery hands it
//! over.
//!
//! The subscription predicates: the event's topic equals the interest's topic,
//! its tenant is the interest's tenant, its type matches one of the interest's
//! GTS patterns, and, when the interest carries a subject-type selector, its
//! subject type matches one of those patterns. An interest may also carry a
//! `FilterSpec`, but the expression language, the engine contract and the
//! evaluation semantics belong to ADR-0005; this module owns only *where* such
//! an engine is invoked.
//!
//! Compiled once at stream open. Type patterns are already validated
//! `GtsIdPattern`s (the wildcard grammar is enforced when a pattern is
//! constructed), so a malformed pattern never reaches a subscription.
//! Selector matching alone is not authorization: production delivery decides
//! each matching event from the member's consume scope
//! ([`crate::domain::streaming::scoped_filter::ScopedFilter`]).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use toolkit::domain_model;

use gts::{GtsId, GtsIdPattern};
use uuid::Uuid;

use crate::domain::consumer_group_coordinator::member_scope::CachedScope;
use crate::domain::error::DomainError;
use crate::domain::model::{Event, FilterSpec, Interest};
use crate::domain::streaming::read::EventBatch;

/// Whether one event is wanted, and whether it is delivered.
///
/// The session calls [`Self::prepare`] once per read batch, then
/// [`Self::admits`] for each event it made decidable, in order. `prepare` is
/// the only await on the delivery path; `admits` never waits.
#[async_trait]
pub trait EventFilter: Send + Sync {
    /// Whether some interest's selector matches `event`. Authorizes nothing.
    fn matches(&self, event: &Event) -> bool;

    /// Gets ready to decide the events of `events` without waiting: all of
    /// them, or only a leading part when deciding more would take more than
    /// one wait. The session delivers that part and reads the rest again.
    ///
    /// # Errors
    ///
    /// Whatever stopped a decision from being made: a failed or late policy
    /// evaluation, or a failed read of an event type. Delivery then ends the
    /// stream without advancing past the batch, never skipping events it
    /// could not decide.
    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        Ok(Prepared::All)
    }

    /// Whether `event`, one [`Self::prepare`] made decidable, is delivered.
    ///
    /// # Errors
    ///
    /// Only on a broken internal invariant, such as no decision prepared
    /// for the event. Delivery then ends the stream as it does for a
    /// `prepare` error.
    fn admits(&self, event: &Event) -> Result<bool, DomainError>;

    /// Takes `scope` as the member's consume scope from now on. Called by
    /// the session whenever the member holds a newer one; a filter that
    /// decides nothing from the scope ignores it.
    fn swap_scope(&self, _scope: Arc<CachedScope>) {}
}

/// Which events of a batch [`EventFilter::prepare`] made decidable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prepared {
    /// Every event of the batch.
    All,
    /// Only the first `n` events, in batch order; the batch is cut before
    /// event `n`, whose decision would have taken another wait. At least 1
    /// for a batch that is not empty, so every pass decides something.
    UpTo(usize),
}

/// One interest, compiled.
#[derive(Debug)]
pub(crate) struct CompiledInterest {
    /// The interest's tenant, which only [`InterestFilter`] compares: the
    /// scoped filter narrows by the ownership filter instead.
    subject_tenant: Uuid,
    /// Canonical GTS patterns, carried by the interest and matched via
    /// [`GtsId::matches_pattern`]. The grammar was validated when each pattern
    /// was constructed (`GtsIdPattern::try_new`), so no re-parsing happens here.
    patterns: Vec<GtsIdPattern>,
    /// Subject-type selector, matched like `patterns`; empty selects every subject type.
    subject_patterns: Vec<GtsIdPattern>,
    /// Carried, not evaluated, and deliberately unread until ADR-0005 lands.
    ///
    /// This is the seam an engine plugs into. Dropping the field instead would
    /// lose the subscription's declared filter at compile time, so a later
    /// engine would have nothing to evaluate and the JOIN-time value would have
    /// been silently discarded.
    #[expect(
        dead_code,
        reason = "the engine that reads it is ADR-0005's, still proposed"
    )]
    filter: Option<FilterSpec>,
}

impl CompiledInterest {
    /// Compiles `interest`.
    pub(crate) fn new(interest: &Interest) -> Self {
        Self {
            subject_tenant: interest.subject_tenant,
            patterns: interest.types.clone(),
            subject_patterns: interest.subject_types.clone(),
            filter: interest.filter.clone(),
        }
    }

    /// Whether the interest's type patterns and subject-type selector match
    /// `event`, whatever its topic and tenant. A type that does not parse
    /// matches no pattern.
    pub(crate) fn selects(&self, event: &Event) -> bool {
        GtsId::try_new(event.r#type.as_ref()).is_ok_and(|event_type| {
            self.patterns
                .iter()
                .any(|pattern| event_type.matches_pattern(pattern))
        }) && self.selects_subject(&OnceCell::new(), event)
    }

    /// Whether this interest's subject-type selector admits the event. No selector
    /// admits every subject type; a subject type that does not parse matches no
    /// selector. `subject_type` holds the event's parsed subject type, shared by
    /// every candidate interest of one event, so it is parsed at most once and
    /// only when a selector reads it.
    fn selects_subject(&self, subject_type: &OnceCell<Option<GtsId>>, event: &Event) -> bool {
        if self.subject_patterns.is_empty() {
            return true;
        }
        subject_type
            .get_or_init(|| GtsId::try_new(event.subject_type.as_ref()).ok())
            .as_ref()
            .is_some_and(|subject_type| {
                self.subject_patterns
                    .iter()
                    .any(|pattern| subject_type.matches_pattern(pattern))
            })
    }
}

/// A subscription's interests, compiled and indexed by topic.
#[domain_model]
#[derive(Debug)]
pub struct InterestFilter {
    /// Keyed by the topic's string form, because that is what an event carries
    /// and a map lookup beats scanning every interest for every event.
    by_topic: HashMap<String, Vec<CompiledInterest>>,
}

impl InterestFilter {
    /// Compiles a subscription's interests, indexing them by topic. The type
    /// patterns are already validated [`GtsIdPattern`]s, so this only groups
    /// them - the pattern grammar is enforced where a pattern is constructed.
    #[must_use]
    pub fn compile(interests: &[Interest]) -> Self {
        let mut by_topic: HashMap<String, Vec<CompiledInterest>> = HashMap::new();

        for interest in interests {
            by_topic
                .entry(interest.topic.as_ref().to_owned())
                .or_default()
                .push(CompiledInterest::new(interest));
        }

        Self { by_topic }
    }

    /// How many interests were compiled, across every topic. For assertions
    /// about what a compile produced; matching is the real interface.
    #[must_use]
    pub fn interest_count(&self) -> usize {
        self.by_topic.values().map(Vec::len).sum()
    }
}

impl EventFilter for InterestFilter {
    fn matches(&self, event: &Event) -> bool {
        // Topic equality is the first predicate, and it is also the index: an
        // event on a topic no interest names cannot match, so the lookup failing
        // is the answer rather than a reason to look elsewhere.
        let Some(candidates) = self.by_topic.get(event.topic.as_ref()) else {
            return false;
        };

        // The type is matched with the canonical `gts` matcher: a wildcard is a
        // trailing `*` covering everything below it, and a bare type id already
        // covers its own derived subtree. A type that does not parse cannot match.
        let Ok(event_type) = GtsId::try_new(event.r#type.as_ref()) else {
            return false;
        };

        let subject_type = OnceCell::new();
        candidates.iter().any(|interest| {
            interest.subject_tenant == event.subject_tenant
                && interest
                    .patterns
                    .iter()
                    .any(|pattern| event_type.matches_pattern(pattern))
                && interest.selects_subject(&subject_type, event)
        })
    }

    /// Delivered when some interest's selector matches in the interest's
    /// tenant: no consume scope and no ownership filter is read, so this is
    /// for tests and benches that open a stream without one.
    fn admits(&self, event: &Event) -> Result<bool, DomainError> {
        Ok(self.matches(event))
    }
}

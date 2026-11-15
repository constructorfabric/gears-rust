//! A specification manager that runs a test's action before each event-type
//! query, then answers as the seeded one does.
//!
//! JOIN, stream open and a stream's scope refresh ask for a topic's
//! event-type candidates on every pass over a group (the refresh on each
//! group it evaluates again), and JOIN then asks which of the group's exact
//! types are registered, a lookup that reads something only when the topic is
//! over the candidate cap. Publish and delivery's single-type evaluation read
//! one event type. A test arms an action on [`SpecQueryHook`] to change state
//! at that point (remove a member, write its scope) or to make the query
//! fail, deterministically and without timing.

use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use gts::GtsTypeId;
use serde_json::Value as JsonValue;
use toolkit_gts::GtsInstanceId;

use crate::domain::backend::BackendResolver;
use crate::domain::error::DomainError;
use crate::domain::model::Topic;
use crate::domain::specification::{
    EventTypeCandidates, ProvisioningLimits, SpecificationManager, TopicState,
};

/// An event-type query the hook acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecQuery {
    /// `get_event_type`.
    EventType,
    /// `get_event_type_candidates`.
    EventTypeCandidates,
    /// `get_registered_event_types`.
    RegisteredEventTypes,
}

/// What an armed action does before a query: `Ok` lets the query reach the
/// seeded manager, an `Err` is what the query returns instead.
type Action = Box<dyn FnMut(SpecQuery) -> Result<(), DomainError> + Send>;

/// The test's handle on the hook: arms an action and counts the queries.
/// Clones share one state.
#[derive(Clone, Default)]
pub struct SpecQueryHook {
    state: Arc<Mutex<HookState>>,
}

#[derive(Default)]
struct HookState {
    action: Option<Action>,
    event_type_queries: usize,
    candidate_queries: usize,
    registered_queries: usize,
}

/// The seeded manager behind the hook. Every method delegates; the three
/// event-type queries run the armed action first.
struct HookedSpecificationManager {
    inner: Arc<dyn SpecificationManager>,
    hook: SpecQueryHook,
}

impl SpecQueryHook {
    /// A hook with no action armed: every query reaches the seeded manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `action` before every later event-type query, in place of any
    /// action armed earlier. The action runs synchronously under the hook's
    /// lock, so it must not query the specification manager itself.
    pub fn arm(&self, action: impl FnMut(SpecQuery) -> Result<(), DomainError> + Send + 'static) {
        self.state().action = Some(Box::new(action));
    }

    /// How many `query` queries reached the hook since the harness was built,
    /// JOIN's included.
    #[must_use]
    pub fn calls(&self, query: SpecQuery) -> usize {
        let state = self.state();
        match query {
            SpecQuery::EventType => state.event_type_queries,
            SpecQuery::EventTypeCandidates => state.candidate_queries,
            SpecQuery::RegisteredEventTypes => state.registered_queries,
        }
    }

    /// `inner` behind this hook.
    pub(crate) fn wrap(
        &self,
        inner: Arc<dyn SpecificationManager>,
    ) -> Arc<dyn SpecificationManager> {
        Arc::new(HookedSpecificationManager {
            inner,
            hook: self.clone(),
        })
    }

    fn before(&self, query: SpecQuery) -> Result<(), DomainError> {
        let mut state = self.state();
        match query {
            SpecQuery::EventType => state.event_type_queries += 1,
            SpecQuery::EventTypeCandidates => state.candidate_queries += 1,
            SpecQuery::RegisteredEventTypes => state.registered_queries += 1,
        }
        match state.action.as_mut() {
            Some(action) => action(query),
            None => Ok(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HookState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether an action is armed and the query counts, never the action. It shows
/// no fields while the hook's lock is held (as it is while an armed action runs)
/// instead of waiting on it.
impl std::fmt::Debug for SpecQueryHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return f.debug_struct("SpecQueryHook").finish_non_exhaustive();
            }
        };
        f.debug_struct("SpecQueryHook")
            .field("armed", &state.action.is_some())
            .field("event_type_queries", &state.event_type_queries)
            .field("candidate_queries", &state.candidate_queries)
            .field("registered_queries", &state.registered_queries)
            .finish()
    }
}

#[async_trait]
impl SpecificationManager for HookedSpecificationManager {
    async fn get_topic(&self, id: &GtsInstanceId) -> Result<Option<Topic>, DomainError> {
        self.inner.get_topic(id).await
    }

    async fn get_event_type(&self, id: &GtsTypeId) -> Result<Option<EventType>, DomainError> {
        self.hook.before(SpecQuery::EventType)?;
        self.inner.get_event_type(id).await
    }

    async fn get_event_type_candidates(
        &self,
        topic: &GtsInstanceId,
    ) -> Result<EventTypeCandidates, DomainError> {
        self.hook.before(SpecQuery::EventTypeCandidates)?;
        self.inner.get_event_type_candidates(topic).await
    }

    async fn get_registered_event_types(
        &self,
        topic: &GtsInstanceId,
        ids: &[GtsTypeId],
    ) -> Result<Vec<GtsTypeId>, DomainError> {
        self.hook.before(SpecQuery::RegisteredEventTypes)?;
        self.inner.get_registered_event_types(topic, ids).await
    }

    async fn validate_event_data(
        &self,
        event_type: &EventType,
        data: &JsonValue,
    ) -> Result<(), DomainError> {
        self.inner.validate_event_data(event_type, data).await
    }

    async fn list_topics(&self) -> Result<Vec<Topic>, DomainError> {
        self.inner.list_topics().await
    }

    async fn list_event_types(&self) -> Result<Vec<EventType>, DomainError> {
        self.inner.list_event_types().await
    }

    async fn resolve_topic_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError> {
        self.inner.resolve_topic_id(id).await
    }

    async fn resolve_event_type_id(&self, id: &GtsTypeId) -> Result<i64, DomainError> {
        self.inner.resolve_event_type_id(id).await
    }

    async fn topic_state(&self, topic: &Topic) -> Result<Option<TopicState>, DomainError> {
        self.inner.topic_state(topic).await
    }

    async fn provision_topics(
        &self,
        backends: &dyn BackendResolver,
        limits: ProvisioningLimits,
    ) -> usize {
        self.inner.provision_topics(backends, limits).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_debug_shows_its_counts() {
        let hook = SpecQueryHook::new();
        hook.arm(|_| Ok(()));

        hook.before(SpecQuery::RegisteredEventTypes)
            .expect("the armed action lets the query through");

        assert_eq!(
            format!("{hook:?}"),
            "SpecQueryHook { armed: true, event_type_queries: 0, candidate_queries: 0, \
             registered_queries: 1 }"
        );
    }
}

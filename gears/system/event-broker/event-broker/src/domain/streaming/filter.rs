//! Whether an event belongs to a subscription.
//!
//! The subscription predicates: the event's topic equals the interest's topic,
//! its type matches one of the interest's GTS patterns, and its tenant is within
//! the interest's scope. An interest may also carry a `FilterSpec`, but the
//! expression language, the engine contract and the evaluation semantics belong
//! to ADR-0005; this module owns only *where* such an engine is invoked.
//!
//! Compiled once at stream open. Type patterns are already validated
//! `GtsIdPattern`s (the wildcard grammar is enforced when a pattern is
//! constructed), so a malformed pattern never reaches a subscription.
//! Production delivery additionally checks consume permission on each concrete
//! event type; selector matching alone is not an authorization decision.

use std::collections::HashMap;

use async_trait::async_trait;
use authz_resolver_sdk::{AccessRequest, EnforcerError, PolicyEnforcer};
use toolkit::domain_model;
use toolkit_security::SecurityContext;

use gts::{GtsId, GtsIdPattern};
use uuid::Uuid;

use crate::domain::authz::event_type_resource;
use crate::domain::error::DomainError;
use crate::domain::model::{Event, FilterSpec, Interest};

/// Whether one event is wanted.
#[async_trait]
pub trait EventFilter: Send + Sync {
    fn matches(&self, event: &Event) -> bool;

    /// Applies the filter, including asynchronous authorization when configured.
    ///
    /// # Errors
    /// Returns an authorization evaluation error if permission cannot be
    /// established. Delivery must terminate rather than skip unread events.
    async fn allows(&self, event: &Event) -> Result<bool, DomainError> {
        Ok(self.matches(event))
    }
}

/// Combines subscription matching with a point decision on the delivered type.
/// A bare GTS selector includes descendants, but a grant on that selector's
/// root need not include them. Never infer a descendant's permission from JOIN.
#[domain_model]
pub struct AuthorizedInterestFilter {
    interests: InterestFilter,
    policy_enforcer: PolicyEnforcer,
    ctx: SecurityContext,
}

impl AuthorizedInterestFilter {
    /// Captures the stream caller so each matching event is checked as that
    /// principal. Decisions are not cached across events: revocation and deny
    /// rules must remain effective for an already-open stream.
    #[must_use]
    pub fn new(
        interests: &[Interest],
        policy_enforcer: PolicyEnforcer,
        ctx: SecurityContext,
    ) -> Self {
        Self {
            interests: InterestFilter::compile(interests),
            policy_enforcer,
            ctx,
        }
    }
}

#[async_trait]
impl EventFilter for AuthorizedInterestFilter {
    fn matches(&self, event: &Event) -> bool {
        self.interests.matches(event)
    }

    async fn allows(&self, event: &Event) -> Result<bool, DomainError> {
        if !self.matches(event) {
            return Ok(false);
        }
        let resource = event_type_resource(&event.r#type)?;
        match self
            .policy_enforcer
            .access_scope_with(
                &self.ctx,
                &resource,
                "consume",
                None,
                &AccessRequest::new()
                    .resource_property("event_type_id", event.r#type.as_ref())
                    .require_constraints(false),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(EnforcerError::Denied { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

/// One interest, compiled.
#[derive(Debug)]
struct CompiledInterest {
    tenant_id: Uuid,
    /// Canonical GTS patterns, carried by the interest and matched via
    /// [`GtsId::matches_pattern`]. The grammar was validated when each pattern
    /// was constructed (`GtsIdPattern::try_new`), so no re-parsing happens here.
    patterns: Vec<GtsIdPattern>,
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
                .push(CompiledInterest {
                    tenant_id: interest.tenant_id,
                    patterns: interest.types.clone(),
                    filter: interest.filter.clone(),
                });
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

        candidates.iter().any(|interest| {
            interest.tenant_id == event.tenant_id
                && interest
                    .patterns
                    .iter()
                    .any(|pattern| event_type.matches_pattern(pattern))
        })
    }
}

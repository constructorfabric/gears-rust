//! The consume decisions a JOIN asks the PDP for.
//!
//! Interests on the same topic and tenant context form one group, and each
//! group costs at most three PDP calls, whatever its number of interests and
//! type patterns:
//! - I, the barrier right (`consume_ignore_barrier` on the topic), only for
//!   `ignore`, decision only;
//! - E, `consume` on the topic, naming the group's patterns, its candidate
//!   event types and their subject types, whose scope is kept;
//! - R, the tenant check on the request resource, only when E fails, to tell
//!   a tenant refusal from a topic refusal.
//!
//! The tenant context follows the interest's ownership filter: `only_mine`
//! asks root-only at the caller's own tenant, always respecting barriers;
//! `entire_tenant` asks root-only at the interest tenant; and
//! `tenant_subtree_included` asks for the subtree under it. I is always
//! root-only, so the PDP checks the barrier right with one ancestry lookup.
//!
//! The groups also tell the JOIN response which interest entries match no
//! event type registered on their topic.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::{
    AccessRequest, EnforcerError, EvaluationRequest, PolicyEnforcer, ResourceType, TenantMode,
};
use gts::{GtsId, GtsIdPattern, GtsIdPatternSegment, GtsTypeId};
use serde_json::Value;
use tokio::time::Instant;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::authz::{
    TENANT_SCOPE_RESOURCE, check_event_type_selector, topic_consume_resource, topic_resource,
};
use crate::domain::consumer_group_coordinator::member_scope::AuthorizedGroup;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{BarrierMode, Interest, OwnershipFilter};
use crate::domain::specification::{EventTypeCandidate, EventTypeCandidates};
use crate::domain::streaming::scope::{AdmittedSubjectTypes, CompiledScope, EVENT_TYPES};
use crate::domain::streaming::scope_memo::{MemoKey, MemoResult, ScopeMemo};

/// The request property naming the group's event-type patterns.
const EVENT_TYPE_PATTERNS: &str = "event_type_patterns";

/// The request property listing the subject types the candidates allow.
const EVENT_SUBJECT_TYPE_PATTERNS: &str = "event_subject_type_patterns";

/// The request property naming the topic.
pub(crate) const TOPIC_ID: &str = "topic_id";

/// The most subject-type patterns E carries; a larger union is not sent.
pub const MAX_SUBJECT_TYPE_PATTERNS: usize = 256;

/// What makes interests share one consume decision: the same topic and
/// tenant context.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupKey {
    /// The topic the interests read.
    pub topic: GtsInstanceId,
    /// The root of the tenant context every call names.
    pub tenant: Uuid,
    /// Whether E and R ask for the root alone or for its subtree.
    pub mode: TenantMode,
    /// Whether the interests read across self-managed tenant barriers.
    pub barrier_mode: BarrierMode,
}

/// Interests that share one consume decision: the same [`GroupKey`].
#[domain_model]
#[derive(Debug, Clone)]
pub struct InterestGroup {
    key: GroupKey,
    interests: Vec<Interest>,
    /// The position in the request of each of `interests`, in the same order.
    positions: Vec<usize>,
}

/// A group with the event types its topic has registered.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConsumeGroup {
    group: InterestGroup,
    /// Shared, so a stream's refresh worker evaluates each renewal with the
    /// list it holds without copying it.
    candidates: Arc<EventTypeCandidates>,
}

/// One PDP call: the resource, the action and the request, with the
/// `require_constraints` flag the request carries, so the request the PDP
/// receives can be rebuilt for the memo key.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConsumeCall {
    resource: ResourceType,
    action: &'static str,
    require_constraints: bool,
    request: AccessRequest,
}

/// The consume answers JOIN, stream open and a stream's scope refresh got, in
/// the shared memo, each for the configured TTL. Stream open and the refresh
/// read them back before they ask.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConsumeMemo {
    memo: Arc<ScopeMemo>,
    ttl: Duration,
}

impl GroupKey {
    /// The tenant context `interest` is decided in for a caller of
    /// `caller_tenant`. `only_mine` reads the caller's own tenant, root-only
    /// and respecting barriers, whatever the interest's tenant and barrier
    /// mode say.
    #[must_use]
    pub fn new(interest: &Interest, caller_tenant: Uuid) -> Self {
        let topic = interest.topic.clone();
        match interest.ownership {
            OwnershipFilter::OnlyMine => Self {
                topic,
                tenant: caller_tenant,
                mode: TenantMode::RootOnly,
                barrier_mode: BarrierMode::Respect,
            },
            OwnershipFilter::EntireTenant => Self {
                topic,
                tenant: interest.subject_tenant,
                mode: TenantMode::RootOnly,
                barrier_mode: interest.barrier_mode,
            },
            OwnershipFilter::TenantSubtreeIncluded => Self {
                topic,
                tenant: interest.subject_tenant,
                mode: TenantMode::Subtree,
                barrier_mode: interest.barrier_mode,
            },
        }
    }

    /// The tenant context, in `mode` and `barrier_mode`, and the tenant
    /// property a call of the group sends.
    fn reach(&self, mode: TenantMode, barrier_mode: BarrierMode) -> AccessRequest {
        AccessRequest::new()
            .context_tenant_id(self.tenant)
            .tenant_mode(mode)
            .barrier_mode(barrier_mode.into())
            .resource_property(pep_properties::OWNER_TENANT_ID, self.tenant)
    }

    /// What every consume scope question of the group sends, the group's E
    /// and a single-type E alike: the group's tenant context, the topic and
    /// the group's event-type `patterns`.
    fn consume_request(&self, patterns: BTreeSet<&str>) -> AccessRequest {
        self.reach(self.mode.clone(), self.barrier_mode)
            .resource_property(TOPIC_ID, self.topic.as_ref())
            .resource_property(EVENT_TYPE_PATTERNS, ConsumeGroup::strings(patterns))
    }
}

impl InterestGroup {
    /// Groups `interests` by topic and tenant context for a caller of
    /// `caller_tenant`, in the order each group's first interest appears, so
    /// the calls go out in a fixed order.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::BadTypePattern`] for the
    /// first type pattern, in interest order, that cannot match any event
    /// type; [`DomainError::Internal`] if the event family pattern cannot be
    /// built.
    pub fn form(interests: &[Interest], caller_tenant: Uuid) -> Result<Vec<Self>, DomainError> {
        let mut groups: Vec<Self> = Vec::new();
        for (position, interest) in interests.iter().enumerate() {
            for pattern in &interest.types {
                check_event_type_selector(pattern)?;
            }
            let key = GroupKey::new(interest, caller_tenant);
            match groups.iter_mut().find(|group| group.key == key) {
                Some(group) => {
                    group.interests.push(interest.clone());
                    group.positions.push(position);
                }
                None => groups.push(Self {
                    key,
                    interests: vec![interest.clone()],
                    positions: vec![position],
                }),
            }
        }
        Ok(groups)
    }

    /// The topic the group's interests read.
    #[must_use]
    pub fn topic(&self) -> &GtsInstanceId {
        &self.key.topic
    }

    /// The group with its topic's registered event types.
    #[must_use]
    pub fn with_candidates(self, candidates: impl Into<Arc<EventTypeCandidates>>) -> ConsumeGroup {
        ConsumeGroup {
            group: self,
            candidates: candidates.into(),
        }
    }
}

impl ConsumeGroup {
    /// The topic the group's interests read.
    #[must_use]
    pub fn topic(&self) -> &GtsInstanceId {
        &self.group.key.topic
    }

    /// Whether E and R ask for the root alone or for its subtree, which is
    /// also how a kept scope compiles.
    #[must_use]
    pub fn tenant_mode(&self) -> &TenantMode {
        &self.group.key.mode
    }

    /// Whether the group asks for the barrier right before E.
    #[must_use]
    pub fn crosses_barriers(&self) -> bool {
        self.group.key.barrier_mode == BarrierMode::Ignore
    }

    /// I: may the caller read across a barrier on this topic. Decision only.
    #[must_use]
    pub fn i_call(&self) -> ConsumeCall {
        ConsumeCall::new(
            topic_resource(&self.group.key.topic),
            "consume_ignore_barrier",
            false,
            self.group
                .key
                .reach(TenantMode::RootOnly, BarrierMode::Ignore)
                .resource_property(TOPIC_ID, self.group.key.topic.as_ref()),
        )
    }

    /// E: what the caller may consume on this topic in the group's tenant
    /// context.
    /// Every list is sorted and deduplicated, so equal groups send equal
    /// requests.
    #[must_use]
    pub fn e_call(&self) -> ConsumeCall {
        let patterns: BTreeSet<&str> = self
            .group
            .interests
            .iter()
            .flat_map(|interest| &interest.types)
            .map(GtsIdPattern::pattern)
            .collect();
        let mut request = self.group.key.consume_request(patterns);
        if let Some(matched) = self.matched() {
            let types: BTreeSet<&str> = matched.iter().map(|(_, c)| c.id.as_ref()).collect();
            let subject_types: BTreeSet<&str> = matched
                .iter()
                .flat_map(|(_, c)| &c.allowed_subject_types)
                .map(GtsIdPattern::pattern)
                .collect();
            request = request.resource_property(EVENT_TYPES, Self::strings(types));
            if subject_types.len() <= MAX_SUBJECT_TYPE_PATTERNS {
                request = request
                    .resource_property(EVENT_SUBJECT_TYPE_PATTERNS, Self::strings(subject_types));
            }
        }
        ConsumeCall::new(
            topic_consume_resource(&self.group.key.topic),
            "consume",
            true,
            request,
        )
    }

    /// R: does the caller reach the group's tenant at all. Sent only after E
    /// fails, with E's tenant context, to pick the refusal's code.
    #[must_use]
    pub fn r_call(&self) -> ConsumeCall {
        ConsumeCall::new(
            TENANT_SCOPE_RESOURCE,
            "consume",
            true,
            self.group
                .key
                .reach(self.group.key.mode.clone(), self.group.key.barrier_mode),
        )
    }

    /// Refuses an exact selector the kept scope can never deliver.
    ///
    /// An exact type pattern is refused when it matches some candidates and
    /// the scope admits none of them.
    ///
    /// An exact subject type S (a type id) is checked when it overlaps some
    /// pattern P that a matched candidate allows: P covers S, or S covers P.
    /// It is refused when no kept constraint admits it, where a constraint
    /// admits S when it has no subject-type filter, or when some type T is
    /// named by every subject-type filter of the constraint, lies under S,
    /// and matches some allowed P (see
    /// [`CompiledScope::admitted_subject_types`]). Only such a T can be both
    /// published and delivered.
    ///
    /// Wildcards, a selector with no candidate, and a topic over the
    /// candidate cap are not checked: the broker cannot tell what they will
    /// match. A subject type that overlaps nothing the candidates allow, where
    /// neither covers the other (including one that shares only some types
    /// with an allowed pattern, for example through a minor version), is not
    /// checked either. The check grants nothing, so skipping it never widens
    /// delivery.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] with [`ErrorCode::EventTypeNotAuthorized`]
    /// or [`ErrorCode::SubjectTypeNotAuthorized`], naming the selector.
    pub fn post_check(&self, scope: &CompiledScope) -> Result<(), DomainError> {
        let Some(matched) = self.matched() else {
            return Ok(());
        };
        let allowed: BTreeMap<&str, &GtsIdPattern> = matched
            .iter()
            .flat_map(|(_, c)| &c.allowed_subject_types)
            .map(|pattern| (pattern.pattern(), pattern))
            .collect();
        let allowed: Vec<&GtsIdPattern> = allowed.into_values().collect();
        // Built on the first checked subject type, then shared by the rest.
        let mut admitted: Option<AdmittedSubjectTypes> = None;
        for interest in &self.group.interests {
            for pattern in interest.types.iter().filter(|p| Self::is_exact(p)) {
                let mut of_pattern = matched
                    .iter()
                    .filter(|(id, _)| id.matches_pattern(pattern))
                    .peekable();
                if of_pattern.peek().is_some()
                    && !of_pattern.any(|(_, c)| scope.admits_event_type(&c.id))
                {
                    return Err(Self::refusal(
                        ErrorCode::EventTypeNotAuthorized,
                        "event type",
                        pattern,
                    ));
                }
            }
            for pattern in &interest.subject_types {
                // Only an exact subject type is a type id: a wildcard, or an
                // exact pattern that names no type, is not checked.
                if !(Self::is_exact(pattern)
                    && pattern
                        .segments()
                        .last()
                        .is_some_and(GtsIdPatternSegment::is_type))
                {
                    continue;
                }
                if Self::overlaps(pattern, &allowed)
                    && !admitted
                        .get_or_insert_with(|| scope.admitted_subject_types(&allowed))
                        .admits_under(pattern)
                {
                    return Err(Self::refusal(
                        ErrorCode::SubjectTypeNotAuthorized,
                        "subject type",
                        pattern,
                    ));
                }
            }
        }
        Ok(())
    }

    /// The type ids the group's interests name, sorted and deduplicated, for
    /// the spec cache to say which are registered when the topic is over the
    /// candidate cap. Empty when the candidates are listed, since they answer
    /// already. A wildcard, or an exact pattern that names an instance, is not
    /// a type id and is left out: no type is registered under it.
    #[must_use]
    pub fn unlisted_exact_type_ids(&self) -> Vec<GtsTypeId> {
        let EventTypeCandidates::OverCap = *self.candidates else {
            return Vec::new();
        };
        let ids: BTreeSet<&str> = self
            .group
            .interests
            .iter()
            .flat_map(|interest| &interest.types)
            .map(GtsIdPattern::pattern)
            .collect();
        ids.into_iter()
            .filter_map(|id| GtsTypeId::try_new(id).ok())
            .collect()
    }

    /// The entries of each interest's `types` that match no event type
    /// registered on the topic, keyed by the interest's position in the
    /// request; an interest with none is left out.
    ///
    /// With the candidates listed, an entry is unmatched when no candidate
    /// matches it as delivery would, so a base type whose derived types are
    /// registered matches. Over the cap only exact entries are checked, each
    /// against `registered`, what the spec cache returned for
    /// [`Self::unlisted_exact_type_ids`]: that lookup is by id, so the same
    /// base type is reported there. A wildcard is never reported over the cap.
    #[must_use]
    pub fn unmatched_types(&self, registered: &[GtsTypeId]) -> Vec<(usize, Vec<GtsIdPattern>)> {
        let registered: BTreeSet<&str> = registered.iter().map(AsRef::as_ref).collect();
        let listed: Option<Vec<GtsId>> = match &*self.candidates {
            EventTypeCandidates::Listed(candidates) => Some(
                candidates
                    .iter()
                    .filter_map(|c| GtsId::try_new(c.id.as_ref()).ok())
                    .collect(),
            ),
            EventTypeCandidates::OverCap => None,
        };
        let unmatched = |pattern: &GtsIdPattern| match &listed {
            Some(ids) => !ids.iter().any(|id| id.matches_pattern(pattern)),
            None => Self::is_exact(pattern) && !registered.contains(pattern.pattern()),
        };
        self.group
            .positions
            .iter()
            .zip(&self.group.interests)
            .filter_map(|(position, interest)| {
                let types: Vec<GtsIdPattern> = interest
                    .types
                    .iter()
                    .filter(|pattern| unmatched(pattern))
                    .cloned()
                    .collect();
                (!types.is_empty()).then_some((*position, types))
            })
            .collect()
    }

    /// The group's entry of a member scope: `scope`, serving until
    /// `expires_at`, with the candidates the group's patterns match, none
    /// when the topic is over the candidate cap.
    #[must_use]
    pub fn authorized(&self, scope: Arc<CompiledScope>, expires_at: Instant) -> AuthorizedGroup {
        let candidates: HashSet<GtsTypeId> = self
            .matched()
            .map(|matched| matched.into_iter().map(|(_, c)| c.id.clone()).collect())
            .unwrap_or_default();
        AuthorizedGroup {
            key: self.group.key.clone(),
            scope,
            candidates: Arc::new(candidates),
            candidates_stale: false,
            expires_at,
        }
    }

    /// The candidates that some pattern of the group matches, each with its id
    /// parsed once, in list order; `None` when the topic is over the candidate
    /// cap and its types are not listed. Matched as delivery matches an
    /// event's type: a candidate whose id does not parse matches nothing.
    fn matched(&self) -> Option<Vec<(GtsId, &EventTypeCandidate)>> {
        let EventTypeCandidates::Listed(candidates) = &*self.candidates else {
            return None;
        };
        let patterns: Vec<&GtsIdPattern> = self
            .group
            .interests
            .iter()
            .flat_map(|interest| &interest.types)
            .collect();
        Some(
            candidates
                .iter()
                .filter_map(|c| {
                    let id = GtsId::try_new(c.id.as_ref()).ok()?;
                    patterns
                        .iter()
                        .any(|pattern| id.matches_pattern(pattern))
                        .then_some((id, c))
                })
                .collect(),
        )
    }

    /// Whether the subject-type `selector` overlaps a pattern the candidates
    /// allow: the pattern covers the selector, or the selector covers the
    /// pattern, an exact type or a wildcard narrower than itself.
    fn overlaps(selector: &GtsIdPattern, allowed: &[&GtsIdPattern]) -> bool {
        allowed
            .iter()
            .any(|pattern| pattern.covers(selector) || selector.covers(pattern))
    }

    fn is_exact(pattern: &GtsIdPattern) -> bool {
        !pattern
            .segments()
            .iter()
            .any(GtsIdPatternSegment::is_wildcard)
    }

    fn strings(values: BTreeSet<&str>) -> Value {
        Value::from(values.into_iter().collect::<Vec<_>>())
    }

    fn refusal(code: ErrorCode, what: &str, pattern: &GtsIdPattern) -> DomainError {
        DomainError::Forbidden {
            code,
            message: format!(
                "calling principal lacks consume on {what} '{}'",
                pattern.pattern()
            ),
            resource: pattern.pattern().to_owned(),
        }
    }
}

impl ConsumeCall {
    /// E for one event type outside the group's candidates, as delivery asks
    /// it: the group's E with `event_types` naming `event_type` alone, and
    /// `event_subject_type_patterns` naming `subject_types`, the subject
    /// types that type allows, sent only when there are some and no more
    /// than [`MAX_SUBJECT_TYPE_PATTERNS`]. `patterns` are the group's
    /// event-type patterns.
    #[must_use]
    pub fn single_type(
        key: &GroupKey,
        patterns: BTreeSet<&str>,
        event_type: &GtsTypeId,
        subject_types: BTreeSet<&str>,
    ) -> Self {
        let mut request = key.consume_request(patterns).resource_property(
            EVENT_TYPES,
            ConsumeGroup::strings(BTreeSet::from([event_type.as_ref()])),
        );
        if !subject_types.is_empty() && subject_types.len() <= MAX_SUBJECT_TYPE_PATTERNS {
            request = request.resource_property(
                EVENT_SUBJECT_TYPE_PATTERNS,
                ConsumeGroup::strings(subject_types),
            );
        }
        Self::new(topic_consume_resource(&key.topic), "consume", true, request)
    }

    fn new(
        resource: ResourceType,
        action: &'static str,
        require_constraints: bool,
        request: AccessRequest,
    ) -> Self {
        Self {
            resource,
            action,
            require_constraints,
            request: request.require_constraints(require_constraints),
        }
    }

    /// Asks the PDP.
    ///
    /// # Errors
    ///
    /// The enforcer's error: a denial, an answer that does not compile, or a
    /// failed evaluation.
    pub async fn send(
        &self,
        ctx: &SecurityContext,
        enforcer: &PolicyEnforcer,
    ) -> Result<AccessScope, EnforcerError> {
        enforcer
            .access_scope_with(ctx, &self.resource, self.action, None, &self.request)
            .await
    }

    /// The request [`Self::send`] sends, as the PDP receives it.
    #[must_use]
    pub fn evaluation_request(
        &self,
        ctx: &SecurityContext,
        enforcer: &PolicyEnforcer,
    ) -> EvaluationRequest {
        enforcer.build_request_with(
            ctx,
            &self.resource,
            self.action,
            None,
            self.require_constraints,
            &self.request,
        )
    }
}

impl ConsumeMemo {
    /// Writes into `memo`, each entry for `ttl`.
    #[must_use]
    pub fn new(memo: Arc<ScopeMemo>, ttl: Duration) -> Self {
        Self { memo, ttl }
    }

    /// How long each answer is kept.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The answer stored for `request` and the instant it stops serving,
    /// while it is live at `now`.
    #[must_use]
    pub fn recall(
        &self,
        request: &EvaluationRequest,
        now: Instant,
    ) -> Option<(MemoResult, Instant)> {
        match MemoKey::try_from(request) {
            Ok(key) => self.memo.get_entry(&key, now),
            Err(err) => {
                tracing::warn!(%err, "a consume answer is not looked up in the memo");
                None
            }
        }
    }

    /// Stores `result` for `request` until `now + ttl`.
    ///
    /// A memo that is full stores nothing: the next stream open or refresh
    /// asks the PDP again.
    pub fn write(&self, request: &EvaluationRequest, result: MemoResult, now: Instant) {
        match MemoKey::try_from(request) {
            Ok(key) => {
                let _stored_or_full = self.memo.insert(key, result, now + self.ttl, now);
            }
            Err(err) => tracing::warn!(%err, "a consume answer is not memoized"),
        }
    }
}

#[cfg(test)]
#[path = "join_scope_tests.rs"]
mod join_scope_tests;

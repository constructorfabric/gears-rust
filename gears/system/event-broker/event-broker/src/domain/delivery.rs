//! `DeliveryService` (`docs/DESIGN.md` "Internal Services"): consumer-facing,
//! owns the read path and the subscription lifecycle.
//!
//! A stream opens with a `topology` frame and then carries `event` and
//! `heartbeat` frames; an open is refused with `StreamingInProgress` while
//! another stream holds the subscription, and with `PositionsNotSet` until
//! its positions are seeded. The consumer-group coordinator republishes a
//! member's assignment on every rebalance that moves its partitions
//! (`publish_generations`), and the stream session applies it (`apply`): a
//! loss narrows the stream with a `topology` frame, and a gain, the loss of
//! every partition, or a LEAVE ends it with a terminal `control` frame. A
//! stream open for a subscription the broker has ended answers `410
//! SubscriptionTerminated` (not yet answered, tracked in #5977; a
//! reaped or left member answers `404 SubscriptionNotFound`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AccessDenial, AccessRequest, BarrierMode, EnforcerError, EvaluationRequest, PolicyEnforcer,
    ResourceType, TenantMode,
};
use chrono::Utc;
use gts::GtsIdPattern;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{SecurityContext, pep_properties};
use toolkit_taskward::WorkerBuilder;
use uuid::Uuid;

use crate::domain::authz::{
    CONSUMER_GROUP_FAMILY_RESOURCE, consumer_group_resource, require_caller_identity,
    tenant_authorized, with_forbidden_code,
};
use crate::domain::backend::BackendResolver;
use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopePrincipal, ScopeWrite,
    ScopeWriteOutcome,
};
use crate::domain::consumer_group_coordinator::{ConsumerGroupCoordinator, TopicInterest};
pub use event_broker_sdk::api::Position;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::join_scope::{ConsumeCall, ConsumeGroup, ConsumeMemo, InterestGroup};
use crate::domain::model::{
    ConsumerGroup, ConsumerGroupKind, Interest, Joiner, OwnershipFilter, Sequence, Subscription,
};
use crate::domain::specification::EventTypeCandidates;
use crate::domain::streaming::assignment::Generation;
use crate::domain::streaming::filter::EventFilter;
use crate::domain::streaming::lease::StreamLeases;
use crate::domain::streaming::progress::ProgressConfig;
use crate::domain::streaming::read::{MaxBytes, MaxEvents, ReadLimit};
use crate::domain::streaming::read_set::ReadSet;
use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::domain::streaming::scope_memo::{MemoResult, ScopeMemo};
use crate::domain::streaming::scope_refresh::ScopeRefresh;
use crate::domain::streaming::scoped_filter::{ScopedFilter, SingleTypeAuthorizer};
use crate::domain::streaming::session::{FrameStream, ScopeFeed, SessionOpening, StreamSession};

/// JOIN request body (`DESIGN.md` "Subscription Lifecycle Endpoints
/// (Delivery)": `consumer_group` + `interests[]`). `session_timeout` is always
/// concrete here - the REST handler and the in-process broker resolve the
/// documented `PT30S` default before constructing this.
#[domain_model]
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub consumer_group: GtsInstanceId,
    pub client_agent: String,
    pub interests: Vec<Interest>,
    pub session_timeout: Duration,
}

impl JoinRequest {
    /// The most interests one JOIN may declare.
    pub const MAX_INTERESTS: usize = 64;
    /// The most event-type selectors one interest may declare.
    pub const MAX_TYPES_PER_INTEREST: usize = 32;
    /// The most subject-type selectors one interest may declare.
    pub const MAX_SUBJECT_TYPES_PER_INTEREST: usize = 32;

    /// Refuses a body over the interest limits, with a nil interest tenant, or
    /// with an `only_mine` interest on a tenant other than `caller_tenant`.
    /// The interest count is checked first, then each interest in order: its
    /// type count, its subject-type count, then its tenant. Messages name an
    /// interest by its index.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::TooManyInterests`] for more
    /// than [`Self::MAX_INTERESTS`] interests, [`ErrorCode::TooManyTypes`] for
    /// an interest with more than [`Self::MAX_TYPES_PER_INTEREST`] types,
    /// [`ErrorCode::TooManySubjectTypes`] for an interest with more than
    /// [`Self::MAX_SUBJECT_TYPES_PER_INTEREST`] subject types, and
    /// [`ErrorCode::InvalidTenant`] for an interest whose tenant is the nil
    /// UUID, or an `only_mine` interest whose tenant is not `caller_tenant`.
    pub fn check_interests(&self, caller_tenant: Uuid) -> Result<(), DomainError> {
        let count = self.interests.len();
        if count > Self::MAX_INTERESTS {
            return Err(DomainError::Validation {
                code: ErrorCode::TooManyInterests,
                message: format!(
                    "interests count {count} exceeds the maximum of {}",
                    Self::MAX_INTERESTS
                ),
            });
        }
        for (index, interest) in self.interests.iter().enumerate() {
            let types = interest.types.len();
            if types > Self::MAX_TYPES_PER_INTEREST {
                return Err(DomainError::Validation {
                    code: ErrorCode::TooManyTypes,
                    message: format!(
                        "interest {index}: types count {types} exceeds the maximum of {}",
                        Self::MAX_TYPES_PER_INTEREST
                    ),
                });
            }
            let subject_types = interest.subject_types.len();
            if subject_types > Self::MAX_SUBJECT_TYPES_PER_INTEREST {
                return Err(DomainError::Validation {
                    code: ErrorCode::TooManySubjectTypes,
                    message: format!(
                        "interest {index}: subject types count {subject_types} exceeds the maximum of {}",
                        Self::MAX_SUBJECT_TYPES_PER_INTEREST
                    ),
                });
            }
            if interest.subject_tenant.is_nil() {
                return Err(DomainError::Validation {
                    code: ErrorCode::InvalidTenant,
                    message: format!(
                        "interest {index}: the interest's tenant must not be the nil UUID"
                    ),
                });
            }
            if interest.ownership == OwnershipFilter::OnlyMine
                && interest.subject_tenant != caller_tenant
            {
                return Err(DomainError::Validation {
                    code: ErrorCode::InvalidTenant,
                    message: format!("interest {index}: only_mine reads the caller's own tenant"),
                });
            }
        }
        Ok(())
    }

    /// Refuses a zero `session_timeout`: the documented contract takes any
    /// positive duration, a sub-second one included.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::InvalidSessionTimeout`]
    /// when `session_timeout` is zero.
    pub fn check_session_timeout(&self) -> Result<(), DomainError> {
        if self.session_timeout.is_zero() {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidSessionTimeout,
                message: "session_timeout must be a positive duration".to_owned(),
            });
        }
        Ok(())
    }
}

/// A JOIN's answer: the subscription it created, and the entries of each
/// interest's `types` that matched no event type registered on its topic.
/// The report is for the JOIN response only: the subscription is stored
/// without it, and a later read does not recompute it.
#[domain_model]
#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub subscription: Subscription,
    /// `(position of the interest in the request, its unmatched entries)`, in
    /// interest order; an interest whose entries all matched is left out.
    pub unmatched_types: Vec<(usize, Vec<GtsIdPattern>)>,
}

/// A SEEK request value (`event-broker-seek-endpoint-shape`): an exact
/// cursor, or a sentinel the broker resolves against the partition's retained
/// events.
/// One SEEK request target. A subscription can span multiple topics, and
/// assignments/cursors are identified by the full `(topic, partition)`
/// pair (`DESIGN.md` "Subscription Schema", `assigned`) - keying by `partition` alone would collapse
/// partition `0` of two different topics into one entry.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SeekTarget {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub value: Position,
}

/// One resolved SEEK result - sentinels are already resolved to a concrete
/// cursor (`event-broker-seek-endpoint-shape`'s response requirement).
#[domain_model]
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub offset: Sequence,
}

/// `control` frame fact (`event-broker-consumption-frames`) - the reason
/// (`rebalanced`/`lose_all`/`teardown`) rides in `Frame::Control::reason`,
/// not the code, so recovery logic switches on the fact.
// `Frame`, `ControlCode`, `Position` and `CloseReason` live in
// `domain/streaming/frames.rs` - the session is their sole constructor, so they
// belong with it rather than with this service.
pub use crate::domain::streaming::frames::{ControlCode, Frame};

/// Stream exclusion lives on `domain::streaming::lease::StreamLeases`, not
/// here. The lease is a field of the session the returned stream owns, so
/// dropping the stream releases it by construction - including before the
/// stream is ever polled, which is what `event-broker-stream-lifecycle`
/// requires.

#[async_trait]
pub trait DeliveryService: Send + Sync {
    /// Creates a subscription as a member of its group's coordinator, in this
    /// instance's memory, claims/joins the group (`docs/DESIGN.md`
    /// "Subscription Lifecycle"), and reports the interest entries that match no
    /// registered event type. Only the principal that created an anonymous
    /// group, in its tenant, may JOIN it, and the policy must allow `consume`
    /// on the group.
    async fn join(
        &self,
        ctx: &SecurityContext,
        request: JoinRequest,
    ) -> Result<JoinOutcome, DomainError>;

    /// Removes the subscription from its group (`DESIGN.md` "Subscription Lifecycle Endpoints
    /// (Delivery)"). Only its
    /// joiner may. No rebalance signal to other members this pass - see
    /// module doc comment.
    async fn leave(&self, ctx: &SecurityContext, subscription_id: Uuid) -> Result<(), DomainError>;

    /// Unfiltered/unpaginated - the REST handler applies `$filter`, then pages
    /// the result with the domain pager (`domain/paging.rs`).
    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, DomainError>;

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<Subscription, DomainError>;

    /// Opens the consumption stream for `subscription_id`: refuses a caller
    /// with no identity, one the consume checks refuse, one that is not the
    /// subscription's joiner or one the policy refuses `consume` on the group,
    /// validates
    /// `StreamingInProgress`/`PositionsNotSet`, makes the member hold the
    /// caller's consume scope, then returns a channel whose first message is
    /// the open-time `topology` baseline frame, followed by
    /// `event`/`heartbeat` frames. Dropping the receiver releases the
    /// subscription's stream lease (`event-broker-stream-lifecycle`).
    async fn stream(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<FrameStream, DomainError>;

    /// Resolves and sets the cursor position for each target
    /// `(topic, partition)` (`DESIGN.md` "Subscription Lifecycle Endpoints (Delivery)",
    /// `event-broker-seek-endpoint-shape`). Only the subscription's joiner
    /// may seek, and the policy must allow `consume` on the group, whose shared
    /// cursors a seek writes. Pre-stream-only: rejects with
    /// `Conflict { code: "StreamingInProgress", .. }` while a stream is
    /// open, without disturbing it.
    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        topology_version: i64,
        targets: Vec<SeekTarget>,
    ) -> Result<Vec<SeekPosition>, DomainError>;

    /// Mints a fresh anonymous consumer group
    /// (`gts.cf.core.events.consumer_group.v1~<uuid>`,
    /// `docs/openapi.yaml`'s `POST /v1/consumer-groups` - "`kind` is always
    /// `anonymous` for this endpoint"; named groups would come from
    /// `types_registry` (not yet implemented, tracked in #5702), not this call).
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        input: crate::domain::model::ConsumerGroupCreateInput,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError>;

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError>;

    /// Unfiltered/unpaginated beyond tenant scoping - the REST handler
    /// applies `$filter` and `LocalBroker` its typed filter, and both page the
    /// result with the domain pager (`domain/paging.rs`), matching
    /// `list_topics`/`list_event_types`. `Anonymous` groups are filtered to
    /// `ctx`'s authorized tenant(s); a `Named` group is kept when the policy
    /// allows `consume` on the group itself. A caller whose subject or tenant
    /// is the nil UUID is refused with
    /// `Forbidden { code: "TenantIdNotAuthorized", .. }` before any read.
    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::ConsumerGroup>, DomainError>;

    /// Fails with `Conflict { code: "ConsumerGroupHasActiveMembers", .. }`
    /// if any subscription currently belongs to the group
    /// (`docs/openapi.yaml`: "Allowed only when the group has no active
    /// members"). Only the principal that created an anonymous group, in its
    /// tenant, may delete it.
    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<(), DomainError>;
}

/// How long a partition may stay unaccounted-for before the stream is closed
/// rather than heartbeating forever over data it cannot read.
///
/// A constant rather than a knob: `event-broker-stream-pipeline` calls it
/// configurable, but adding a knob is a config change and this is the value the
/// session tests already use. Promoting it belongs with the rest of group 6.
const UNANSWERABLE_TOLERANCE: Duration = Duration::from_secs(30);

/// The scope stores one stream open makes: the first, and one more after a
/// later writer superseded it. The stream lease keeps a second opener out, so
/// a store superseded twice is a writer racing the open, which the open
/// reports as a failure instead of retrying.
const SCOPE_STORE_ATTEMPTS: usize = 2;

/// Real `DeliveryService`: subscription lifecycle plus the baseline
/// (non-rebalance) consumption stream. Generic over one repo type
/// implementing every remaining trait it needs (subscriptions, cursors,
/// consumer groups - not topics or events), plus
/// `SpecificationManager`/`BackendResolver` for topic resolution and event
/// reads.
#[domain_model]
pub struct DeliveryServiceImpl<R> {
    repo: Arc<R>,
    heartbeat_interval: Duration,
    policy_enforcer: PolicyEnforcer,
    spec_manager: Arc<dyn crate::domain::specification::SpecificationManager>,
    backend_resolver: Arc<dyn BackendResolver>,
    groups: Arc<ConsumerGroupCoordinator>,
    /// Read batch bounds and progress cadence, from configuration rather than
    /// from literals at the call site.
    streaming: crate::config::StreamingConfig,
    /// Attaches a session's readers to its assigned partitions. A domain port
    /// (`ReaderAttacher`), so the domain names no infra type; infra's
    /// `TopicManager` implements it.
    attacher: Arc<dyn crate::domain::streaming::source::ReaderAttacher>,
    /// Stream exclusion: at most one open stream per subscription.
    leases: Arc<crate::domain::streaming::lease::InProcessStreamLeases>,
    /// The consume decisions of JOIN, of stream open and of each open
    /// stream's scope refresh, and the group checks of JOIN, stream open and
    /// seek, shared with the refresh workers.
    consume: Arc<ConsumeEvaluator>,
    /// Shares one allocation between equal tenant sets of compiled scopes.
    tenant_sets: Arc<TenantSetInterner>,
}

impl<R> DeliveryServiceImpl<R> {
    // Each argument is a distinctly-typed, fully-required dependency and the
    // service is constructed once (infra::wiring), so a builder or deps-struct
    // would add indirection without preventing any real mistake.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        repo: Arc<R>,
        policy_enforcer: PolicyEnforcer,
        spec_manager: Arc<dyn crate::domain::specification::SpecificationManager>,
        backend_resolver: Arc<dyn BackendResolver>,
        groups: Arc<ConsumerGroupCoordinator>,
        attacher: Arc<dyn crate::domain::streaming::source::ReaderAttacher>,
        leases: Arc<crate::domain::streaming::lease::InProcessStreamLeases>,
        scope_memo: Arc<ScopeMemo>,
        tenant_sets: Arc<TenantSetInterner>,
        streaming: crate::config::StreamingConfig,
        // The heartbeat cadence as a `Duration`, passed in rather than derived
        // from `streaming.heartbeat_interval_secs`, so a test harness can run a
        // sub-second heartbeat without `StreamingConfig` carrying a millisecond
        // field. Production passes `Duration::from_secs(...)` of that same knob.
        heartbeat_interval: Duration,
    ) -> Self {
        let memo = ConsumeMemo::new(
            scope_memo,
            Duration::from_secs(u64::from(streaming.authz_scope_ttl_secs)),
        );
        let consume = Arc::new(ConsumeEvaluator::new(
            policy_enforcer.clone(),
            Arc::clone(&spec_manager),
            memo,
            Arc::clone(&tenant_sets),
        ));
        Self {
            repo,
            heartbeat_interval,
            policy_enforcer,
            spec_manager,
            backend_resolver,
            groups,
            streaming,
            attacher,
            leases,
            consume,
            tenant_sets,
        }
    }
}

impl<R> DeliveryServiceImpl<R> {
    /// Fetches `subscription_id`, returning `SubscriptionNotFound` if
    /// absent, then verifies the calling principal is authorized to act as
    /// the subscription's `tenant_id` (`domain::authz::tenant_authorized`,
    /// which names that tenant as the request's tenant context) - a denial
    /// surfaces as `Forbidden { code: "TenantIdNotAuthorized", .. }` before
    /// the caller reads or mutates anything about the subscription, so a
    /// caller never acts on a subscription of a tenant it may not consume as.
    async fn find_authorized_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<Subscription, DomainError> {
        let subscription =
            self.groups
                .get(subscription_id)
                .ok_or_else(|| DomainError::NotFound {
                    code: ErrorCode::SubscriptionNotFound,
                    message: format!("subscription '{subscription_id}' does not exist"),
                    resource: subscription_id.to_string(),
                })?;
        tenant_authorized(
            ctx,
            &self.policy_enforcer,
            "consume",
            subscription.tenant_id,
        )
        .await?;
        Ok(subscription)
    }

    /// Makes the member hold a consume scope for `ctx` and returns it: the
    /// one it holds when that was evaluated for the same principal and none
    /// of its groups has expired, else one evaluated through the memo and
    /// stored with a compare-and-swap on the generation read, then read
    /// back. With it come the candidate lists the open evaluated each group
    /// with, in interest-group order, none when it reused the held scope.
    ///
    /// A later writer can supersede the store. The open then reads the
    /// member's scope again and uses it when it serves, or evaluates and
    /// stores once more; a second supersession fails the open.
    ///
    /// # Errors
    ///
    /// What the evaluation refuses or fails with, as JOIN maps it;
    /// [`DomainError::NotFound`] with [`ErrorCode::SubscriptionNotFound`]
    /// when the subscription is no longer a group member;
    /// [`DomainError::Internal`] when the store is superseded on every
    /// attempt.
    async fn hold_member_scope(
        &self,
        ctx: &SecurityContext,
        subscription: &Subscription,
    ) -> Result<(Arc<CachedScope>, Vec<Arc<EventTypeCandidates>>), DomainError> {
        let principal = ScopePrincipal::from(ctx);
        let mut held = self.groups.scope_of(subscription.id);
        for _ in 0..SCOPE_STORE_ATTEMPTS {
            if let Some(scope) = held
                .as_ref()
                .filter(|scope| scope.serves(&principal, tokio::time::Instant::now()))
            {
                return Ok((Arc::clone(scope), Vec::new()));
            }
            let expected = held.as_deref().map_or(Expected::Absent, |scope| {
                Expected::Generation(scope.generation)
            });
            let (groups, listed) = self.consume.evaluate_scope(ctx, subscription).await?;
            let write = ScopeWrite::Store {
                expected,
                content: ScopeContent {
                    principal: principal.clone(),
                    groups,
                },
            };
            match self.groups.write_scope(subscription.id, write) {
                ScopeWriteOutcome::Applied => {
                    let scope = self
                        .groups
                        .scope_of(subscription.id)
                        .ok_or_else(|| Self::no_longer_member(subscription.id))?;
                    return Ok((scope, listed));
                }
                ScopeWriteOutcome::MemberGone => {
                    return Err(Self::no_longer_member(subscription.id));
                }
                ScopeWriteOutcome::Superseded => held = self.groups.scope_of(subscription.id),
            }
        }
        Err(DomainError::Internal(
            "member scope changed during stream open".to_owned(),
        ))
    }

    /// The subscription left its group while the caller was acting on it.
    fn no_longer_member(subscription_id: Uuid) -> DomainError {
        DomainError::NotFound {
            code: ErrorCode::SubscriptionNotFound,
            message: format!("subscription '{subscription_id}' is no longer a group member"),
            resource: subscription_id.to_string(),
        }
    }
}

/// The consume decisions of delivery: what JOIN admits, the scope a stream
/// open stores on the member, each group a stream's refresh worker
/// re-evaluates ([`ScopeRefresh`](crate::domain::streaming::scope_refresh)),
/// and the group check of JOIN, stream open and seek. One per delivery
/// service, shared with the refresh workers, so all of them ask the same PDP
/// and read and write the same memo.
#[domain_model]
pub(crate) struct ConsumeEvaluator {
    policy_enforcer: PolicyEnforcer,
    spec_manager: Arc<dyn crate::domain::specification::SpecificationManager>,
    /// The consume answers JOIN, stream open and the refresh got, and the
    /// group checks JOIN, stream open and seek allowed, kept for
    /// `streaming.authz_scope_ttl_secs` under the exact request each one
    /// answered. JOIN writes and never reads; stream open, seek and the refresh
    /// read, and write what they had to ask. The bearer token is outside the
    /// key on purpose (#5502) while the token scopes are inside it: a caller
    /// whose scopes change is asked again, and one whose token alone changes
    /// reuses the answer until the TTL ends.
    memo: ConsumeMemo,
    /// Shares one allocation between equal tenant sets of compiled scopes.
    tenant_sets: Arc<TenantSetInterner>,
}

impl ConsumeEvaluator {
    pub(crate) fn new(
        policy_enforcer: PolicyEnforcer,
        spec_manager: Arc<dyn crate::domain::specification::SpecificationManager>,
        memo: ConsumeMemo,
        tenant_sets: Arc<TenantSetInterner>,
    ) -> Self {
        Self {
            policy_enforcer,
            spec_manager,
            memo,
            tenant_sets,
        }
    }

    /// How long a consume answer, and a group window after a refresh, serve.
    pub(crate) fn ttl(&self) -> Duration {
        self.memo.ttl()
    }

    /// The event types registered on `topic`, the candidates of its groups.
    ///
    /// # Errors
    ///
    /// What the specification manager's candidate query fails with.
    pub(crate) async fn get_candidates(
        &self,
        topic: &GtsInstanceId,
    ) -> Result<EventTypeCandidates, DomainError> {
        self.spec_manager.get_event_type_candidates(topic).await
    }

    /// Decides what the caller may consume for one group: the barrier right
    /// (I) first for an `ignore` group, then the consume scope (E), then, only
    /// when E fails, the tenant check (R) that picks the refusal's code.
    /// Every allowed I and every kept E scope is written to the memo; a
    /// refusal is not, so a grant fixed after a 403 counts on the next JOIN.
    ///
    /// A consume answer that does not compile is a refusal with the tenant or
    /// topic code (403), while publish answers 500 for one; a failed
    /// evaluation is a 500 on both paths.
    pub(crate) async fn authorize_consume(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
    ) -> Result<Arc<CompiledScope>, DomainError> {
        if group.crosses_barriers() {
            self.ask_barrier_right(ctx, group, &group.i_call()).await?;
        }
        self.decide_consume_scope(ctx, group, &group.e_call()).await
    }

    /// Evaluates every interest group of `subscription`, in interest-group
    /// order: the candidate query, then the consume answers through the memo.
    /// Group `k` stops serving at its staggered expiry, or earlier when an
    /// answer it reused from the memo expires first. Also returns the
    /// candidate list each group was evaluated with, shared, not copied.
    pub(crate) async fn evaluate_scope(
        &self,
        ctx: &SecurityContext,
        subscription: &Subscription,
    ) -> Result<(Arc<[AuthorizedGroup]>, Vec<Arc<EventTypeCandidates>>), DomainError> {
        let groups = InterestGroup::form(&subscription.interests, ctx.subject_tenant_id())?;
        let expiry = ScopeExpiry::new(tokio::time::Instant::now(), self.memo.ttl(), groups.len())?;
        let mut authorized = Vec::with_capacity(groups.len());
        let mut listed = Vec::with_capacity(groups.len());
        for (k, group) in groups.into_iter().enumerate() {
            let candidates = Arc::new(self.get_candidates(group.topic()).await?);
            let group = group.with_candidates(Arc::clone(&candidates));
            let (scope, remembered) = self.recall_consume(ctx, &group).await?;
            authorized.push(group.authorized(scope, expiry.of_group(k, remembered)?));
            listed.push(candidates);
        }
        Ok((authorized.into(), listed))
    }

    /// Re-evaluates `group` for its scope's refresh worker, as `ctx`: I for
    /// an `ignore` group, then E, each answered from a memo entry `memo_use`
    /// admits and asked otherwise, an asked answer written to the memo as at
    /// stream open. No R: R only picks the code of a refusal, and a refresh
    /// has no caller to answer.
    pub(crate) async fn renew_group(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
        memo_use: MemoUse,
    ) -> Renewal {
        let mut remembered = Remembered::No;
        if group.crosses_barriers() {
            let call = group.i_call();
            match self.recall(ctx, &call, memo_use) {
                Some((MemoResult::Decision(true), until)) => remembered = remembered.until(until),
                _ => match self.ask_barrier(ctx, &call).await {
                    Ok(()) => {}
                    Err(EnforcerError::AccessDenied(_) | EnforcerError::CompileFailed(_)) => {
                        return Renewal::Revoked;
                    }
                    // A failed evaluation, or an outcome class this build
                    // does not know: never read as a revocation.
                    Err(_) => return Renewal::Unavailable,
                },
            }
        }
        let call = group.e_call();
        let scope = match self.recall(ctx, &call, memo_use) {
            Some((MemoResult::Scope(scope), until)) => {
                remembered = remembered.until(until);
                scope
            }
            _ => match self.ask_consume_scope(ctx, group, &call).await {
                Ok(ConsumeAnswer::Kept(scope)) => scope,
                Ok(ConsumeAnswer::Refused { .. }) => return Renewal::Revoked,
                Err(_failed_evaluation) => return Renewal::Unavailable,
            },
        };
        Renewal::Renewed { scope, remembered }
    }

    /// The group check of a JOIN: asked every time, as JOIN asks its consume
    /// calls, and an allow written to the memo. A refusal is not written, so a
    /// grant fixed after a `403` counts on the next call.
    ///
    /// # Errors
    ///
    /// What [`GroupCheck::authorize`] refuses or fails with.
    pub(crate) async fn decide_group(
        &self,
        ctx: &SecurityContext,
        check: &GroupCheck,
    ) -> Result<(), DomainError> {
        check.authorize(ctx, &self.policy_enforcer).await?;
        self.memo.write(
            &check.evaluation_request(ctx, &self.policy_enforcer),
            MemoResult::Decision(true),
            tokio::time::Instant::now(),
        );
        Ok(())
    }

    /// The group check of a stream open or a seek: an allow JOIN, a seek or an
    /// earlier open left in the memo is reused, and a miss is asked as
    /// [`Self::decide_group`] asks it.
    ///
    /// # Errors
    ///
    /// What [`GroupCheck::authorize`] refuses or fails with.
    pub(crate) async fn recall_group(
        &self,
        ctx: &SecurityContext,
        check: &GroupCheck,
    ) -> Result<(), DomainError> {
        let request = check.evaluation_request(ctx, &self.policy_enforcer);
        if let Some((MemoResult::Decision(true), _)) =
            self.memo.recall(&request, tokio::time::Instant::now())
        {
            return Ok(());
        }
        self.decide_group(ctx, check).await
    }

    /// The group's consume scope at stream open: an allowed I and a kept E
    /// that JOIN or an earlier open left in the memo are reused, and only an
    /// answer the memo does not hold is asked, as [`Self::authorize_consume`]
    /// asks it. Also says until when the reused answers serve.
    async fn recall_consume(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
    ) -> Result<(Arc<CompiledScope>, Remembered), DomainError> {
        let mut remembered = Remembered::No;
        if group.crosses_barriers() {
            let call = group.i_call();
            match self.recall(ctx, &call, MemoUse::Live) {
                Some((MemoResult::Decision(true), until)) => remembered = remembered.until(until),
                _ => self.ask_barrier_right(ctx, group, &call).await?,
            }
        }
        let call = group.e_call();
        let scope = match self.recall(ctx, &call, MemoUse::Live) {
            Some((MemoResult::Scope(scope), until)) => {
                remembered = remembered.until(until);
                scope
            }
            _ => self.decide_consume_scope(ctx, group, &call).await?,
        };
        Ok((scope, remembered))
    }

    /// The memo's answer to `call` and the instant it stops serving, when
    /// `memo_use` admits it.
    fn recall(
        &self,
        ctx: &SecurityContext,
        call: &ConsumeCall,
        memo_use: MemoUse,
    ) -> Option<(MemoResult, tokio::time::Instant)> {
        let request = call.evaluation_request(ctx, &self.policy_enforcer);
        self.memo
            .recall(&request, tokio::time::Instant::now())
            .filter(|(_, until)| memo_use.admits(*until))
    }

    /// E, then R when E fails: the kept scope, or the refusal R classifies.
    async fn decide_consume_scope(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
        call: &ConsumeCall,
    ) -> Result<Arc<CompiledScope>, DomainError> {
        match self.ask_consume_scope(ctx, group, call).await? {
            ConsumeAnswer::Kept(scope) => Ok(scope),
            ConsumeAnswer::Refused { reason } => {
                Err(self.classify_refusal(ctx, group, reason).await)
            }
        }
    }

    /// I: `Ok` when the caller may read across barriers on the group's topic.
    async fn ask_barrier_right(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
        call: &ConsumeCall,
    ) -> Result<(), DomainError> {
        match self.ask_barrier(ctx, call).await {
            Ok(()) => Ok(()),
            Err(EnforcerError::CompileFailed(_)) => {
                tracing::warn!(
                    topic = %group.topic(),
                    "the barrier-right answer does not compile; refusing the tenant"
                );
                Err(Self::consume_refusal(
                    ErrorCode::TenantIdNotAuthorized,
                    None,
                    String::new(),
                ))
            }
            Err(error) => Err(with_forbidden_code(
                error.into(),
                ErrorCode::TenantIdNotAuthorized,
            )),
        }
    }

    /// I asked: `Ok` when allowed, which is written to the memo, else the
    /// enforcer's error.
    async fn ask_barrier(
        &self,
        ctx: &SecurityContext,
        call: &ConsumeCall,
    ) -> Result<(), EnforcerError> {
        call.send(ctx, &self.policy_enforcer).await?;
        self.memo.write(
            &call.evaluation_request(ctx, &self.policy_enforcer),
            MemoResult::Decision(true),
            tokio::time::Instant::now(),
        );
        Ok(())
    }

    /// E: the scope the caller may consume, or a refusal with the PDP's deny
    /// reason when it gave one. Only a failed evaluation is an error.
    async fn ask_consume_scope(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
        call: &ConsumeCall,
    ) -> Result<ConsumeAnswer, DomainError> {
        let scope = match call.send(ctx, &self.policy_enforcer).await {
            Ok(scope) => scope,
            Err(EnforcerError::AccessDenied(AccessDenial::Pdp { reason })) => {
                return Ok(ConsumeAnswer::Refused {
                    reason: reason.and_then(|r| r.details),
                });
            }
            // A PEP-local refusal, such as a permitted scope that matches
            // nothing, carries no PDP deny reason.
            Err(EnforcerError::AccessDenied(_)) => {
                return Ok(ConsumeAnswer::Refused { reason: None });
            }
            Err(EnforcerError::CompileFailed(_)) => {
                tracing::warn!(
                    topic = %group.topic(),
                    "the consume answer does not compile; refusing the group"
                );
                return Ok(ConsumeAnswer::Refused { reason: None });
            }
            Err(error) => return Err(error.into()),
        };
        match CompiledScope::compile(&scope, group.tenant_mode(), &self.tenant_sets) {
            Ok(compiled) => {
                let compiled = Arc::new(compiled);
                self.memo.write(
                    &call.evaluation_request(ctx, &self.policy_enforcer),
                    MemoResult::Scope(Arc::clone(&compiled)),
                    tokio::time::Instant::now(),
                );
                Ok(ConsumeAnswer::Kept(compiled))
            }
            Err(not_enforceable) => {
                tracing::warn!(
                    topic = %group.topic(),
                    dropped = ?not_enforceable.dropped,
                    "no constraint of the consume answer can be enforced; refusing the group"
                );
                Ok(ConsumeAnswer::Refused { reason: None })
            }
        }
    }

    /// R: after E failed, a tenant the caller reaches makes it a topic
    /// refusal carrying E's `reason`, and one it does not a tenant refusal.
    async fn classify_refusal(
        &self,
        ctx: &SecurityContext,
        group: &ConsumeGroup,
        reason: Option<String>,
    ) -> DomainError {
        match group.r_call().send(ctx, &self.policy_enforcer).await {
            Ok(_) => Self::consume_refusal(
                ErrorCode::TopicNotAuthorized,
                reason,
                group.topic().to_string(),
            ),
            Err(EnforcerError::CompileFailed(_)) => {
                tracing::warn!(
                    topic = %group.topic(),
                    "the tenant-check answer does not compile; refusing the topic"
                );
                Self::consume_refusal(
                    ErrorCode::TopicNotAuthorized,
                    reason,
                    group.topic().to_string(),
                )
            }
            Err(error) => with_forbidden_code(error.into(), ErrorCode::TenantIdNotAuthorized),
        }
    }

    /// A consume refusal carrying the PDP's deny reason when it gave one.
    fn consume_refusal(code: ErrorCode, reason: Option<String>, resource: String) -> DomainError {
        DomainError::Forbidden {
            code,
            message: reason.unwrap_or_else(|| "access denied by policy".to_owned()),
            resource,
        }
    }
}

/// Which memo entries an evaluation reuses instead of asking the PDP.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MemoUse {
    /// Every live entry: a stream open reuses what JOIN or an earlier open
    /// asked.
    Live,
    /// Only an entry that still serves after this instant. A refresh passes
    /// `now` plus the group's refresh lead: a group renewed from an entry
    /// that expires sooner would be due again at once. The entry the group
    /// was built from expires with the group, so it never qualifies and every
    /// refresh moves the group into a new window; a fresher entry another
    /// open or refresh asked is reused.
    OutlivingPast(tokio::time::Instant),
}

impl MemoUse {
    /// Whether an entry that stops serving at `until` may be reused.
    fn admits(self, until: tokio::time::Instant) -> bool {
        match self {
            Self::Live => true,
            Self::OutlivingPast(past) => until > past,
        }
    }
}

/// What re-evaluating one group for its scope's refresh worker found.
#[derive(Debug)]
pub(crate) enum Renewal {
    /// The group's new scope, and until when the memo answers it reused
    /// serve.
    Renewed {
        scope: Arc<CompiledScope>,
        remembered: Remembered,
    },
    /// I or E denied, or answered with nothing that compiles or can be
    /// enforced: the grant is gone.
    Revoked,
    /// I or E failed to evaluate, the deadline included: the PDP's state is
    /// unknown.
    Unavailable,
}

/// What E answered for one group.
enum ConsumeAnswer {
    /// The compiled scope, kept for the group.
    Kept(Arc<CompiledScope>),
    /// Denied, not compilable, or nothing enforceable; `reason` is the PDP's
    /// deny reason when it gave one.
    Refused { reason: Option<String> },
}

/// When each group of a scope evaluated at `now` stops serving. Group `k` of
/// `count` serves until `now + ttl + ttl * k / count`, so the groups of one
/// member do not all expire, and get evaluated again, at once.
struct ScopeExpiry {
    now: tokio::time::Instant,
    ttl: Duration,
    count: u32,
}

/// Until when the memo answers that a group reused keep serving.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Remembered {
    /// Every answer was asked.
    No,
    /// The earliest expiry of the memo entries reused.
    Until(tokio::time::Instant),
}

impl ScopeExpiry {
    /// The expiries of `count` groups evaluated at `now`, each kept for `ttl`
    /// and staggered across one more `ttl`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] only on a broken internal invariant: more
    /// groups than `u32` counts, which the interest limit rules out.
    fn new(now: tokio::time::Instant, ttl: Duration, count: usize) -> Result<Self, DomainError> {
        let count = u32::try_from(count)
            .map_err(|_| DomainError::Internal("too many interest groups to stagger".to_owned()))?;
        Ok(Self { now, ttl, count })
    }

    /// Group `k`'s expiry, no later than `remembered`.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] only on a broken internal invariant: an
    /// expiry past what `Instant` holds, or no groups to stagger.
    fn of_group(
        &self,
        k: usize,
        remembered: Remembered,
    ) -> Result<tokio::time::Instant, DomainError> {
        let staggered = u32::try_from(k)
            .ok()
            .and_then(|k| self.ttl.checked_mul(k))
            .and_then(|spread| spread.checked_div(self.count))
            .and_then(|offset| self.ttl.checked_add(offset))
            .and_then(|lifetime| self.now.checked_add(lifetime))
            .ok_or_else(|| {
                DomainError::Internal("an interest group's expiry is out of range".to_owned())
            })?;
        Ok(remembered.cap(staggered))
    }
}

impl Remembered {
    /// Also bounded by a memo entry that expires at `until`.
    fn until(self, until: tokio::time::Instant) -> Self {
        match self {
            Self::No => Self::Until(until),
            Self::Until(earlier) => Self::Until(earlier.min(until)),
        }
    }

    /// `at`, or the earliest expiry of the reused entries when that is
    /// sooner: a group never serves past an answer it reused.
    pub(crate) fn cap(self, at: tokio::time::Instant) -> tokio::time::Instant {
        match self {
            Self::No => at,
            Self::Until(until) => at.min(until),
        }
    }
}

/// The group check: `action` on one consumer group, a decision only. An
/// anonymous group is asked as [`CONSUMER_GROUP_FAMILY_RESOURCE`], naming the
/// group and its tenant, with that tenant as the root; a named group as its own
/// resource ([`consumer_group_resource`]), naming the group, with the caller's
/// tenant as the root. Both in `root_only` mode with barriers respected, so a
/// tenant-aware PDP answers it with its rule for one tenant, and the request
/// declares the `owner_tenant_id` that rule's allow names.
#[domain_model]
#[derive(Debug, Clone)]
pub(crate) struct GroupCheck {
    resource: ResourceType,
    action: &'static str,
    request: AccessRequest,
}

impl GroupCheck {
    /// The check of `action` on `group` for `ctx`.
    pub(crate) fn new(ctx: &SecurityContext, group: &ConsumerGroup, action: &'static str) -> Self {
        let (resource, root, request) = match group.kind {
            ConsumerGroupKind::Anonymous => (
                CONSUMER_GROUP_FAMILY_RESOURCE,
                group.tenant_id,
                AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, group.tenant_id),
            ),
            ConsumerGroupKind::Named => (
                consumer_group_resource(&group.id),
                ctx.subject_tenant_id(),
                AccessRequest::new(),
            ),
        };
        Self {
            resource,
            action,
            request: request
                .resource_property("consumer_group_id", group.id.as_ref())
                .context_tenant_id(root)
                .tenant_mode(TenantMode::RootOnly)
                .barrier_mode(BarrierMode::Respect)
                .require_constraints(false),
        }
    }

    /// Asks the PDP.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] with [`ErrorCode::ConsumerGroupNotAuthorized`]
    /// when the PDP denies; [`DomainError::Internal`] when its answer does not
    /// compile or the evaluation fails.
    pub(crate) async fn authorize(
        &self,
        ctx: &SecurityContext,
        enforcer: &PolicyEnforcer,
    ) -> Result<(), DomainError> {
        enforcer
            .access_scope_with(ctx, &self.resource, self.action, None, &self.request)
            .await
            .map_err(|e| with_forbidden_code(e.into(), ErrorCode::ConsumerGroupNotAuthorized))?;
        Ok(())
    }

    /// The request [`Self::authorize`] sends, as the PDP receives it, which
    /// keys the memo.
    pub(crate) fn evaluation_request(
        &self,
        ctx: &SecurityContext,
        enforcer: &PolicyEnforcer,
    ) -> EvaluationRequest {
        enforcer.build_request_with(ctx, &self.resource, self.action, None, false, &self.request)
    }
}

/// Filters `items` down to the ones whose tenant (`tenant_of`) the calling
/// principal is authorized for, calling `tenant_authorized` once per
/// *distinct* tenant present rather than once per item, since the decision
/// depends on the tenant alone - a denial excludes that tenant's items; any
/// other error propagates, since that's a genuine backend failure, not a
/// decline.
async fn filter_by_authorized_tenant<T>(
    items: Vec<T>,
    ctx: &SecurityContext,
    policy_enforcer: &PolicyEnforcer,
    action: &'static str,
    tenant_of: impl Fn(&T) -> Uuid,
) -> Result<Vec<T>, DomainError> {
    let mut authorized: std::collections::HashMap<Uuid, bool> = std::collections::HashMap::new();
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let tenant = tenant_of(&item);
        let is_authorized = if let Some(cached) = authorized.get(&tenant) {
            *cached
        } else {
            let ok = match tenant_authorized(ctx, policy_enforcer, action, tenant).await {
                Ok(()) => true,
                Err(DomainError::Forbidden { .. }) => false,
                Err(other) => return Err(other),
            };
            authorized.insert(tenant, ok);
            ok
        };
        if is_authorized {
            result.push(item);
        }
    }
    Ok(result)
}

impl<R> DeliveryServiceImpl<R>
where
    R: crate::domain::repo::ConsumerGroupRepo + Send + Sync + 'static,
{
    /// The consumer group `id`.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] with [`ErrorCode::ConsumerGroupNotFound`] when
    /// no group `id` is registered; what the repo read fails with.
    async fn find_registered_group(
        &self,
        id: &GtsInstanceId,
    ) -> Result<ConsumerGroup, DomainError> {
        self.repo
            .find_consumer_group(id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ConsumerGroupNotFound,
                message: format!("consumer group '{id}' is not registered"),
                resource: id.to_string(),
            })
    }

    /// Fetches `id`, returning `ConsumerGroupNotFound` if absent, then
    /// authorizes `action` against it per `docs/DESIGN.md`'s Consumer Group
    /// Lifecycle shape split: `Anonymous` groups go through the tenant gate
    /// on the group's tenant (`tenant_authorized`); `Named` groups go through
    /// the group check on their own id ([`GroupCheck`]) - they're
    /// permission-owned, not tenant-owned. The REST get and delete handlers
    /// answer 501 for a named group before this (`reject_named_group`; tracked
    /// in #5702).
    async fn find_authorized_consumer_group(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        id: &GtsInstanceId,
    ) -> Result<ConsumerGroup, DomainError> {
        let group = self.find_registered_group(id).await?;
        match group.kind {
            ConsumerGroupKind::Anonymous => {
                tenant_authorized(ctx, &self.policy_enforcer, action, group.tenant_id).await?;
            }
            ConsumerGroupKind::Named => {
                GroupCheck::new(ctx, &group, action)
                    .authorize(ctx, &self.policy_enforcer)
                    .await?;
            }
        }
        Ok(group)
    }

    /// The group check of a stream open or a seek on `subscription`'s group:
    /// the group read by id, then `consume` on it through the memo.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] with [`ErrorCode::ConsumerGroupNotFound`] when
    /// the group is no longer registered; what [`GroupCheck::authorize`]
    /// refuses or fails with.
    async fn recall_group_check(
        &self,
        ctx: &SecurityContext,
        subscription: &Subscription,
    ) -> Result<(), DomainError> {
        let group = self
            .find_registered_group(&subscription.consumer_group)
            .await?;
        self.consume
            .recall_group(ctx, &GroupCheck::new(ctx, &group, "consume"))
            .await
    }
}

#[async_trait]
impl<R> DeliveryService for DeliveryServiceImpl<R>
where
    R: crate::domain::repo::RoutingMarkers
        + crate::domain::repo::CursorRepo
        + crate::domain::repo::ConsumerGroupRepo
        + Send
        + Sync
        + 'static,
{
    async fn join(
        &self,
        ctx: &SecurityContext,
        request: JoinRequest,
    ) -> Result<JoinOutcome, DomainError> {
        // Before the first await: a refused JOIN reads nothing and asks no PDP.
        // `client_agent`, then `session_timeout`, are judged ahead of the
        // identity guard, so a malformed JOIN is a `400` whoever sends it. Only
        // an in-process call can fail these checks: the REST handler refuses
        // the same values first, as the documented REST check order requires.
        event_broker_sdk::validate::client_agent(&request.client_agent)?;
        request.check_session_timeout()?;
        require_caller_identity(ctx)?;
        request.check_interests(ctx.subject_tenant_id())?;
        let group = self
            .repo
            .find_consumer_group(&request.consumer_group)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ConsumerGroupNotFound,
                resource: request.consumer_group.to_string(),
                message: format!(
                    "consumer group '{}' is not registered - anonymous groups must be created \
                     via POST /v1/consumer-groups first",
                    request.consumer_group
                ),
            })?;
        // Only the creator, in its tenant, may JOIN an anonymous group.
        // Checked before any PDP call, and before the coordinator evicts the
        // group's `Disconnected` members.
        group.require_owner(ctx)?;
        // Group-level JOIN authorization (`docs/DESIGN.md` "Consumer Group
        // Lifecycle (Delivery)"), distinct from the consume checks per group
        // of interests below, which gate what the caller may *consume*, not
        // whether it may join *this group* at all: the group check, `consume`
        // on the group. A named group never reaches it over REST: the JOIN
        // handler answers 501 first (`reject_named_group`; named groups are
        // tracked in #5702).
        self.consume
            .decide_group(ctx, &GroupCheck::new(ctx, &group, "consume"))
            .await?;

        // Every group is decided before anything is created: the first
        // refusal ends the JOIN.
        let mut decided = Vec::new();
        let mut unmatched_types = Vec::new();
        for group in InterestGroup::form(&request.interests, ctx.subject_tenant_id())? {
            // The spec cache never deletes a type (#5756), so a type removed
            // from the registry stays a candidate; the PDP still decides it.
            let candidates = self
                .spec_manager
                .get_event_type_candidates(group.topic())
                .await?;
            let group = group.with_candidates(candidates);
            let scope = self.consume.authorize_consume(ctx, &group).await?;
            let registered = self
                .spec_manager
                .get_registered_event_types(group.topic(), &group.unlisted_exact_type_ids())
                .await?;
            unmatched_types.extend(group.unmatched_types(&registered));
            decided.push((group, scope));
        }
        unmatched_types.sort_by_key(|(position, _)| *position);
        for (group, scope) in &decided {
            group.post_check(scope)?;
        }

        let sub_id = Uuid::new_v4();
        let mut topic_interests = Vec::new();
        let mut topics = Vec::new();
        for interest in &request.interests {
            let topic = self
                .spec_manager
                .get_topic(&interest.topic)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    code: ErrorCode::TopicNotFound,
                    message: format!("topic '{}' is not registered", interest.topic),
                    resource: interest.topic.to_string(),
                })?;
            topics.push(topic.id.clone());
            topic_interests.push(TopicInterest {
                id: topic.id.clone(),
                partitions: *topic.settings.partitions().value(),
            });
        }

        // The owner marker is keyed by the UUID the group's id ends in. Read
        // before any marker is written, so a refused JOIN leaves none behind.
        let group_uuid =
            event_broker_sdk::ConsumerGroupId::try_from_gts(request.consumer_group.as_ref())
                .ok_or_else(|| {
                    DomainError::Internal(format!(
                        "consumer group '{}' has no owner marker: its id does not end in a UUID \
                         (named consumer groups are tracked in #5702)",
                        request.consumer_group
                    ))
                })?;
        // Markers first: if the cluster cache cannot take them, the JOIN fails
        // before a member exists that no dispatcher could route to.
        self.repo
            .mark_subscription(sub_id, &request.consumer_group)
            .await?;
        self.repo.mark_group(group_uuid).await?;

        // `assigned` and `topology_version` are the coordinator's to fill.
        let outcome = self.groups.join(
            Subscription {
                id: sub_id,
                tenant_id: ctx.subject_tenant_id(),
                joiner: Joiner::from(ctx),
                consumer_group: request.consumer_group,
                client_agent: request.client_agent,
                interests: request.interests,
                topics,
                assigned: Vec::new(),
                topology_version: 0,
                session_timeout: request.session_timeout,
                created_at: Utc::now(),
            },
            &topic_interests,
        );
        for removal in &outcome.evicted {
            crate::domain::consumer_group_coordinator::sweeper::clear_markers(&*self.repo, removal)
                .await;
        }
        Ok(JoinOutcome {
            subscription: outcome.subscription,
            unmatched_types,
        })
    }

    async fn leave(&self, ctx: &SecurityContext, subscription_id: Uuid) -> Result<(), DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        let subscription = self
            .find_authorized_subscription(ctx, subscription_id)
            .await?;
        subscription.require_joiner(ctx)?;
        // `None` is the sweep having reaped it since the lookup - already gone,
        // markers already cleared.
        if let Some(removal) = self
            .groups
            .leave(&subscription.consumer_group, subscription_id)
        {
            crate::domain::consumer_group_coordinator::sweeper::clear_markers(
                &*self.repo,
                &removal,
            )
            .await;
        }
        Ok(())
    }

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        let subscriptions = self.groups.list();
        filter_by_authorized_tenant(subscriptions, ctx, &self.policy_enforcer, "consume", |s| {
            s.tenant_id
        })
        .await
    }

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<Subscription, DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        self.find_authorized_subscription(ctx, subscription_id)
            .await
    }

    /// Opens the stream in this order, every step in the domain layer:
    /// 1. the caller's identity: a nil subject or tenant is a 403 before any
    ///    read or PDP call;
    /// 2. the tenant gate (`find_authorized_subscription`): 404 when the
    ///    subscription is not a group member, 403 when the caller may not
    ///    consume as its tenant;
    /// 3. the joiner (`Subscription::require_joiner`): 403
    ///    `SubscriptionNotOwned` when another principal joined it;
    /// 4. the group check (`recall_group_check`), answered from the memo when
    ///    JOIN, a seek or an earlier open left an allow there: 404
    ///    `ConsumerGroupNotFound` when the group is no longer registered, 403
    ///    `ConsumerGroupNotAuthorized` when the policy refuses `consume` on
    ///    it, 500 when its answer does not compile or its evaluation fails;
    /// 5. the cursors: 409 `PositionsNotSet` for an unseeded assigned
    ///    partition;
    /// 6. the stream lease: 409 `StreamingInProgress` while another stream is
    ///    open;
    /// 7. the member's consume scope, reused or evaluated and stored
    ///    (`hold_member_scope`): a 403, 500 or 503 as JOIN answers it, a 404
    ///    when the member is gone, or a 500 when the store is superseded
    ///    twice;
    /// 8. `subscribe` and the member's scope feed (404 when the member is
    ///    gone), the stream's scope refresh worker, then the session, which
    ///    decides each event from the held scope and every later one the
    ///    feed brings, and whose drop stops the worker.
    ///
    /// Every failure after the lease drops it with the open.
    async fn stream(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<FrameStream, DomainError> {
        // Before the first await: a refused open reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        let subscription = self
            .find_authorized_subscription(ctx, subscription_id)
            .await?;
        subscription.require_joiner(ctx)?;
        self.recall_group_check(ctx, &subscription).await?;

        // One pass for both jobs: a partition with no cursor is unseeded, and
        // the cursors that do exist are where delivery starts.
        let mut unseeded = Vec::new();
        let mut cursors = Vec::with_capacity(subscription.assigned.len());
        for assignment in &subscription.assigned {
            match self
                .repo
                .find_cursor(
                    &subscription.consumer_group,
                    &assignment.topic,
                    assignment.partition,
                )
                .await?
            {
                Some(cursor) => cursors.push(cursor),
                None => unseeded.push(format!("{}:{}", assignment.topic, assignment.partition)),
            }
        }
        if !unseeded.is_empty() {
            return Err(DomainError::Conflict {
                code: ErrorCode::PositionsNotSet,
                message: format!("unseeded assigned partitions: {}", unseeded.join(", ")),
                resource: subscription_id.to_string(),
            });
        }

        // The lease is a field of the session the returned stream owns, so
        // exclusion cannot outlive the stream.
        let lease = self
            .leases
            .acquire(subscription_id)
            .ok_or_else(|| DomainError::Conflict {
                code: ErrorCode::StreamingInProgress,
                message: format!("subscription '{subscription_id}' already has an open stream"),
                resource: subscription_id.to_string(),
            })?;

        let (held, listed) = self.hold_member_scope(ctx, &subscription).await?;

        // Compiled per open, not per event; the scope it starts from is the
        // one just held, and the session hands it every later one.
        let filter: Arc<dyn EventFilter> = Arc::new(ScopedFilter::new(
            &subscription.interests,
            held,
            SingleTypeAuthorizer::new(
                self.policy_enforcer.clone(),
                Arc::clone(&self.spec_manager),
                Arc::clone(&self.tenant_sets),
                ctx.clone(),
                Duration::from_secs(u64::from(self.streaming.authz_eval_deadline_secs)),
            ),
        )?);

        // Subscribing takes the membership handle with it: the session holds
        // both, so dropping the stream both stops reading and reports the
        // stream closed. `None` means the member is gone underneath us, which a
        // caller racing a LEAVE can legitimately see.
        let (generations, membership) =
            crate::domain::consumer_group_coordinator::ConsumerGroupCoordinator::subscribe(
                &self.groups,
                &subscription.consumer_group,
                subscription_id,
            )
            .ok_or_else(|| Self::no_longer_member(subscription_id))?;
        // Taken after the scope was held: a newer scope, or none, stored
        // since then is what the session's first pass reads.
        let scope = self
            .groups
            .watch_scope(subscription_id)
            .ok_or_else(|| Self::no_longer_member(subscription_id))?;
        // One refresh worker per open stream, off the delivery path: it
        // re-evaluates each group ahead of its expiry, and the session only
        // reads what it stores. The session holds its cancellation guard, so
        // the worker stops when the stream is dropped. A worker starts idle;
        // `start` runs its first pass, which schedules the first refresh.
        let refresh = ScopeRefresh::new(
            &subscription,
            ctx.clone(),
            listed,
            Arc::clone(&self.groups),
            Arc::clone(&self.consume),
            &self.streaming,
        )?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let start = Arc::new(tokio::sync::Notify::new());
        let worker = WorkerBuilder::new("consume-scope-refresh", cancel.clone())
            .notifier(Arc::clone(&start))
            .build(refresh);
        tokio::spawn(worker.run());
        start.notify_one();
        let scope = ScopeFeed::refreshed(scope, cancel.drop_guard());

        let ready = Arc::new(tokio::sync::Notify::new());
        let slots = self
            .attacher
            .attach_readers(&subscription.assigned, &cursors, &ready);

        let cfg = &self.streaming;
        let session = StreamSession::open(SessionOpening {
            read_set: ReadSet::seed(slots),
            filter,
            progress: ProgressConfig {
                drift_threshold: cfg.progress_drift_threshold,
                min_interval: Duration::from_secs(u64::from(cfg.progress_min_interval_secs)),
            },
            heartbeat_interval: self.heartbeat_interval,
            limit: ReadLimit::new(
                MaxEvents(cfg.read_batch_max_events),
                MaxBytes(cfg.read_batch_max_bytes),
            ),
            // What the read set was seeded from: a rebalance since the
            // subscription was read reaches the session as its first change.
            baseline: Generation::new(subscription.topology_version, subscription.assigned),
            ready,
            started_at: tokio::time::Instant::now(),
            now: Arc::new(chrono::Utc::now),
            unanswerable_tolerance: UNANSWERABLE_TOLERANCE,
            lease,
            generations,
            membership,
            scope: Some(scope),
        });

        Ok(FrameStream::new(session))
    }

    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        topology_version: i64,
        targets: Vec<SeekTarget>,
    ) -> Result<Vec<SeekPosition>, DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        let subscription = self
            .find_authorized_subscription(ctx, subscription_id)
            .await?;
        subscription.require_joiner(ctx)?;
        // A seek writes the group's shared cursors.
        self.recall_group_check(ctx, &subscription).await?;

        if self.leases.is_held(subscription_id) {
            return Err(DomainError::Conflict {
                code: ErrorCode::StreamingInProgress,
                message: format!("subscription '{subscription_id}' has an open stream"),
                resource: subscription_id.to_string(),
            });
        }

        // Resolve every target's topic first (404 for any unregistered one). An
        // unregistered topic is a fixed fact about the request, independent of
        // topology, so it is judged before the version fence.
        let mut with_topics = Vec::with_capacity(targets.len());
        for target in targets {
            let topic = self
                .spec_manager
                .get_topic(&target.topic)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    code: ErrorCode::TopicNotFound,
                    message: format!("topic '{}' is not registered", target.topic),
                    resource: target.topic.to_string(),
                })?;
            with_topics.push((target, topic));
        }

        // Topology-version fence: the caller echoes the `topology_version` it
        // last observed; if the group has rebalanced since, its view of the
        // assignment is stale. Reject the whole seek here - after topic
        // registration, before any per-partition assignment check and before any
        // cursor write - so a partition reassigned by a concurrent rebalance is
        // told apart from one the caller never owned (a client error), and a
        // mismatched seek seeds nothing. The body carries no version or
        // assignment: the caller re-reads the subscription, the single source of
        // truth for the fresh state, and re-seeks.
        if topology_version != subscription.topology_version {
            return Err(DomainError::Conflict {
                code: ErrorCode::TopologyVersionMismatch,
                message: "subscription topology changed; re-read the subscription and re-seek"
                    .to_owned(),
                resource: subscription_id.to_string(),
            });
        }

        // Resolve every target before applying any: a request naming several
        // partitions applies all of them or none, so a single inadmissible
        // position leaves no cursor written.
        let mut resolved = Vec::new();
        // Every offending entry is reported, not just the first, so a caller
        // correcting several positions needs one round trip rather than one
        // per partition.
        let mut violations = Vec::new();
        for (target, topic) in with_topics {
            // SEEK only applies to partitions currently assigned to this
            // subscription. The version fence above guarantees the caller's view
            // is current here, so an unassigned partition is a genuine client
            // error (not a stale-view race). Checked before any backend work and
            // before any cursor is written, so a request naming one unassigned
            // partition seeds none of them. Fail on the first offender, as the
            // reference mock does.
            if !subscription
                .assigned
                .iter()
                .any(|a| a.topic == target.topic && a.partition == target.partition)
            {
                return Err(DomainError::Conflict {
                    code: ErrorCode::PartitionNotAssigned,
                    message: format!(
                        "partition is not assigned to subscription {subscription_id} \
                         (topology_version={})",
                        subscription.topology_version
                    ),
                    resource: format!("{}:{}", target.topic, target.partition),
                });
            }
            let backend = self.backend_resolver.resolve(&topic);
            // The backend owns this: it is the only thing that knows where
            // retention has reached, what it has assigned, and when each event
            // occurred. An inadmissible `Exact` fails to resolve.
            let at = event_broker_sdk::backend::TopicPartition::new(
                topic.id.clone(),
                event_broker_sdk::backend::Partition::new(target.partition.cast_unsigned()),
            );
            let position = target.value.clone();
            let outcome = match backend.resolve(&at, target.value).await {
                // A registered topic its backend does not hold yet reads as a
                // partition nothing was written to (feature 0007 §4.3).
                Err(event_broker_sdk::backend::BackendError::NotProvisioned { .. }) => {
                    resolve_unprovisioned(&position)
                }
                outcome => outcome,
            };
            match outcome {
                Ok(offset) => resolved.push(SeekPosition {
                    topic: target.topic,
                    partition: target.partition,
                    offset,
                }),
                Err(event_broker_sdk::backend::BackendError::OffsetOutOfRange {
                    floor,
                    ceiling,
                    breached,
                    ..
                }) => violations.push(
                    event_broker_sdk::PositionViolation::builder(
                        target.topic.to_string(),
                        target.partition,
                    )
                    .floor(floor)
                    .ceiling(ceiling)
                    .breached(breached)
                    .build(),
                ),
                Err(other) => return Err(other.into()),
            }
        }
        if !violations.is_empty() {
            return Err(DomainError::SeekOutOfRange { violations });
        }

        // Only now that every target resolved does anything become durable,
        // and every cursor is written in one transaction, so a failed or
        // dropped write leaves none of them written.
        let cursors: Vec<_> = resolved
            .iter()
            .map(|position| crate::domain::model::Cursor {
                topic: position.topic.clone(),
                consumer_group: subscription.consumer_group.clone(),
                partition: position.partition,
                offset: position.offset,
            })
            .collect();
        self.repo.put_cursors(&cursors).await?;
        Ok(resolved)
    }

    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        input: crate::domain::model::ConsumerGroupCreateInput,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError> {
        // The persisted text fields, checked on every path in (REST and the
        // in-process broker) and ahead of authorization, so a malformed
        // request is a `400` whoever sends it.
        if let Some(client_agent) = input.client_agent.as_deref() {
            event_broker_sdk::validate::client_agent(client_agent)?;
        }
        if let Some(description) = input.description.as_deref() {
            event_broker_sdk::validate::description(description)?;
        }
        // Before the first await, after the text checks: a refused create
        // stores nothing and asks no PDP.
        require_caller_identity(ctx)?;

        // `docs/DESIGN.md` "Consumer Group Lifecycle (Delivery)": a caller
        // without `define` on the consumer group family is refused with
        // `ConsumerGroupNotAuthorized`. No `consumer_group_id` property - the
        // group doesn't exist yet, so this asks whether the principal may
        // define a group at all, in its own tenant: the caller's tenant is the
        // root of a `root_only` context, where the new group will live.
        self.policy_enforcer
            .access_scope_with(
                ctx,
                &CONSUMER_GROUP_FAMILY_RESOURCE,
                "define",
                None,
                &AccessRequest::new()
                    .context_tenant_id(ctx.subject_tenant_id())
                    .tenant_mode(TenantMode::RootOnly)
                    .barrier_mode(BarrierMode::Respect)
                    .require_constraints(false),
            )
            .await
            .map_err(|e| with_forbidden_code(e.into(), ErrorCode::ConsumerGroupNotAuthorized))?;

        tracing::info!(
            client_agent = input.client_agent.as_deref(),
            "consumer group create requested"
        );
        let group = crate::domain::model::ConsumerGroup {
            id: GtsInstanceId::new(
                event_broker_sdk::gts::CONSUMER_GROUP_RESOURCE_TYPE,
                &Uuid::new_v4().to_string(),
            ),
            kind: crate::domain::model::ConsumerGroupKind::Anonymous,
            tenant_id: ctx.subject_tenant_id(),
            owner_principal_id: ctx.subject_id(),
            description: input.description,
            created_at: Utc::now(),
        };
        self.repo.create_consumer_group(group).await
    }

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        self.find_authorized_consumer_group(ctx, "consume", id)
            .await
    }

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::ConsumerGroup>, DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        let groups = self.repo.list_consumer_groups().await?;
        let (anonymous, named): (Vec<_>, Vec<_>) = groups
            .into_iter()
            .partition(|g| g.kind == crate::domain::model::ConsumerGroupKind::Anonymous);
        let mut visible =
            filter_by_authorized_tenant(anonymous, ctx, &self.policy_enforcer, "consume", |g| {
                g.tenant_id
            })
            .await?;
        // Named groups have no shared tenant to batch on: one group check per
        // group keeps each group the check allows, per `docs/DESIGN.md`
        // "Consumer Group Lifecycle (Delivery)".
        for group in named {
            match GroupCheck::new(ctx, &group, "consume")
                .authorize(ctx, &self.policy_enforcer)
                .await
            {
                Ok(()) => visible.push(group),
                Err(DomainError::Forbidden { .. }) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(visible)
    }

    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<(), DomainError> {
        // Before the first await: a refused call reads nothing and asks no PDP.
        require_caller_identity(ctx)?;
        self.find_authorized_consumer_group(ctx, "manage", id)
            .await?
            .require_owner(ctx)?;
        if self.groups.has_members(id) {
            return Err(DomainError::Conflict {
                code: ErrorCode::ConsumerGroupHasActiveMembers,
                message: format!("consumer group '{id}' still has active members"),
                resource: id.to_string(),
            });
        }
        self.repo.delete_consumer_group(id).await
    }
}

/// Where `position` resolves on a partition nothing was written to: head 0,
/// and 0 the only position a cursor can hold.
fn resolve_unprovisioned(
    position: &Position,
) -> Result<Sequence, event_broker_sdk::backend::BackendError> {
    match position {
        Position::Exact(requested) if !requested.is_none() => {
            Err(event_broker_sdk::backend::BackendError::out_of_range(
                *requested,
                Sequence::NONE,
                Sequence::NONE,
            ))
        }
        _ => Ok(Sequence::NONE),
    }
}

#[cfg(test)]
#[path = "join_guard_tests.rs"]
mod join_guard_tests;

#[cfg(test)]
#[path = "management_guard_tests.rs"]
mod management_guard_tests;

#[cfg(test)]
#[path = "stream_open_authz_tests.rs"]
mod stream_open_authz_tests;

#[cfg(test)]
#[path = "subscription_owner_tests.rs"]
mod subscription_owner_tests;

#[cfg(test)]
#[path = "group_owner_tests.rs"]
mod group_owner_tests;

#[cfg(test)]
#[path = "group_check_tests.rs"]
mod group_check_tests;

#[cfg(test)]
#[path = "seek_atomicity_tests.rs"]
mod seek_atomicity_tests;

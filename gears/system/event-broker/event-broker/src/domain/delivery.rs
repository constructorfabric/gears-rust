//! `DeliveryService` (`DESIGN.md:641-658`): consumer-facing, owns the read
//! path and subscription lifecycle.
//!
//! Streaming here is the baseline subset of `event-broker-stream-lifecycle`
//! only: open-time `topology` frame, `event`/`heartbeat` frames,
//! `StreamingInProgress`/`PositionsNotSet` validation, and
//! `DELETE`-as-priority-interrupt. Rebalance-triggered mid-stream frames
//! (`topology` on loss, `terminal` on gain/lose-all, `410
//! SubscriptionTerminated`) need a consumer-group rebalance coordinator that
//! doesn't exist yet and are deferred to a follow-up `OpenSpec` change (see
//! `eb-rest-handlers`'s design.md "Streaming/rebalance scope").
//!
//! The event-delivery loop wakes on `domain::notify::DeliveryNotifier`
//! (design.md D6: a `ClusterCacheV1`-backed, payload-free notification the
//! ingest outbox publishes after a successful persist - see that module's
//! own doc comment for the exact mechanism and a correction against D6's
//! literal per-`(topic, partition)`-key wording) rather than a busy-poll.
//! Still bounded by `heartbeat_interval` each iteration, so idle-heartbeat
//! cadence is unaffected by whether a notification ever arrives.

use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::authz::{
    CONSUMER_GROUP_RESOURCE, EVENT_TYPE_RESOURCE, TOPIC_RESOURCE, tenant_authorized,
    with_forbidden_code,
};
use crate::domain::backend::BackendResolver;
use crate::domain::error::DomainError;
use crate::domain::model::{Assignment, Event, Interest, Subscription};
use crate::domain::notify::DeliveryNotifier;

/// JOIN request body (`DESIGN.md:692`: `consumer_group` + `interests[]`).
/// `session_timeout` is always concrete here - the REST layer resolves the
/// documented `PT30S` default before constructing this.
#[domain_model]
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub consumer_group: GtsInstanceId,
    pub client_agent: String,
    pub interests: Vec<Interest>,
    pub session_timeout: Duration,
}

/// A SEEK request value (`event-broker-seek-endpoint-shape`): an exact
/// broker-logical cursor, or a sentinel the broker resolves against the
/// partition's retained events.
#[domain_model]
#[derive(Debug, Clone)]
pub enum SeekValue {
    Exact(i64),
    Earliest,
    Latest,
    AtTimestamp(DateTime<Utc>),
}

/// One SEEK request target. A subscription can span multiple topics, and
/// assignments/cursors are identified by the full `(topic, partition)`
/// pair (`DESIGN.md:658`) - keying by `partition` alone would collapse
/// partition `0` of two different topics into one entry.
#[domain_model]
#[derive(Debug, Clone)]
pub struct SeekTarget {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub value: SeekValue,
}

/// One resolved SEEK result - `value` is always a concrete broker-logical
/// cursor, sentinels already resolved (`event-broker-seek-endpoint-shape`'s
/// response requirement).
#[domain_model]
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub offset: i64,
}

/// `control` frame fact (`event-broker-consumption-frames`) - the reason
/// (`rebalanced`/`lose_all`/`teardown`) rides in `Frame::Control::reason`,
/// not the code, so recovery logic switches on the fact.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCode {
    Progress,
    Terminal,
}

/// One frame on the consumption stream (`event-broker-consumption-frames`).
/// This change only ever emits `Event`, `Heartbeat`, and the open-time
/// `Topology` baseline - `Control` exists in the type for shape-completeness
/// with the spec but isn't produced by the baseline stream implemented here
/// (see module doc comment).
#[domain_model]
#[derive(Debug, Clone)]
pub enum Frame {
    Event(Box<Event>),
    Heartbeat {
        at: DateTime<Utc>,
    },
    Topology {
        topology_version: i64,
        assigned: Vec<Assignment>,
    },
    Control {
        code: ControlCode,
        positions: Vec<Assignment>,
        reason: Option<String>,
    },
}

/// Active-stream bookkeeping, kept separate from `SubscriptionRepo` because
/// it's concurrency-control state (at most one open stream per
/// subscription), not persisted subscription data. Plain (non-`async`)
/// methods: an in-memory marker needs no I/O, and `StreamHandle`'s `Drop`
/// guard needs to call `clear_streaming` synchronously.
pub trait ActiveStreamMarker: Send + Sync {
    /// `true` and marks streaming if not already active; `false` (does not
    /// mark) if a stream is already open for `subscription_id`.
    fn try_mark_streaming(&self, subscription_id: Uuid) -> bool;
    fn clear_streaming(&self, subscription_id: Uuid);
    /// Read-only check, used by `seek` (rejecting with `StreamingInProgress`
    /// must not itself claim the marker).
    fn is_streaming(&self, subscription_id: Uuid) -> bool;
}

/// Returned by `DeliveryService::stream`. `frames`'s first message is
/// always the open-time `topology` baseline. Dropping this handle - even
/// before `frames` is ever polled - clears the subscription's active-stream
/// marker (`event-broker-stream-lifecycle`'s "Stream active marker follows
/// returned stream lifetime").
#[domain_model]
pub struct StreamHandle {
    pub frames: mpsc::Receiver<Frame>,
    _clear_on_drop: ClearStreamingOnDrop,
}

impl StreamHandle {
    #[must_use]
    pub fn new(frames: mpsc::Receiver<Frame>, clear: impl FnOnce() + Send + 'static) -> Self {
        Self {
            frames,
            _clear_on_drop: ClearStreamingOnDrop(Some(Box::new(clear))),
        }
    }
}

/// `#[domain_model]` on `StreamHandle` requires every field to itself be
/// domain-model-shaped; this newtype gives the boxed cleanup closure a named
/// type to satisfy that rather than a bare `Option<Box<dyn FnOnce() + Send>>`
/// field.
struct ClearStreamingOnDrop(Option<Box<dyn FnOnce() + Send>>);

impl Drop for ClearStreamingOnDrop {
    fn drop(&mut self) {
        if let Some(clear) = self.0.take() {
            clear();
        }
    }
}

#[async_trait]
pub trait DeliveryService: Send + Sync {
    /// Creates a subscription in cache, claims/joins the group
    /// (`DESIGN.md:655`).
    async fn join(
        &self,
        ctx: &SecurityContext,
        request: JoinRequest,
    ) -> Result<Subscription, DomainError>;

    /// Removes the subscription from its group (`DESIGN.md:656`). No
    /// rebalance signal to other members this pass - see module doc
    /// comment.
    async fn leave(&self, ctx: &SecurityContext, subscription_id: Uuid) -> Result<(), DomainError>;

    /// Unfiltered/unpaginated - the REST handler applies `$filter`/pagination
    /// (`api/rest/pagination.rs`).
    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, DomainError>;

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<Subscription, DomainError>;

    /// Opens the consumption stream for `subscription_id`: validates
    /// `StreamingInProgress`/`PositionsNotSet`, then returns a channel whose
    /// first message is the open-time `topology` baseline frame, followed
    /// by `event`/`heartbeat` frames. Dropping the receiver clears the
    /// subscription's active-stream marker (`event-broker-stream-lifecycle`).
    async fn stream(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<StreamHandle, DomainError>;

    /// Resolves and sets the cursor position for each target
    /// `(topic, partition)` (`DESIGN.md:658`,
    /// `event-broker-seek-endpoint-shape`). Pre-stream-only: rejects with
    /// `Conflict { code: "StreamingInProgress", .. }` while a stream is
    /// open, without disturbing it.
    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        targets: Vec<SeekTarget>,
    ) -> Result<Vec<SeekPosition>, DomainError>;

    /// Mints a fresh anonymous consumer group
    /// (`gts.cf.core.events.consumer_group.v1~<uuid>`,
    /// `docs/openapi.yaml`'s `POST /v1/consumer-groups` - "`kind` is always
    /// `anonymous` for this endpoint"; named groups come from
    /// `types_registry`, not this call).
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
    /// applies `$filter`/pagination (`api/rest/pagination.rs`), matching
    /// `list_topics`/`list_event_types`. `Anonymous` groups are filtered to
    /// `ctx`'s authorized tenant(s); `Named` groups are returned unfiltered
    /// (`eb-tenant-isolation-fix`).
    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::ConsumerGroup>, DomainError>;

    /// Fails with `Conflict { code: "ConsumerGroupHasActiveMembers", .. }`
    /// if any subscription currently belongs to the group
    /// (`docs/openapi.yaml`: "Allowed only when there are no active
    /// members").
    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<(), DomainError>;
}

/// Default idle-heartbeat cadence (`event-broker-consumption-frames`'s
/// documented default) - a default, not a fixed constant: this is genuinely
/// configurable, matching the spec's own wording and gRPC's own
/// per-channel-configurable keepalive interval. `config.rs`'s
/// `StreamingConfig::heartbeat_interval_secs` is the operator-facing knob;
/// this const is only its fallback value and the test-only override path
/// (`DeliveryServiceImpl::with_heartbeat_interval`).
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Real `DeliveryService`: subscription lifecycle plus the baseline
/// (non-rebalance) consumption stream. Generic over one repo type
/// implementing every remaining trait it needs (subscriptions, cursors,
/// consumer groups - not topics or events, per `IngestServiceImpl`'s same
/// D1/D3 change), plus `SpecificationManager`/`BackendResolver` for topic
/// resolution and event reads.
pub struct DeliveryServiceImpl<R> {
    repo: std::sync::Arc<R>,
    heartbeat_interval: Duration,
    policy_enforcer: PolicyEnforcer,
    spec_manager: std::sync::Arc<dyn crate::domain::specification::SpecificationManager>,
    backend_resolver: std::sync::Arc<dyn BackendResolver>,
}

impl<R> DeliveryServiceImpl<R> {
    #[must_use]
    pub fn new(
        repo: std::sync::Arc<R>,
        policy_enforcer: PolicyEnforcer,
        spec_manager: std::sync::Arc<dyn crate::domain::specification::SpecificationManager>,
        backend_resolver: std::sync::Arc<dyn BackendResolver>,
    ) -> Self {
        Self {
            repo,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            policy_enforcer,
            spec_manager,
            backend_resolver,
        }
    }

    /// Overrides the heartbeat cadence - test-only, so idle-cadence
    /// behavior is verifiable without a real 5s wait.
    #[must_use]
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }
}

impl<R> DeliveryServiceImpl<R>
where
    R: crate::domain::repo::SubscriptionRepo + Send + Sync + 'static,
{
    /// Fetches `subscription_id`, returning `SubscriptionNotFound` if
    /// absent, then verifies the calling principal is authorized to act as
    /// the subscription's `tenant_id` (`domain::authz::tenant_authorized`,
    /// the same check `join` applies when creating it) - a denial surfaces
    /// as `Forbidden { code: "TenantIdNotAuthorized", .. }` before the
    /// caller reads or mutates anything about the subscription
    /// (`eb-tenant-isolation-fix`).
    async fn find_authorized_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<Subscription, DomainError> {
        let subscription = self
            .repo
            .find_subscription(subscription_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: "SubscriptionNotFound",
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
}

/// Filters `items` down to the ones whose tenant (`tenant_of`) the calling
/// principal is authorized for, calling `tenant_authorized` once per
/// *distinct* tenant present rather than once per item
/// (`eb-tenant-isolation-fix`'s "list filtering" design decision) - a
/// denial excludes that tenant's items; any other error propagates, since
/// that's a genuine backend failure, not a decline.
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
    /// Fetches `id`, returning `ConsumerGroupNotFound` if absent, then
    /// authorizes `action` against it per `docs/DESIGN.md`'s Consumer Group
    /// Lifecycle shape split: `Anonymous` groups check owner-tenant
    /// equality (`tenant_authorized`, the same rule `join` applies);
    /// `Named` groups check the `action` permission via PEP
    /// (`named_group_authorized`) - they're permission-owned, not
    /// tenant-owned (`eb-tenant-isolation-fix`).
    async fn find_authorized_consumer_group(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        id: &GtsInstanceId,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError> {
        let group =
            self.repo
                .find_consumer_group(id)
                .await?
                .ok_or_else(|| DomainError::NotFound {
                    code: "ConsumerGroupNotFound",
                    message: format!("consumer group '{id}' is not registered"),
                    resource: id.to_string(),
                })?;
        match group.kind {
            crate::domain::model::ConsumerGroupKind::Anonymous => {
                tenant_authorized(ctx, &self.policy_enforcer, action, group.tenant_id).await?;
            }
            crate::domain::model::ConsumerGroupKind::Named => {
                self.named_group_authorized(ctx, action, &group.id).await?;
            }
        }
        Ok(group)
    }

    /// Checks `action` (`"define"`/`"consume"`/`"manage"`) against
    /// `CONSUMER_GROUP_RESOURCE` for `group_id` via PEP - the permission
    /// half of the Consumer Group Lifecycle authorization split (`Named`
    /// groups; `Anonymous` groups use `tenant_authorized` instead).
    ///
    /// # Errors
    /// Returns `DomainError::Forbidden { code: "ConsumerGroupNotAuthorized",
    /// .. }` on denial, or the mapped `DomainError` for any other PEP
    /// failure.
    async fn named_group_authorized(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        group_id: &GtsInstanceId,
    ) -> Result<(), DomainError> {
        self.policy_enforcer
            .access_scope_with(
                ctx,
                &CONSUMER_GROUP_RESOURCE,
                action,
                None,
                &AccessRequest::new()
                    .resource_property("consumer_group_id", group_id.as_ref())
                    .require_constraints(false),
            )
            .await
            .map_err(|e| with_forbidden_code(e.into(), "ConsumerGroupNotAuthorized"))?;
        Ok(())
    }
}

#[async_trait]
impl<R> DeliveryService for DeliveryServiceImpl<R>
where
    R: crate::domain::repo::SubscriptionRepo
        + crate::domain::repo::CursorRepo
        + crate::domain::repo::ConsumerGroupRepo
        + ActiveStreamMarker
        + DeliveryNotifier
        + Send
        + Sync
        + 'static,
{
    async fn join(
        &self,
        ctx: &SecurityContext,
        request: JoinRequest,
    ) -> Result<Subscription, DomainError> {
        let group = self
            .repo
            .find_consumer_group(&request.consumer_group)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: "ConsumerGroupNotFound",
                resource: request.consumer_group.to_string(),
                message: format!(
                    "consumer group '{}' is not registered - anonymous groups must be created \
                     via POST /v1/consumer-groups first",
                    request.consumer_group
                ),
            })?;
        // Group-level JOIN authorization (`docs/DESIGN.md`'s Consumer Group
        // Lifecycle: "JOIN authorization differs by shape: anonymous →
        // owner-tenant equality; named → explicit :consume permission via
        // PEP") - distinct from the per-interest topic/event-type/tenant
        // checks below, which gate what the caller may *consume*, not
        // whether it may join *this group* at all (`eb-tenant-isolation-fix`).
        match group.kind {
            crate::domain::model::ConsumerGroupKind::Anonymous => {
                tenant_authorized(ctx, &self.policy_enforcer, "consume", group.tenant_id).await?;
            }
            crate::domain::model::ConsumerGroupKind::Named => {
                self.named_group_authorized(ctx, "consume", &group.id)
                    .await?;
            }
        }

        let mut assigned = Vec::new();
        let mut topics = Vec::new();
        for interest in &request.interests {
            // Authz/tenant-scope enforcement (`gears-rust#4516`,
            // `eb-authz-enforcement`) - any single denial aborts the whole
            // JOIN via `?`'s early return, before a subscription is
            // created (spec.md "A single unauthorized interest blocks the
            // whole JOIN").
            self.policy_enforcer
                .access_scope_with(
                    ctx,
                    &TOPIC_RESOURCE,
                    "consume",
                    None,
                    &AccessRequest::new()
                        .resource_property("topic_id", interest.topic.as_ref())
                        .require_constraints(false),
                )
                .await
                .map_err(|e| with_forbidden_code(e.into(), "TopicNotAuthorized"))?;

            for pattern in &interest.types {
                validate_type_pattern(pattern)?;
                self.policy_enforcer
                    .access_scope_with(
                        ctx,
                        &EVENT_TYPE_RESOURCE,
                        "consume",
                        None,
                        &AccessRequest::new()
                            .resource_property("event_type_id", pattern.clone())
                            .require_constraints(false),
                    )
                    .await
                    .map_err(|e| {
                        let mut err = with_forbidden_code(e.into(), "EventTypeNotAuthorized");
                        if let DomainError::Forbidden {
                            message, resource, ..
                        } = &mut err
                        {
                            *message = format!(
                                "calling principal lacks consume on event type '{pattern}'"
                            );
                            resource.clone_from(pattern);
                        }
                        err
                    })?;
            }

            tenant_authorized(ctx, &self.policy_enforcer, "consume", interest.tenant_id).await?;

            let topic = self
                .spec_manager
                .get_topic(&interest.topic)
                .await
                .ok_or_else(|| DomainError::NotFound {
                    code: "TopicNotFound",
                    message: format!("topic '{}' is not registered", interest.topic),
                    resource: interest.topic.to_string(),
                })?;
            topics.push(topic.id.clone());
            // No live rebalance this pass - a fresh JOIN is assigned every
            // partition of every interested topic (design.md "Streaming/
            // rebalance scope").
            for partition in 0..topic.partitions {
                assigned.push(Assignment {
                    topic: topic.id.clone(),
                    partition,
                    offset: 0,
                    last_examined: 0,
                });
            }
        }

        let now = Utc::now();
        let subscription = Subscription {
            id: Uuid::new_v4(),
            tenant_id: ctx.subject_tenant_id(),
            consumer_group: request.consumer_group,
            client_agent: request.client_agent,
            interests: request.interests,
            topics,
            assigned,
            session_timeout: request.session_timeout,
            last_seen_at: now,
            expires_at: now
                + chrono::Duration::from_std(request.session_timeout)
                    .unwrap_or(chrono::Duration::seconds(30)),
        };
        self.repo.put_subscription(&subscription).await?;
        Ok(subscription)
    }

    async fn leave(&self, ctx: &SecurityContext, subscription_id: Uuid) -> Result<(), DomainError> {
        self.find_authorized_subscription(ctx, subscription_id)
            .await?;
        self.repo.delete_subscription(subscription_id).await
    }

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, DomainError> {
        let subscriptions = self.repo.list_subscriptions().await?;
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
        self.find_authorized_subscription(ctx, subscription_id)
            .await
    }

    async fn stream(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
    ) -> Result<StreamHandle, DomainError> {
        let subscription = self
            .find_authorized_subscription(ctx, subscription_id)
            .await?;

        let mut unseeded = Vec::new();
        for assignment in &subscription.assigned {
            if self
                .repo
                .find_cursor(
                    &subscription.consumer_group,
                    &assignment.topic,
                    assignment.partition,
                )
                .await?
                .is_none()
            {
                unseeded.push(format!("{}:{}", assignment.topic, assignment.partition));
            }
        }
        if !unseeded.is_empty() {
            return Err(DomainError::Conflict {
                code: "PositionsNotSet",
                message: format!("unseeded assigned partitions: {}", unseeded.join(", ")),
                resource: subscription_id.to_string(),
            });
        }

        if !self.repo.try_mark_streaming(subscription_id) {
            return Err(DomainError::Conflict {
                code: "StreamingInProgress",
                message: format!("subscription '{subscription_id}' already has an open stream"),
                resource: subscription_id.to_string(),
            });
        }

        let (tx, rx) = mpsc::channel(16);
        let repo = std::sync::Arc::clone(&self.repo);
        let spec_manager = std::sync::Arc::clone(&self.spec_manager);
        let backend_resolver = std::sync::Arc::clone(&self.backend_resolver);
        let poll_ctx = ctx.clone();
        let heartbeat_interval = self.heartbeat_interval;
        let assigned = subscription.assigned.clone();
        let consumer_group = subscription.consumer_group.clone();
        tokio::spawn(async move {
            if tx
                .send(Frame::Topology {
                    topology_version: 0,
                    assigned: assigned.clone(),
                })
                .await
                .is_err()
            {
                return;
            }

            let mut cursors: std::collections::HashMap<(GtsInstanceId, i32), i64> = assigned
                .iter()
                .map(|a| ((a.topic.clone(), a.partition), a.offset))
                .collect();
            let mut last_frame_at = tokio::time::Instant::now();

            loop {
                let mut delivered_any = false;
                for assignment in &assigned {
                    let Some(topic) = spec_manager.get_topic(&assignment.topic).await else {
                        continue;
                    };
                    let since = cursors[&(assignment.topic.clone(), assignment.partition)];
                    let backend = backend_resolver.resolve(&topic);
                    let max_count = usize::try_from(i32::MAX).unwrap_or(usize::MAX);
                    let events = match backend
                        .read(
                            &poll_ctx,
                            &topic.id.to_string(),
                            assignment.partition.cast_unsigned(),
                            since,
                            max_count,
                        )
                        .await
                    {
                        Ok(events) => events,
                        Err(err) => {
                            tracing::warn!(?err, topic = %assignment.topic, partition = assignment.partition, "failed to query events during delivery poll");
                            continue;
                        }
                    };
                    let events: Vec<Event> = match events
                        .into_iter()
                        .map(crate::domain::backend::from_sdk_event)
                        .collect::<Result<Vec<_>, DomainError>>()
                    {
                        Ok(events) => events,
                        Err(err) => {
                            tracing::warn!(?err, topic = %assignment.topic, "failed to convert backend event during delivery poll");
                            continue;
                        }
                    };
                    for event in events {
                        let Some(sequence) = event.sequence else {
                            continue;
                        };
                        if tx.send(Frame::Event(Box::new(event))).await.is_err() {
                            return;
                        }
                        cursors.insert((assignment.topic.clone(), assignment.partition), sequence);
                        if let Err(err) = repo
                            .put_cursor(&crate::domain::model::Cursor {
                                topic: assignment.topic.clone(),
                                consumer_group: consumer_group.clone(),
                                partition: assignment.partition,
                                offset: sequence,
                            })
                            .await
                        {
                            tracing::warn!(?err, "failed to persist session cursor after delivery");
                        }
                        delivered_any = true;
                        last_frame_at = tokio::time::Instant::now();
                    }
                }

                if !delivered_any && last_frame_at.elapsed() >= heartbeat_interval {
                    if tx.send(Frame::Heartbeat { at: Utc::now() }).await.is_err() {
                        return;
                    }
                    last_frame_at = tokio::time::Instant::now();
                }

                // Wake early on an ingest notification, but never wait past
                // the next heartbeat's own due time - idle-heartbeat cadence
                // must not depend on whether a notification ever arrives.
                let remaining = heartbeat_interval.saturating_sub(last_frame_at.elapsed());
                repo.wait_for_notification(remaining).await;
            }
        });

        let cleanup_repo = std::sync::Arc::clone(&self.repo);
        Ok(StreamHandle::new(rx, move || {
            cleanup_repo.clear_streaming(subscription_id);
        }))
    }

    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: Uuid,
        targets: Vec<SeekTarget>,
    ) -> Result<Vec<SeekPosition>, DomainError> {
        let subscription = self
            .find_authorized_subscription(ctx, subscription_id)
            .await?;

        if self.repo.is_streaming(subscription_id) {
            return Err(DomainError::Conflict {
                code: "StreamingInProgress",
                message: format!("subscription '{subscription_id}' has an open stream"),
                resource: subscription_id.to_string(),
            });
        }

        let mut resolved = Vec::new();
        for target in targets {
            let topic = self
                .spec_manager
                .get_topic(&target.topic)
                .await
                .ok_or_else(|| DomainError::NotFound {
                    code: "TopicNotFound",
                    message: format!("topic '{}' is not registered", target.topic),
                    resource: target.topic.to_string(),
                })?;
            let backend = self.backend_resolver.resolve(&topic);
            let max_count = usize::try_from(i32::MAX).unwrap_or(usize::MAX);
            let events: Vec<Event> = backend
                .read(
                    ctx,
                    &topic.id.to_string(),
                    target.partition.cast_unsigned(),
                    0,
                    max_count,
                )
                .await?
                .into_iter()
                .map(crate::domain::backend::from_sdk_event)
                .collect::<Result<Vec<_>, DomainError>>()?;
            let value = match target.value {
                SeekValue::Exact(v) => v,
                SeekValue::Earliest => 0,
                SeekValue::Latest => events.last().and_then(|e| e.sequence).unwrap_or(0),
                SeekValue::AtTimestamp(ts) => match events.iter().find(|e| e.occurred_at >= ts) {
                    Some(e) => e.sequence.unwrap_or(0).saturating_sub(1),
                    None => events.last().and_then(|e| e.sequence).unwrap_or(0),
                },
            };
            self.repo
                .put_cursor(&crate::domain::model::Cursor {
                    topic: target.topic.clone(),
                    consumer_group: subscription.consumer_group.clone(),
                    partition: target.partition,
                    offset: value,
                })
                .await?;
            resolved.push(SeekPosition {
                topic: target.topic,
                partition: target.partition,
                offset: value,
            });
        }
        Ok(resolved)
    }

    #[toolkit_macros::temporary(
        tracking = "gears-rust#4347",
        reason = "id-mint + uniqueness-check + insert is a single \
                  `repo.create_consumer_group(...)` call with no surrounding \
                  transaction; harmless against `InMemoryDomainRepo`'s single \
                  mutex today, but a real backend needs this wrapped in one \
                  transaction once the pluggable StorageBackend lands"
    )]
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        input: crate::domain::model::ConsumerGroupCreateInput,
    ) -> Result<crate::domain::model::ConsumerGroup, DomainError> {
        // `docs/DESIGN.md`'s Consumer Group Lifecycle: "Errors: 403
        // Forbidden — caller lacks consumer_group:define permission via
        // PEP". No `consumer_group_id` property - the group doesn't exist
        // yet, so this is a general "may this principal define a group at
        // all" capability check, not one scoped to a specific id
        // (`eb-tenant-isolation-fix`).
        self.policy_enforcer
            .access_scope_with(
                ctx,
                &CONSUMER_GROUP_RESOURCE,
                "define",
                None,
                &AccessRequest::new().require_constraints(false),
            )
            .await
            .map_err(|e| with_forbidden_code(e.into(), "ConsumerGroupNotAuthorized"))?;

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
        self.find_authorized_consumer_group(ctx, "consume", id)
            .await
    }

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::ConsumerGroup>, DomainError> {
        let groups = self.repo.list_consumer_groups().await?;
        let (anonymous, named): (Vec<_>, Vec<_>) = groups
            .into_iter()
            .partition(|g| g.kind == crate::domain::model::ConsumerGroupKind::Anonymous);
        let mut visible =
            filter_by_authorized_tenant(anonymous, ctx, &self.policy_enforcer, "consume", |g| {
                g.tenant_id
            })
            .await?;
        // Named groups have no shared tenant to batch on (`docs/DESIGN.md`:
        // "filtered by AccessScope from the PEP ... any named groups the
        // caller has :consume permission on") - one PEP call per group,
        // matching `join`'s own per-interest checks (no batching there
        // either).
        for group in named {
            match self.named_group_authorized(ctx, "consume", &group.id).await {
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
        self.find_authorized_consumer_group(ctx, "manage", id)
            .await?;
        if self.repo.has_active_members(id).await? {
            return Err(DomainError::Conflict {
                code: "ConsumerGroupHasActiveMembers",
                message: format!("consumer group '{id}' still has active members"),
                resource: id.to_string(),
            });
        }
        self.repo.delete_consumer_group(id).await
    }
}

/// GTS §10 wildcard rules, the subset `docs/openapi.yaml`'s `BadTypePattern`
/// documents: a wildcard segment (`*`) must fill its whole dot-separated
/// segment (not a substring of one, not mid-pattern text), and at most one
/// segment may be a wildcard.
fn validate_type_pattern(pattern: &str) -> Result<(), DomainError> {
    let segments: Vec<&str> = pattern.split('.').collect();
    let wildcard_segments = segments.iter().filter(|s| s.contains('*')).count();
    let bad_segment = segments.iter().any(|s| s.contains('*') && *s != "*");
    if wildcard_segments > 1 || bad_segment {
        return Err(DomainError::Validation {
            code: "BadTypePattern",
            message: format!(
                "'{pattern}' violates GTS wildcard rules - a wildcard must fill its whole \
                 segment and at most one segment may be a wildcard"
            ),
        });
    }
    Ok(())
}

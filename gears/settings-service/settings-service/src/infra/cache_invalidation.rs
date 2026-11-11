//! The replica's cache invalidation consumer.
//!
//! One `stateful` background task per replica: it owns the replica's anonymous
//! consumer group on the cache topic, joins it with one interest (the
//! `cache_invalidate` type under the root tenant, offsets in memory from the
//! latest position), and hands each delivery to the key-wide eviction of the
//! domain. The broker is a soft dependency: a lost broker pauses the task, which
//! drops its group best-effort and, on resume, creates a new one.
//!
//! The group's session — creation, join, stream, release — is the shared
//! `broker_group` session, driven through the broker API rather than the SDK's
//! `ConsumerBuilder` (`dispatcher.rs` `ensure_group`); this module is what is
//! the cache consumer's own: its interest, its delivery and its labels.

use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use event_broker_sdk::FrameStream;
use event_broker_sdk::{
    EventBrokerApi, EventV1, GtsIdPattern, GtsInstanceId, api::SubscriptionInterest,
};
use settings_service_sdk::gts::{CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{Span, debug, info, info_span, warn};

use super::broker_group::{
    self, CLEANUP_DEADLINE, GroupSpec, GroupTiming, HEALTHY_STREAM, LIVENESS_PERIOD,
};
#[cfg(test)]
use super::broker_group::{Ended, Held, Taken};
use crate::domain::activation::{NoNotifyMetrics, NotifyMetrics};
use crate::domain::coherence::{decode_cache_invalidation, evict_keys};
use crate::domain::resolution::EffectiveCache;
use crate::infra::event_broker::{Backoff, EventBrokerClient};

/// The most frames taken off the stream at once: what is already waiting
/// behind a delivery, up to this many, is taken with it and evicted in one
/// call, so a burst of broadcasts costs the read path one eviction rather than
/// one per delivery, and the stop and the liveness check are never starved.
pub(crate) const DRAIN_LIMIT: usize = 64;

/// The deliveries taken off the stream together: each one's consume span and
/// the keys it named, evicted as one union.
#[derive(Default)]
pub(crate) struct Taking {
    deliveries: Vec<(Span, Vec<String>)>,
}

impl Taking {
    /// Evict the union of the keys in one call. A single delivery evicts
    /// inside its own `cache_invalidate.consume` span; several evict inside one
    /// `cache_invalidate.evict` span that follows from each of theirs.
    fn evict(self, cache: &EffectiveCache) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-5
        match <[(Span, Vec<String>); 1]>::try_from(self.deliveries) {
            Ok([(span, keys)]) => span.in_scope(|| evict_keys(cache, &keys)),
            Err(deliveries) if deliveries.is_empty() => {}
            Err(deliveries) => {
                let keys: Vec<String> = deliveries
                    .iter()
                    .flat_map(|(_, keys)| keys.iter().cloned())
                    .collect();
                let span = info_span!(
                    "cache_invalidate.evict",
                    deliveries = deliveries.len(),
                    keys = keys.len(),
                );
                for (delivery, _) in &deliveries {
                    span.follows_from(delivery);
                }
                span.in_scope(|| evict_keys(cache, &keys));
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-5
    }
}

/// The per-replica consumer of the `cache_invalidate` broadcast.
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-consumer:p1
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-soft-dependency:p1
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-observability:p1
pub struct CacheInvalidationConsumer {
    client: Arc<EventBrokerClient>,
    cache: Arc<EffectiveCache>,
    metrics: Arc<dyn NotifyMetrics>,
    cleanup_deadline: Duration,
    liveness: Duration,
    healthy: Duration,
}

impl CacheInvalidationConsumer {
    /// A consumer evicting from `cache` on what `client`'s broker delivers.
    #[must_use]
    pub fn new(client: Arc<EventBrokerClient>, cache: Arc<EffectiveCache>) -> Self {
        Self {
            client,
            cache,
            metrics: Arc::new(NoNotifyMetrics),
            cleanup_deadline: CLEANUP_DEADLINE,
            liveness: LIVENESS_PERIOD,
            healthy: HEALTHY_STREAM,
        }
    }

    /// Observe `settings_notify_publish_to_deliver_seconds` for each delivery
    /// through `metrics`.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn NotifyMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Replace the stop deadline, the liveness period and the healthy-stream
    /// period; for tests.
    #[must_use]
    pub const fn with_timing(
        mut self,
        cleanup_deadline: Duration,
        liveness: Duration,
        healthy: Duration,
    ) -> Self {
        self.cleanup_deadline = cleanup_deadline;
        self.liveness = liveness;
        self.healthy = healthy;
        self
    }

    /// Run until `cancel` fires, then leave and delete the group best-effort.
    pub async fn run(&self, cancel: CancellationToken) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-2
        info!("cache invalidation consumer started");
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-2
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-3
        broker_group::run(self, &cancel).await;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-3
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-11
        info!("cache invalidation consumer stopped");
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-11
    }

    /// Take deliveries off `stream` as the session does; for tests.
    #[cfg(test)]
    async fn take(
        &self,
        stream: FrameStream,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Taken {
        broker_group::take(self, stream, &mut (), pause, cancel).await
    }

    /// Leave and delete `held` as the session does; for tests.
    #[cfg(test)]
    async fn release(&self, held: &Held, why: &str) {
        broker_group::release(self, held, why).await;
    }

    /// One delivery taken alone: received, then its keys evicted.
    #[cfg(test)]
    fn deliver(&self, event: &EventV1) {
        let mut taking = Taking::default();
        self.receive(event, &mut taking);
        taking.evict(&self.cache);
    }

    /// One delivery: the consume flow up to the eviction, inside a
    /// `cache_invalidate.consume` span that continues the publisher's trace
    /// when the event carries a `trace_parent`. A malformed body only warns;
    /// a well-formed one leaves its span and its keys in `taking`, whose
    /// eviction is idempotent and completes every delivery taken with it.
    fn receive(&self, event: &EventV1, taking: &mut Taking) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-1
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-2
        if *event.event_type() != CACHE_INVALIDATE_EVENT_TYPE {
            debug!(event_id = %event.id(), "event of another type ignored");
            return;
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-2
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-17
        // At receipt, before the malformed drop: from the envelope's
        // `occurred_at` to now, so a dropped delivery is observed too.
        self.metrics.publish_to_deliver(
            "cache_invalidate",
            (chrono::Utc::now() - event.occurred_at())
                .to_std()
                .unwrap_or(Duration::ZERO),
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-17
        let span = info_span!(
            "cache_invalidate.consume",
            change_set_id = tracing::field::Empty,
            tenant = tracing::field::Empty,
        );
        if let Some(traceparent) = event.trace_parent()
            && let Ok(value) = http::HeaderValue::from_str(traceparent)
        {
            let mut headers = http::HeaderMap::new();
            headers.insert(toolkit_trace_context::TRACEPARENT, value);
            toolkit_trace_context::set_parent_from_headers(&span, &headers);
        }
        for field in ["change_set_id", "tenant"] {
            if let Some(value) = event
                .data()
                .and_then(|data| data.get(field))
                .and_then(|v| v.as_str())
            {
                span.record(field, value);
            }
        }
        if let Some(keys) = span.in_scope(|| decode_cache_invalidation(event.id(), event.data())) {
            taking.deliveries.push((span, keys));
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-1
    }
}

impl GroupSpec for CacheInvalidationConsumer {
    /// The subscription has one fixed interest.
    type Mark = ();
    type Taking = Taking;
    const DRAIN_LIMIT: usize = DRAIN_LIMIT;
    const LOST: &'static str = "event broker lost; cache invalidation consumer paused";
    /// The cycle starts once resolution found the broker: a broker gone after
    /// that is a loss, held group or not.
    const WARN_UNHELD_LOSS: bool = true;
    const LEAVE_FAILED: &'static str = "leaving the cache topic failed";

    fn broker(&self) -> Option<Arc<dyn EventBrokerApi>> {
        self.client.broker()
    }

    /// The publishing identity, fixed for the cycle.
    fn identity(&self) -> Option<SecurityContext> {
        self.client.publishing_context()
    }

    fn alias(&self) -> String {
        self.client.group_alias()
    }

    fn description(&self) -> String {
        "settings-service replica cache invalidation".to_owned()
    }

    fn timing(&self) -> GroupTiming {
        GroupTiming {
            cleanup_deadline: self.cleanup_deadline,
            liveness: self.liveness,
            healthy: self.healthy,
        }
    }

    fn backoff(&self) -> Backoff {
        self.client.backoff()
    }

    /// Resolve the broker, the root tenant and the cache types first.
    async fn prepare(&self, cancel: &CancellationToken) -> bool {
        self.client.resolve_until_ready(cancel).await
    }

    fn create_refused(&self, reason: &str, _grant_missing: bool) {
        warn!(error = %reason, "consumer group refused by the broker; retrying");
    }

    fn join_refused(&self, reason: &str, _grant_missing: bool) {
        warn!(%reason, "cache topic join refused; retrying on the same group");
    }

    /// The one interest: the `cache_invalidate` type under the root tenant.
    fn interests(&self) -> Result<(Vec<SubscriptionInterest>, ()), String> {
        let root = self
            .client
            .root_tenant()
            .ok_or_else(|| "the root tenant is not known".to_owned())?;
        let topic = GtsInstanceId::try_new(CACHE_TOPIC_ID).map_err(|e| e.to_string())?;
        let pattern =
            GtsIdPattern::try_new(CACHE_INVALIDATE_EVENT_TYPE).map_err(|e| e.to_string())?;
        let interest = SubscriptionInterest::builder()
            .topic(topic)
            .tenant_id(root)
            .types([pattern])
            .build()
            .map_err(|e| e.to_string())?;
        Ok((vec![interest], ()))
    }

    /// The interest never changes.
    async fn changed((): &mut ()) {
        std::future::pending::<()>().await;
    }

    /// Received into `taking`, never cut short: the eviction is synchronous.
    async fn on_event(
        &self,
        event: &EventV1,
        taking: &mut Taking,
        _cancel: &CancellationToken,
    ) -> bool {
        self.receive(event, taking);
        true
    }

    /// The keys of the deliveries taken together are evicted in one call.
    fn complete(&self, taking: Taking) {
        taking.evict(&self.cache);
    }
}

#[cfg(test)]
#[path = "cache_invalidation_tests.rs"]
mod cache_invalidation_tests;

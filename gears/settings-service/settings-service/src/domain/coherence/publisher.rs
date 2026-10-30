//! The end-of-request publish hook.
//!
//! A request — a value write, a declaration change, a reconcile pass — gathers
//! the tenants it touched in a [`TouchedTenants`] and hands it here once, after
//! its last commit and after the local evictions. The hook builds the
//! broadcasts and publishes them through the broker client; it never fails the
//! request and never waits beyond `publish_timeout_seconds` for a refused one.

use std::sync::Arc;

use event_broker_sdk::Event;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::{
    BroadcastContext, BroadcastMetrics, BroadcastResult, BroadcastSink, BroadcastState,
    ChangeOutcome, RequestBroadcast, TenantKeys, TouchedTenants,
};

/// What the hook reads from the broker client when a request ends. Nothing here
/// awaits: the broker is a hub lookup, the rest is cached state.
pub trait BroadcastGateway: Send + Sync {
    /// The registration and root-tenant state.
    fn state(&self) -> BroadcastState;

    /// The root tenant's id, once learned.
    fn root_tenant(&self) -> Option<Uuid>;

    /// The broker and the publishing context, resolved together once for a
    /// request and held for all of its publishes: `None` when no broker
    /// resolves now. A broker that disappears afterwards is a refusal on the
    /// handle already held, never a false "no broker".
    fn session(&self) -> Option<Box<dyn BroadcastSink>>;

    /// The request's broadcasts: per tenant, in first-touch order, the events
    /// that carry its keys — several when a key set would not fit the broker's
    /// event limit. The envelope and the size accounting are the client's.
    fn broadcasts(&self, touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<Event>;
}

/// A request that built nothing: every attempt counts `skipped`, so it says
/// so at `debug`; nobody is left stale beyond `cache_ttl_seconds`.
fn log_skipped(change_set_id: Uuid, skipped: usize, state: BroadcastState) {
    tracing::debug!(
        %change_set_id,
        skipped,
        types_registered = state.types_registered,
        root_tenant_known = state.root_tenant_known,
        "cache_invalidate not published; peers converge within cache_ttl_seconds"
    );
}

/// One line per request with the counts. The refused publish itself was logged
/// at `warn` once, by the outcome; this summary is `info` when something was
/// sent or failed and `debug` when every broadcast was skipped.
fn log_results(change_set_id: Uuid, built: usize, results: &[BroadcastResult]) {
    let count = |wanted: BroadcastResult| results.iter().filter(|r| **r == wanted).count();
    let (sent, skipped, failed) = (
        count(BroadcastResult::Sent),
        count(BroadcastResult::Skipped),
        count(BroadcastResult::Failed),
    );
    if sent > 0 || failed > 0 {
        tracing::info!(%change_set_id, built, sent, skipped, failed, "cache_invalidate published");
    } else {
        tracing::debug!(%change_set_id, built, skipped, "cache_invalidate not published");
    }
}

/// The correlation id of a request: its `x-request-id` when that is a UUID,
/// otherwise a fresh one, as the audit path mints where none exists.
#[must_use]
pub fn correlation_id(request_id: &str) -> Uuid {
    Uuid::parse_str(request_id).unwrap_or_else(|_| Uuid::new_v4())
}

/// The end-of-request publish hook.
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-publish:p1
#[domain_model]
pub struct CoherencePublisher {
    gateway: Arc<dyn BroadcastGateway>,
    metrics: Arc<dyn BroadcastMetrics>,
}

impl CoherencePublisher {
    /// A hook over the broker client's view and the broadcast counter.
    #[must_use]
    pub fn new(gateway: Arc<dyn BroadcastGateway>, metrics: Arc<dyn BroadcastMetrics>) -> Self {
        Self { gateway, metrics }
    }

    /// The end of a value-writing request, run inside the request's span: the
    /// outcomes of its changes, in the order it evaluated them, gathered and
    /// published under its change set.
    pub async fn publish_value_writes(
        &self,
        outcomes: impl IntoIterator<Item = ChangeOutcome>,
        change_set_id: Uuid,
    ) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-1
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-2
        let mut touched = TouchedTenants::new();
        touched.record_all(outcomes);
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-2
        self.publish_request(
            &touched,
            change_set_id,
            toolkit_trace_context::current_traceparent(),
        )
        .await;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-1
    }

    /// The end of a request or reconcile pass that retired, reactivated,
    /// evolved or reclassified declarations: one committed change at the root
    /// tenant per key, under the request's correlation id or the id the pass
    /// minted once. A request's call carries the request's trace context in the
    /// envelope's `trace_parent`; the reconcile pass has no request, so there
    /// is no active span and the field is naturally absent.
    pub async fn publish_declaration_changes(
        &self,
        keys: impl IntoIterator<Item = String>,
        change_set_id: Uuid,
    ) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-9
        let mut touched = TouchedTenants::new();
        touched.record_all(
            keys.into_iter()
                .map(|key| ChangeOutcome::declaration_change(&key)),
        );
        self.publish_request(
            &touched,
            change_set_id,
            toolkit_trace_context::current_traceparent(),
        )
        .await;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-9
    }

    /// Publish what `touched` gathered, once per tenant touched, under
    /// `change_set_id`. Returns when every publish has been counted; the
    /// request's own answer never depends on it.
    pub async fn publish_request(
        &self,
        touched: &TouchedTenants,
        change_set_id: Uuid,
        trace_parent: Option<String>,
    ) {
        // A request that committed nothing publishes nothing and counts nothing.
        if touched.is_empty() {
            return;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-5
        let state = self.gateway.state();
        let Some(root) = self.gateway.root_tenant() else {
            // Nothing is built: the root tenant's id is the envelope's tenant.
            RequestBroadcast::new(None, self.metrics.as_ref(), state, change_set_id)
                .skip(touched.touched_count());
            log_skipped(change_set_id, touched.touched_count(), state);
            return;
        };
        // The broker and the publishing context, taken once for the request.
        let session = self.gateway.session();
        let mut request = RequestBroadcast::new(
            session.as_deref(),
            self.metrics.as_ref(),
            state,
            change_set_id,
        );
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-5
        let ctx = BroadcastContext {
            root_tenant: root,
            change_set_id,
            trace_parent,
        };
        // One body per tenant touched, in first-touch order, wrapped in the
        // broker-native envelope and split where a key set would not fit. The
        // events are built whatever the broker's state: with no broker or the
        // types unregistered each is counted `skipped` by the outcome.
        let events = self.gateway.broadcasts(&touched.resolve(root), &ctx);
        let built = events.len();
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-6
        let results = request.publish_all(events).await;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-6
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-8
        log_results(change_set_id, built, &results);
        // The request answers its caller with its own outcomes, whatever the
        // broadcasts' were; this hook returns nothing to it.
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-publish:p1:inst-rc-publish-8
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod publisher_tests;

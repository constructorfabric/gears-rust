//! Event-broker adapter for the [`AuditSink`] port (`cpt-cf-credstore-nfr-audit`).
//!
//! Every audit record becomes one event of the credstore secret-audit event
//! type, published to the credstore audit topic through the platform
//! `EventBrokerApi`. The adapter owns the best-effort contract: the broker
//! client is resolved lazily on every event (the broker is a non-blocking
//! dependency and may be absent or start later), each publish is bounded by a
//! timeout, and every failure is reduced to "log an error without the secret
//! and count `audit_publish_failed`". `record` never fails and never panics.
//!
//! Topic and event type are declared through the types-registry at gear init
//! ([`register_audit_types`]): the topic as an instance of the broker's topic
//! base type, the event type as a derived schema of its event base type, the
//! way the broker's design prescribes for an owning gear.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::CREDENTIAL_RESOURCE_TYPE;
use event_broker_sdk::gts::derived_event_type_schema;
use event_broker_sdk::{Event, EventBrokerApi, GtsTypeId};
use serde_json::json;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::ports::audit::{AuditEvent, AuditSink};
use crate::domain::ports::metrics::CredStoreMetricsPort;

/// Topic every credstore audit event is published to.
pub const AUDIT_TOPIC_ID: &str = gts_id!("cf.core.events.topic.v1~cf.core.credstore.audit.v1");

/// Event type of a credstore secret audit event.
pub const SECRET_AUDIT_EVENT_TYPE_ID: &str =
    gts_id!("cf.core.events.event.v1~cf.core.credstore.secret_audit.v1~");

/// `source` stamped on every audit event.
const AUDIT_SOURCE: &str = "credstore";

/// Principal credstore publishes audit events as. The caller's own context is
/// not reused: a caller authorized to read or write a secret need not be
/// authorized to publish to the broker, and audit must not depend on that.
const AUDIT_ACTOR: Uuid = uuid::uuid!("00000000-0000-cf01-0000-637265647374");

/// `subject_type` of the actor context.
const AUDIT_ACTOR_TYPE: &str = "credstore.system";

/// Upper bound on one publish. Audit never delays a read or a write longer.
pub const DEFAULT_PUBLISH_TIMEOUT: Duration = Duration::from_millis(500);

/// Yields the broker client if one is currently registered.
pub type BrokerResolver = Arc<dyn Fn() -> Option<Arc<dyn EventBrokerApi>> + Send + Sync>;

/// [`AuditSink`] publishing through the platform event broker.
pub struct EventBrokerAuditSink {
    resolve: BrokerResolver,
    metrics: Arc<dyn CredStoreMetricsPort>,
    timeout: Duration,
    absent_logged: AtomicBool,
}

impl EventBrokerAuditSink {
    #[must_use]
    pub fn new(
        resolve: BrokerResolver,
        metrics: Arc<dyn CredStoreMetricsPort>,
        timeout: Duration,
    ) -> Self {
        Self {
            resolve,
            metrics,
            timeout,
            absent_logged: AtomicBool::new(false),
        }
    }

    fn failed(&self) {
        self.metrics.audit_publish_failed();
    }

    fn wire_event(event: &AuditEvent) -> Event {
        Event {
            id: Uuid::new_v4(),
            type_id: GtsTypeId::new(SECRET_AUDIT_EVENT_TYPE_ID),
            tenant_id: event.tenant_id,
            source: AUDIT_SOURCE.to_owned(),
            subject: event.reference.clone(),
            subject_type: GtsTypeId::new(CREDENTIAL_RESOURCE_TYPE),
            occurred_at: chrono::Utc::now(),
            trace_parent: None,
            data: Some(json!({
                "subject_id": event.subject_id,
                "reference": event.reference,
                "secret_type": event.secret_type,
                "operation": event.operation.as_str(),
                "outcome": event.outcome.as_str(),
            })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        }
    }
}

#[async_trait]
impl AuditSink for EventBrokerAuditSink {
    async fn record(&self, event: AuditEvent) {
        let Some(broker) = (self.resolve)() else {
            // Logged once: an absent broker is a deployment fact, not a
            // per-event incident. Every dropped event is still counted.
            if !self.absent_logged.swap(true, Ordering::Relaxed) {
                tracing::error!(
                    target: "credstore.audit",
                    "event broker is not available; secret audit events are dropped and counted \
                     in audit_publish_failed"
                );
            }
            self.failed();
            return;
        };
        let Ok(ctx) = SecurityContext::builder()
            .subject_id(AUDIT_ACTOR)
            .subject_type(AUDIT_ACTOR_TYPE)
            .subject_tenant_id(event.tenant_id)
            .build()
        else {
            tracing::error!(target: "credstore.audit", "audit security context could not be built");
            self.failed();
            return;
        };

        let wire = Self::wire_event(&event);
        match tokio::time::timeout(self.timeout, broker.publish(&ctx, &wire)).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::error!(
                    target: "credstore.audit",
                    operation = event.operation.as_str(),
                    outcome = event.outcome.as_str(),
                    error = %err,
                    "event broker rejected a secret audit event"
                );
                self.failed();
            }
            Err(_) => {
                tracing::error!(
                    target: "credstore.audit",
                    operation = event.operation.as_str(),
                    outcome = event.outcome.as_str(),
                    timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
                    "publishing a secret audit event timed out"
                );
                self.failed();
            }
        }
    }
}

/// Payload contract of a secret audit event: identifiers and labels only.
fn audit_data_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["subject_id", "reference", "secret_type", "operation", "outcome"],
        "properties": {
            "subject_id": { "type": "string", "format": "uuid" },
            "reference": { "type": "string" },
            "secret_type": { "type": "string" },
            "operation": { "enum": ["read", "create", "replace", "remove", "delete"] },
            "outcome": { "enum": ["success", "failure"] },
        },
    })
}

/// The topic instance document registered in the types-registry.
fn audit_topic() -> serde_json::Value {
    json!({
        "id": AUDIT_TOPIC_ID,
        "description": "Credstore audit stream: one event per secret read or secret write, \
                        naming subject, tenant, reference, type, operation and outcome; never \
                        the secret.",
    })
}

/// The derived event-type schema registered in the types-registry.
fn audit_event_type() -> serde_json::Value {
    derived_event_type_schema(
        SECRET_AUDIT_EVENT_TYPE_ID,
        AUDIT_TOPIC_ID,
        audit_data_schema(),
        &[CREDENTIAL_RESOURCE_TYPE],
    )
}

/// Declare the audit topic and event type in the types-registry.
///
/// # Errors
///
/// Returns `Err` when the registry call fails or rejects either document. The
/// caller treats this as non-fatal: publishing then fails per event and is
/// counted, it never stops the gear from starting.
pub async fn register_audit_types(registry: &dyn TypesRegistryClient) -> anyhow::Result<()> {
    let results = registry
        .register_instances(vec![audit_topic()])
        .await
        .map_err(|e| anyhow::anyhow!("registering the audit topic failed: {e}"))?;
    first_error(results, "audit topic")?;
    let results = registry
        .register_type_schemas(vec![audit_event_type()])
        .await
        .map_err(|e| anyhow::anyhow!("registering the audit event type failed: {e}"))?;
    first_error(results, "audit event type")
}

fn first_error(results: Vec<RegisterResult>, what: &str) -> anyhow::Result<()> {
    for result in results {
        if let RegisterResult::Err { gts_id, error } = result {
            return Err(anyhow::anyhow!(
                "registering the {what} ({}) was rejected: {error}",
                gts_id.as_deref().unwrap_or("unknown id")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "audit_tests.rs"]
pub mod tests;

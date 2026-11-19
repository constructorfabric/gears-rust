// Created: 2026-10-02 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
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
//! Topic and event type are declared in Rust, next to the sink, through the
//! crate's `toolkit_gts::local` collector: the topic as an instance of the
//! broker's topic base type ([`TopicV1`]), the event type as a derived schema
//! of its event base type ([`EventV1`]), the way the broker's design
//! prescribes for an owning gear. The gear publishes both with its other
//! declarations before `init` (`gts_publication`), so nothing here talks to
//! the registry.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::CREDENTIAL_RESOURCE_TYPE;
use event_broker_sdk::gts::{EventV1, EventV1Traits, TopicV1};
use event_broker_sdk::{Ack, EventBrokerApi, EventBrokerError, GtsTypeId};
use gts::GtsSchema;
use serde_json::json;
use toolkit_gts::gts_id;
use toolkit_gts::local::{gts_instance, gts_type_schema};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::ports::audit::{AuditEvent, AuditSink};
use crate::domain::ports::metrics::CredStoreMetricsPort;

/// Topic every credstore audit event is published to.
pub const AUDIT_TOPIC_ID: &str = gts_id!("cf.core.events.topic.v1~cf.core.credstore.audit.v1");

/// Event type of a credstore secret audit event.
pub const SECRET_AUDIT_EVENT_TYPE_ID: &str = <SecretAuditEventV1 as GtsSchema>::TYPE_ID;

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

    fn wire_event(event: &AuditEvent) -> Result<EventV1, EventBrokerError> {
        EventV1::builder_for(
            GtsTypeId::new(SECRET_AUDIT_EVENT_TYPE_ID),
            json!({
                "subject_id": event.subject_id,
                "reference": event.reference,
                "secret_type": event.secret_type,
                "operation": event.operation.as_str(),
                "outcome": event.outcome.as_str(),
            }),
        )
        .subject_tenant(event.tenant_id)
        .source(AUDIT_SOURCE)
        .subject(event.reference.clone())
        .subject_type_of(GtsTypeId::new(CREDENTIAL_RESOURCE_TYPE))
        .build()
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

        let wire = match Self::wire_event(&event) {
            Ok(wire) => wire,
            Err(err) => {
                tracing::error!(
                    target: "credstore.audit",
                    operation = event.operation.as_str(),
                    outcome = event.outcome.as_str(),
                    error = %err,
                    "a secret audit event could not be built"
                );
                self.failed();
                return;
            }
        };
        // Audit is best-effort and never waits for the event to be persisted.
        match tokio::time::timeout(self.timeout, broker.publish(&ctx, &wire, Ack::Accepted)).await {
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

gts_instance! {
    TopicV1 {
        id: gts_id!("cf.core.events.topic.v1~cf.core.credstore.audit.v1"),
        description: "Credstore audit stream: one event per secret read or secret write, naming subject, tenant, reference, type, operation and outcome; never the secret.".to_owned(),
        retention: None,
    }
}

/// Declaration of the secret-audit event type under the broker's event base,
/// on [`AUDIT_TOPIC_ID`] with subject type [`CREDENTIAL_RESOURCE_TYPE`]; its
/// fields are the event's `data` contract: identifiers and labels only, never
/// the secret. The struct only declares the schema, the payload that is sent
/// is built in `EventBrokerAuditSink::wire_event`.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~cf.core.credstore.secret_audit.v1~"),
    description = "Credstore secret audit event: one event per secret read or secret write, naming subject, reference, type, operation and outcome; never the secret.",
    properties = "subject_id,reference,secret_type,operation,outcome",
    traits = EventV1Traits::builder()
        .with_topic(AUDIT_TOPIC_ID)
        .with_allowed_subject_types([CREDENTIAL_RESOURCE_TYPE])
        .build(),
)]
pub struct SecretAuditEventV1 {
    /// Subject that performed the operation.
    pub subject_id: Uuid,
    /// Reference of the audited record.
    pub reference: String,
    /// GTS type id of the credential.
    pub secret_type: String,
    /// What was done: `read`, `create`, `replace`, `remove` or `delete`.
    #[schemars(with = "String", extend("enum" = ["read", "create", "replace", "remove", "delete"]))]
    pub operation: String,
    /// How it ended: `success` or `failure`.
    #[schemars(with = "String", extend("enum" = ["success", "failure"]))]
    pub outcome: String,
}

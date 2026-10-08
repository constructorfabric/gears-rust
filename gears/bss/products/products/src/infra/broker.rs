//! Typed registry events, broker binding and the transactional outbox sink.
//! @cpt-dod:cpt-cf-bss-products-dod-sku-changed-payload:p1
use event_broker_sdk::{ProducerOutbox, ProducerOutboxHandle, TypedEvent};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub const TOPIC: &str = "gts.cf.core.events.topic.v1~cf.bss.products.catalog.v1";
/// Every products event type this producer declares.
const EVENT_TYPE_WILDCARD: &str = "gts.cf.core.events.event.v1~cf.bss.products.*";
pub const SOURCE: &str = "bss-products";
pub const SKU_SUBJECT_TYPE: &str = "gts.cf.core.events.subject.v1~cf.bss.products.sku.v1~";

/// Approval-unit event subject; SKU events retain the existing subject type.
pub const APPROVAL_UNIT_SUBJECT_TYPE: &str =
    "gts.cf.core.events.subject.v1~cf.bss.products.approval_unit.v1~";

/// A SKU was published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkuPublished {
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub published_version: i64,
    pub actor_ref: Uuid,
}
impl TypedEvent for SkuPublished {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~cf.bss.products.sku_published.v1~";
    const SUBJECT_TYPE: &'static str = SKU_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.sku_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        crate::infra::events::traceparent().map(Cow::Owned)
    }
}

/// Approved business content took effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkuChanged {
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub changed: Vec<String>,
    #[serde(with = "crate::infra::serde_date")]
    pub effective_from: time::Date,
    pub published_version: i64,
    pub actor_ref: Uuid,
}
impl TypedEvent for SkuChanged {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~cf.bss.products.sku_changed.v1~";
    const SUBJECT_TYPE: &'static str = SKU_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.sku_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        crate::infra::events::traceparent().map(Cow::Owned)
    }
}

/// A SKU completed retirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkuRetired {
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub actor_ref: Uuid,
}
impl TypedEvent for SkuRetired {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~cf.bss.products.sku_retired.v1~";
    const SUBJECT_TYPE: &'static str = SKU_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.sku_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        crate::infra::events::traceparent().map(Cow::Owned)
    }
}

/// A terminal decision, including withdrawal or quorum-zero approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalUnitDecided {
    pub tenant_id: Uuid,
    pub unit_id: Uuid,
    pub kind: String,
    pub state: String,
    pub generation: i32,
    pub actors: Vec<Uuid>,
}
impl TypedEvent for ApprovalUnitDecided {
    const TYPE_ID: &'static str =
        "gts.cf.core.events.event.v1~cf.bss.products.approval_unit_decided.v1~";
    const SUBJECT_TYPE: &'static str = APPROVAL_UNIT_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.unit_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        crate::infra::events::traceparent().map(Cow::Owned)
    }
}

/// An operator released a reservation; its owner must reconcile the referenced object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReferenceForceReleased {
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub reference_id: Uuid,
    pub owner: String,
    pub kind: String,
    pub ref_id: Uuid,
    pub actor_ref: Uuid,
    pub reason: String,
}
impl TypedEvent for ReferenceForceReleased {
    const TYPE_ID: &'static str =
        "gts.cf.core.events.event.v1~cf.bss.products.reference_force_released.v1~";
    const SUBJECT_TYPE: &'static str = SKU_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.sku_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
    fn trace_parent(&self) -> Option<Cow<'_, str>> {
        crate::infra::events::traceparent().map(Cow::Owned)
    }
}

/// The producer's own identity. A builder refusal fails the boot through `bind_producer`'s `?`,
/// with its context, instead of a panic (RS-34).
fn producer_system_actor() -> anyhow::Result<SecurityContext> {
    /// Hand-picked, version nibble `0`. `62 73 73 70` is `bssp`.
    const PRODUCER_ACTOR: Uuid = uuid::uuid!("00000000-0000-0f01-0000-627373702d70");
    /// The subject type an `AuthZ` policy may key on to route this gear's
    /// producer traffic separately from tenant traffic.
    const SUBJECT_TYPE: &str = "bss-products.system";

    tracing::info!(
        target: "bss_products.system_actor",
        site = "broker_producer",
        "bss-products system actor constructed"
    );
    SecurityContext::builder()
        .subject_id(PRODUCER_ACTOR)
        .subject_type(SUBJECT_TYPE)
        .subject_tenant_id(Uuid::nil())
        .build()
        .map_err(|e| anyhow::anyhow!("bss-products: the producer's system actor: {e}"))
}

/// The pipeline a route enqueues into.
#[derive(Clone)]
pub enum EventSink {
    /// **P-D-199's shape.** The toolkit outbox whose processor is the SDK's
    /// producer; the envelope is the broker's and the id is the SDK's.
    Broker(Box<ProducerOutbox>),
    /// The interim envelope on this gear's own toolkit queue, held by
    /// [`crate::infra::events::PendingBrokerProducer`] and never delivered.
    ///
    /// The fallback for a deployment whose `ClientHub` carries no
    /// `EventBrokerApi`. Safe by construction: the holding processor answers
    /// `Retry` to every message, so nothing is ever reported delivered and the
    /// `dod-outbox-eventing` clause *"emission success MUST NOT be reported
    /// before the event is durably accepted"* holds trivially — nothing is
    /// reported at all.
    Interim(Arc<toolkit_db::outbox::Outbox>),
}

pub(crate) async fn bind_producer(
    hub: &toolkit::client_hub::ClientHub,
    db: toolkit_db::Db,
    table_prefix: &str,
    partitions: toolkit_db::outbox::Partitions,
) -> anyhow::Result<Option<(EventSink, ProducerOutboxHandle)>> {
    let broker = match hub.get::<dyn event_broker_sdk::EventBrokerApi>() {
        Ok(broker) => broker,
        // The configured-out case, and the only silent one.
        Err(toolkit::client_hub::ClientHubError::NotFound { .. }) => return Ok(None),
        // A registration that is there but wrong is an operator's mistake, not a
        // deployment without a broker. Collapsing the two — which a
        // `let ... else` does — is the degradation this function's doc forbids.
        Err(other) => {
            return Err(anyhow::Error::new(other)
                .context("bss-products: the ClientHub holds an unusable EventBrokerApi"));
        }
    };

    let producer = event_broker_sdk::DbProducer::builder()
        .broker(broker)
        .db(db.clone())
        .security_context(producer_system_actor()?)
        .identity(
            event_broker_sdk::ProducerIdentity::new()
                .source(SOURCE)
                // **No version here.** `ProducerRegistration::validate_matches`
                // compares the stored `client_agent` against the supplied one and
                // returns `InvalidProducerOptions` on any difference — before
                // `on_missing`/`on_unknown` are consulted, so neither policy is an
                // escape. A version in this string would make an ordinary
                // `version = "0.1.0"` in the manifest an unbootable gear against
                // an existing registration row, recoverable only by hand-editing
                // the SDK's table. The crate is at `0.0.0` today, so the version
                // carries no diagnostic value either.
                .client_agent(SOURCE),
        )
        // **Monotonic, not chained**: the managed monotonic mode of P-D-47
        // (backup `3a38f0b28`). The toolkit outbox's `seq` is the durable local
        // sequence the chain's `meta.sequence` is built from, write-only, for
        // ingest-side dedup.
        .deduplication(
            event_broker_sdk::DbDeduplication::managed(event_broker_sdk::ProducerMode::Monotonic)
                .key(SOURCE)
                .on_missing(event_broker_sdk::MissingProducerRegistration::RegisterNew)
                .on_unknown(event_broker_sdk::UnknownProducerRegistration::RegisterNew)
                .build()?,
        )
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE_WILDCARD])
        .prepare_all()
        .await?;

    // Resolve schemas before any business transaction enqueues an event.
    producer.prepare::<SkuPublished>().await?;
    producer.prepare::<SkuChanged>().await?;
    producer.prepare::<SkuRetired>().await?;
    producer.prepare::<ApprovalUnitDecided>().await?;
    producer.prepare::<ReferenceForceReleased>().await?;

    // The queue name is the table prefix's own, so the producer's queue and
    // this gear's tables are named from one constant.
    // **`QUEUE_NAME`, not the table prefix.** `outbox_queue`'s first argument is
    // the *queue name*, and passing the prefix here gave the two arms two
    // different queue names over one table family — so rows an interim boot had
    // accumulated under `QUEUE_NAME` had no processor once a broker appeared,
    // and stopped moving. They are not lost (`Dialect::vacuum_cleanup` is scoped
    // by `partition_id`, which is per queue), but they are stranded, and the
    // boot is green either way. One name across both arms is what makes an arm
    // switch survivable.
    let queue = producer.outbox_queue(crate::infra::events::QUEUE_NAME, partitions)?;
    let handle = queue
        .start(toolkit_db::outbox::Outbox::builder(db).table_prefix(table_prefix)?)
        .await?;
    let sink = EventSink::Broker(Box::new(handle.outbox().clone()));
    Ok(Some((sink, handle)))
}

#[cfg(test)]
#[path = "broker_tests.rs"]
mod broker_tests;
#[cfg(test)]
#[path = "broker_wake_tests.rs"]
mod broker_wake_tests;

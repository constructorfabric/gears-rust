//! The broker SDK's producer over pricing's outbox queue (D-400; Products' `bind_producer`).
//!
//! Where the `ClientHub` carries an `EventBrokerApi`, pricing binds a `DbProducer` as the
//! queue's processor and events are enqueued in its envelope; where it carries none, the
//! queue keeps the holding processor ([`super::events::PendingProducer`]) and nothing is ever
//! reported delivered. A registration that is present but unusable fails the boot.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-outbox-toolkit:p1
use super::{
    events::{
        ApprovalUnitDecided, EventSink, PlanRevisionPublished, PricesPublished, QUEUE, SOURCE,
        TOPIC,
    },
    reference_events::{PlanReferenceLost, PriceBookEntryReferenceLost},
};
use anyhow::Context;
use bss_products_sdk::PRICING_SYSTEM_ACTOR;
use event_broker_sdk::ProducerOutboxHandle;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Every pricing event type this producer declares.
const EVENT_TYPE_WILDCARD: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.*";

/// The identity the producer presents to the broker: pricing's system actor, no tenant.
fn producer_system_actor() -> anyhow::Result<SecurityContext> {
    SecurityContext::builder()
        .subject_id(PRICING_SYSTEM_ACTOR)
        .subject_type("bss-pricing.system")
        .subject_tenant_id(Uuid::nil())
        .build()
        .context("bss-pricing: the producer system actor could not be built")
}

/// Bind the broker SDK's producer to pricing's outbox queue, or `None` without a broker.
/// # Errors
/// An `EventBrokerApi` registration that cannot be read, a producer the broker refuses, or
/// a queue that does not start: a half-configured broker must not degrade quietly into an
/// envelope no consumer reads.
pub async fn bind_producer(
    hub: &toolkit::ClientHub,
    db: toolkit_db::Db,
    table_prefix: &str,
    partitions: toolkit_db::outbox::Partitions,
) -> anyhow::Result<Option<(EventSink, ProducerOutboxHandle)>> {
    let broker = match hub.get::<dyn event_broker_sdk::EventBrokerApi>() {
        Ok(broker) => broker,
        // The configured-out case, and the only silent one.
        Err(toolkit::client_hub::ClientHubError::NotFound { .. }) => return Ok(None),
        Err(other) => {
            return Err(anyhow::Error::new(other)
                .context("bss-pricing: the ClientHub holds an unusable EventBrokerApi"));
        }
    };
    let producer = event_broker_sdk::DbProducer::builder()
        .broker(broker)
        .db(db.clone())
        .security_context(producer_system_actor()?)
        // No version in the agent: a stored registration compares it verbatim.
        .identity(
            event_broker_sdk::ProducerIdentity::new()
                .source(SOURCE)
                .client_agent(SOURCE),
        )
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
    // Resolve every schema before a business transaction enqueues an event.
    producer.prepare::<PriceBookEntryReferenceLost>().await?;
    producer.prepare::<PlanReferenceLost>().await?;
    producer.prepare::<PricesPublished>().await?;
    producer.prepare::<PlanRevisionPublished>().await?;
    producer.prepare::<ApprovalUnitDecided>().await?;
    // One queue name across both processors, so an arm switch strands no row.
    let queue = producer.outbox_queue(QUEUE, partitions)?;
    let handle = queue
        .start(toolkit_db::outbox::Outbox::builder(db).table_prefix(table_prefix)?)
        .await?;
    let sink = EventSink::Broker(Box::new(handle.outbox().clone()));
    Ok(Some((sink, handle)))
}

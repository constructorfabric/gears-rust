//! Pricing's typed events (D-400): broker `TypedEvent` payloads written through the toolkit
//! outbox on the caller's transaction, in the broker's producer-outbox envelope.
//!
//! The runner a writer passes is the transaction of the act the event reports, so a rollback
//! erases the event with the act, and a failed outbox insert fails the act. Where a broker is
//! registered, the queue's processor is the broker SDK's producer ([`super::broker`]);
//! otherwise it is [`PendingProducer`], which holds every envelope, so nothing is reported
//! delivered before a broker exists (Products' interim pattern).
//!
//! An enqueue wakes the outbox's sequencer only after the transaction commits (D-455): the
//! writer enqueues through the transaction's [`TxOutbox`], and [`transaction`] fires what the
//! committed attempt enqueued.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-outbox-same-tx:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-events-typed-outbox:p1
use super::storage::RepoError;
use event_broker_sdk::TypedEvent;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};
use toolkit_db::{
    Db, DbError, DbTx,
    outbox::Wake,
    secure::{DBRunner, TxConfig},
};
use toolkit_gts::gts_id;
use uuid::Uuid;

/// The toolkit outbox table family of this gear.
pub const OUTBOX_TABLE_PREFIX: &str = "bss_pricing_outbox";
/// The one queue every pricing event is enqueued on.
pub const QUEUE: &str = "bss_pricing_events";
/// The broker topic pricing publishes to.
pub const TOPIC: &str = gts_id!("cf.core.events.topic.v1~cf.bss.pricing.catalog.v1");
/// The producer source of every pricing event.
pub const SOURCE: &str = "bss-pricing";
/// `PricesPublished` is about a book.
pub const PRICE_BOOK_SUBJECT_TYPE: &str =
    gts_id!("cf.core.events.subject.v1~cf.bss.pricing.price_book.v1~");
/// `PlanRevisionPublished` is about a plan: which revision it sells moved.
pub const PLAN_SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~cf.bss.pricing.plan.v1~");
/// `ApprovalUnitDecided` is about a unit.
pub const APPROVAL_UNIT_SUBJECT_TYPE: &str =
    gts_id!("cf.core.events.subject.v1~cf.bss.pricing.approval_unit.v1~");
const CONTENT_TYPE: &str =
    "application/vnd.constructorfabric.event-broker.producer-outbox+json;version=1";

/// Where an event is enqueued: the broker SDK's producer outbox, or the holding queue.
#[derive(Clone)]
pub enum EventSink {
    /// The bound `DbProducer`'s outbox: its envelope, its processor.
    Broker(Box<event_broker_sdk::ProducerOutbox>),
    /// The interim envelope on pricing's queue, held by [`PendingProducer`].
    Interim(std::sync::Arc<toolkit_db::outbox::Outbox>),
}
/// The event sink as one transaction sees it (D-455). An event enqueued through it leaves its
/// outbox [`Wake`] here instead of waking the sequencer at once: a sequencer woken before the
/// commit finds nothing committed, and the row then waits for the cold reconciler. Whoever opens
/// the transaction settles the handle when the transaction ends: [`fire`](Self::fire) once it has
/// committed, [`discard`](Self::discard) when it rolls back. [`transaction`] does both, and every
/// door, job and approval callback that enqueues runs in it. Clones share one handle.
#[derive(Clone)]
pub struct TxOutbox {
    sink: EventSink,
    wake: Arc<Mutex<Wake>>,
}
impl TxOutbox {
    /// A handle over `sink` that holds no wake yet.
    #[must_use]
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            wake: Arc::new(Mutex::new(Wake::empty())),
        }
    }
    fn add(&self, wake: Wake) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) += wake;
    }
    fn take(&self) -> Wake {
        std::mem::replace(
            &mut *self.wake.lock().unwrap_or_else(PoisonError::into_inner),
            Wake::empty(),
        )
    }
    /// Wake the sequencers for every event enqueued through this handle. Call it only once the
    /// transaction has committed.
    pub fn fire(&self) {
        self.take().fire();
    }
    /// Drop every enqueued event's wake unfired: the transaction rolled back, and its rows with
    /// it, so there is nothing to wake a sequencer for.
    pub fn discard(&self) {
        self.take().discard();
    }
}

/// Run `work` in `db`'s retrying transaction with a [`TxOutbox`] over `sink`, and wake the
/// outbox's sequencers for what it enqueued only once it has committed (D-455). An attempt that
/// the retry repeats has rolled back, so the next attempt first discards its wakes; a transaction
/// that fails discards the rest. This is the toolkit's `outbox::in_transaction` contract on the
/// retrying transaction the gear's doors run in.
/// # Errors
/// The transaction's error after its retries, as [`Db::transaction_with_retry`] answers it.
pub async fn transaction<T, E, X, F>(
    db: &Db,
    sink: &EventSink,
    config: TxConfig,
    extract_db_err: X,
    work: F,
) -> Result<T, E>
where
    E: From<DbError> + Send + 'static,
    T: Send + 'static,
    X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
    F: for<'a> FnMut(
            &'a DbTx<'a>,
            TxOutbox,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
        + Send,
{
    transaction_with_attempts(
        db,
        sink,
        config,
        toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS,
        extract_db_err,
        work,
    )
    .await
}

/// The event transaction with an explicit budget. Detached-capture callers use one attempt
/// here and own the shared capture/transaction retry budget outside every database transaction.
/// # Errors
/// The original typed refusal or driver error after rollback and discarding event wakes.
pub async fn transaction_with_attempts<T, E, X, F>(
    db: &Db,
    sink: &EventSink,
    config: TxConfig,
    attempts: u32,
    extract_db_err: X,
    mut work: F,
) -> Result<T, E>
where
    E: From<DbError> + Send + 'static,
    T: Send + 'static,
    X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
    F: for<'a> FnMut(
            &'a DbTx<'a>,
            TxOutbox,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
        + Send,
{
    let outbox = TxOutbox::new(sink.clone());
    let attempt = outbox.clone();
    let result = db
        .transaction_with_retry_max(config, attempts, extract_db_err, move |tx| {
            attempt.discard();
            work(tx, attempt.clone())
        })
        .await;
    if result.is_ok() {
        outbox.fire();
    } else {
        outbox.discard();
    }
    result
}

/// Holds every envelope while no broker producer is bound.
pub struct PendingProducer;
#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for PendingProducer {
    async fn handle(
        &self,
        msg: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        tracing::debug!(
            queue = QUEUE,
            payload_type = %msg.payload_type,
            "bss-pricing: no broker producer is bound; holding the message in the queue"
        );
        toolkit_db::outbox::MessageResult::Retry
    }
}

// @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1
/// One price whose window or state a `prices` unit's apply changed, as the apply left it: a price
/// the unit approved, a price before it whose end the chain re-closed or re-opened, a price the
/// unit cancelled or ended (D-520, D-521).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPrice {
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
    /// The chain: the dimension value, `None` for the default chain. (A value may itself be
    /// spelled `default`, so the chain is not a string.)
    pub dim_value: Option<String>,
    /// ISO date, inclusive.
    pub effective_from: String,
    /// ISO date, exclusive; `None` is an open tail.
    pub effective_to: Option<String>,
    /// `all` or `new`.
    pub eligibility: String,
    /// The price's stored state after the apply: `approved`, or `cancelled` for a price the unit
    /// cancelled, which is in no chain and never in force (D-520). Additive: an event written
    /// before it has none, and none is not written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A `prices` unit was applied: its prices are approved in the book, and every price whose window
/// or state the apply changed is listed with it (D-520, D-521).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesPublished {
    pub tenant_id: Uuid,
    pub book_id: Uuid,
    pub unit_id: Uuid,
    /// In ascending price id. Never a `cancel` or `end` row: those are records, not prices.
    pub prices: Vec<PublishedPrice>,
    /// The principal whose act applied the unit.
    pub actor_ref: Uuid,
}
impl TypedEvent for PricesPublished {
    const TYPE_ID: &'static str =
        gts_id!("cf.core.events.event.v1~cf.bss.pricing.prices_published.v1~");
    const SUBJECT_TYPE: &'static str = PRICE_BOOK_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.book_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
}
// @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1

// @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-2
/// A `plan_revision` unit was applied: the revision is the plan's published one, the revision
/// published before it (if any) is superseded, and the plan's `published_rev` is its number. A
/// revision approved before its sale date is announced instead when its switch is persisted on that
/// date, once, by the switch job or the door that catches it up (D-449, D-450). Existing
/// subscription pins do not move (D-394).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRevisionPublished {
    pub tenant_id: Uuid,
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    pub rev_no: i32,
    /// The book the revision reads its money from.
    pub book_id: Uuid,
    /// The revision this one superseded; `None` for a plan's first publication.
    pub superseded_revision_id: Option<Uuid>,
    pub unit_id: Uuid,
    /// The principal whose act applied the unit; at a switch, the unit's latest current approver,
    /// or its submitter when no one voted (quorum 0, D-450).
    pub actor_ref: Uuid,
}
impl TypedEvent for PlanRevisionPublished {
    const TYPE_ID: &'static str =
        gts_id!("cf.core.events.event.v1~cf.bss.pricing.plan_revision_published.v1~");
    const SUBJECT_TYPE: &'static str = PLAN_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.plan_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
}
// @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-2

// @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1
/// A unit reached a terminal state: approved (applied), rejected or withdrawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalUnitDecided {
    pub tenant_id: Uuid,
    pub unit_id: Uuid,
    pub kind: String,
    /// The outcome: `approved`, `rejected` or `withdrawn`.
    pub state: String,
    pub generation: i32,
    /// The deciding principals: every current-generation voter and the actor, ascending.
    pub actors: Vec<Uuid>,
}
impl TypedEvent for ApprovalUnitDecided {
    const TYPE_ID: &'static str =
        gts_id!("cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~");
    const SUBJECT_TYPE: &'static str = APPROVAL_UNIT_SUBJECT_TYPE;
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.unit_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
}
// @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1

/// The broker SDK's producer-outbox envelope of `event`, as the interim arm writes it before any
/// broker is bound. Its `producer_mode` is `stateless`, the one mode an envelope written with no
/// producer registration can carry (D-455's note on the interim envelope, whole-branch review
/// PS-21): the SDK's processor refuses a monotonic or chained envelope without a `producer_id`,
/// so a held row in either would never drain once a broker is bound on the same queue; a
/// stateless one drains, published without the producer's sequence. A test deserializes it as
/// `event_broker_sdk::producer::ProducerOutboxEnvelope`, so a change of the SDK's envelope fails
/// the build of that test, not a boot.
/// # Errors
/// Serialization failures.
pub(crate) fn interim_envelope<E: TypedEvent>(
    event: &E,
    now: time::OffsetDateTime,
) -> Result<serde_json::Value, RepoError> {
    let serialize = |e: String| RepoError::Db(format!("{} event: {e}", E::TYPE_ID));
    Ok(serde_json::json!({
        "version": 1,
        "event_id": Uuid::now_v7(),
        "type": E::TYPE_ID,
        "topic": TOPIC,
        "tenant_id": event.tenant_id(),
        "source": E::SOURCE,
        "subject": event.subject(),
        "subject_type": E::SUBJECT_TYPE,
        "occurred_at": now
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| serialize(e.to_string()))?,
        "trace_parent": event.trace_parent(),
        "data": serde_json::to_value(event).map_err(|e| serialize(e.to_string()))?,
        "broker_partition": 0,
        "producer_mode": "stateless",
        "diagnostic_metadata": {"sdk_client_agent": SOURCE},
    }))
}
/// Enqueue one event on the caller's transaction in the broker's producer-outbox envelope. Its
/// wake stays with `outbox` until the transaction ends (D-455).
/// # Errors
/// Serialization failures, and outbox failures with the driver error kept typed so a
/// serializable transaction can retry.
pub async fn enqueue<E: TypedEvent + Clone>(
    outbox: &TxOutbox,
    tx: &(impl DBRunner + Sync),
    event: &E,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-3
    // Neither arm fires its `Wake`: the transaction is still open, and a sequencer woken now
    // would find nothing committed. `outbox` keeps it for the transaction's end.
    let queue = match &outbox.sink {
        EventSink::Interim(queue) => queue,
        // The SDK's enqueue erases the outbox's database error into a string, so a
        // contended insert here fails the act instead of retrying it (as in Products).
        EventSink::Broker(producer) => {
            return producer
                .enqueue(tx, event.clone())
                .await
                .map(|wake| outbox.add(wake))
                .map_err(|e| RepoError::Db(format!("{} event: {e}", E::TYPE_ID)));
        }
    };
    let serialize = |e: String| RepoError::Db(format!("{} event: {e}", E::TYPE_ID));
    let envelope = interim_envelope(event, now)?;
    let record = toolkit_db::outbox::Record::to(QUEUE, 0)
        .payload(
            serde_json::to_vec(&envelope).map_err(|e| serialize(e.to_string()))?,
            CONTENT_TYPE,
        )
        .build()
        .map_err(|e| RepoError::Db(e.to_string()))?;
    queue
        .enqueue(tx, record)
        .await
        .map(|wake| outbox.add(wake))
        .map_err(|e| match e {
            toolkit_db::outbox::OutboxError::Database(source) => RepoError::Driver {
                context: format!("{} event", E::TYPE_ID),
                source,
            },
            other => RepoError::Db(other.to_string()),
        })?;
    // @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-3
    Ok(())
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;

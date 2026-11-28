use crate::api::{JoinRequest, SubscriptionAssignment};
use crate::broker_stub::BrokerStub;
use crate::consumer::{
    BatchHandler, BatchHandlerOutcome, ConsumerBuffering, ConsumerBuilder, ConsumerGroupRef,
    ConsumerHandle, ConsumerListenerSettings, ConsumerProfile, Delivery, EventBatch, EventHandler,
    Fallback, HandlerOutcome, InMemoryOffsetManager,
};
use crate::error::{ConsumerError, EventBrokerError};
use crate::gts::EventV1;
use crate::ids::ConsumerGroupId;
use crate::models::{ConsumerGroup, ConsumerGroupKind, CreateConsumerGroupRequest, EventType};
use gts::{GtsIdPattern, GtsInstanceId};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use toolkit_gts::{GTS_ID_PREFIX, gts_id};
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn ginst(id: impl AsRef<str>) -> GtsInstanceId {
    GtsInstanceId::try_new(id.as_ref()).expect("valid GTS instance id")
}

fn gpat(id: impl AsRef<str>) -> GtsIdPattern {
    GtsIdPattern::try_new(id.as_ref()).expect("valid GTS pattern")
}

struct NoopHandler;

#[async_trait::async_trait]
impl EventHandler<serde_json::Value> for NoopHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

struct NoopBatchHandler;

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for NoopBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        Ok(BatchHandlerOutcome::Success)
    }
}

/// A broker every call of which panics, so a consumer that reaches it fails the test.
struct NoBroker;

impl BrokerStub for NoBroker {}

/// Bounds every await on a consumer start or stop, so a hang fails the test.
const BOUND: Duration = Duration::from_secs(5);

/// The group `RecordingBroker` creates for every create request.
const CREATED_GROUP: ConsumerGroupId = ConsumerGroupId(Uuid::from_u128(0x2301));

/// A broker call `RecordingBroker` received, in the order received.
#[derive(Debug, Clone, PartialEq)]
enum BrokerCall {
    ListEventTypes,
    CreateConsumerGroup {
        client_agent: String,
        description: Option<String>,
    },
    Join {
        group: ConsumerGroupId,
    },
}

/// A broker that records the calls a consumer start makes. It lists no event types,
/// or refuses the lookup as unauthorized, creates `CREATED_GROUP` for every create
/// request, and refuses every JOIN, which ends the slot that sent it.
struct RecordingBroker {
    refuses_type_lookup: bool,
    calls: Mutex<Vec<BrokerCall>>,
}

impl RecordingBroker {
    fn listing_no_event_types() -> Self {
        Self {
            refuses_type_lookup: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn refusing_the_event_type_lookup() -> Self {
        Self {
            refuses_type_lookup: true,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<BrokerCall> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record(&self, call: BrokerCall) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

#[async_trait::async_trait]
impl BrokerStub for RecordingBroker {
    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        self.record(BrokerCall::ListEventTypes);
        if self.refuses_type_lookup {
            return Err(EventBrokerError::Unauthorized {
                detail: "the caller may not read event types".to_owned(),
            });
        }
        Ok(Vec::new())
    }

    async fn create_consumer_group(
        &self,
        _ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        self.record(BrokerCall::CreateConsumerGroup {
            client_agent: req.client_agent,
            description: req.description,
        });
        Ok(ConsumerGroup {
            id: CREATED_GROUP,
            tenant: Uuid::from_u128(0x1402),
            owner_principal: Uuid::from_u128(0x1401),
            kind: ConsumerGroupKind::Anonymous,
            description: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
        })
    }

    async fn join(
        &self,
        _ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        self.record(BrokerCall::Join {
            group: req.consumer_group,
        });
        Err(EventBrokerError::Transport(
            "the broker refuses every JOIN".to_owned(),
        ))
    }
}

/// A context that identifies its caller in process, so `start()` gets past the
/// identity check.
fn identified_context() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1401))
        .subject_tenant_id(Uuid::from_u128(0x1402))
        .build()
        .expect("context with ids builds")
}

#[test]
fn a_consumer_with_a_nil_subject_and_no_token_has_no_identity() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::from_u128(0x1402))
        .build()
        .expect("context with a nil subject builds");
    let builder = ConsumerBuilder::new_unbound().security_context(ctx);

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = builder.require_identity()
    else {
        panic!("a nil subject without a bearer token must be refused");
    };

    assert_eq!(
        detail,
        "the consumer has no identity: set a security context with a subject and a tenant, \
         or one with a bearer token"
    );
}

#[test]
fn a_consumer_with_a_nil_tenant_and_no_token_has_no_identity() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1401))
        .subject_tenant_id(Uuid::nil())
        .build()
        .expect("context with a nil tenant builds");
    let builder = ConsumerBuilder::new_unbound().security_context(ctx);

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = builder.require_identity()
    else {
        panic!("a nil tenant without a bearer token must be refused");
    };

    assert_eq!(
        detail,
        "the consumer has no identity: set a security context with a subject and a tenant, \
         or one with a bearer token"
    );
}

#[test]
fn a_consumer_with_a_token_but_no_ids_has_an_identity() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .bearer_token("bearer-token".to_owned())
        .build()
        .expect("token-only context builds");
    let builder = ConsumerBuilder::new_unbound().security_context(ctx);

    assert!(
        matches!(builder.require_identity(), Ok(())),
        "a bearer token alone identifies the caller over REST"
    );
}

#[test]
fn a_consumer_with_a_subject_and_a_tenant_has_an_identity() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1401))
        .subject_tenant_id(Uuid::from_u128(0x1402))
        .build()
        .expect("context with ids builds");
    let builder = ConsumerBuilder::new_unbound().security_context(ctx);

    assert!(
        matches!(builder.require_identity(), Ok(())),
        "a subject and a tenant identify the caller in process"
    );
}

/// The default context is anonymous; `start()` refuses it before the type cache is
/// primed, so the panicking broker is never called.
#[tokio::test]
async fn a_consumer_without_an_identity_is_refused_before_it_calls_the_broker() {
    let ready = ConsumerBuilder::new(Arc::new(NoBroker))
        .group(ConsumerGroupRef::auto_anonymous("builder-no-identity"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = ready.start().await else {
        panic!("a consumer without an identity must be refused at start");
    };

    assert_eq!(
        detail,
        "the consumer has no identity: set a security context with a subject and a tenant, \
         or one with a bearer token"
    );
}

#[tokio::test]
async fn an_unbound_consumer_with_an_identity_is_refused_for_its_missing_broker() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1401))
        .subject_tenant_id(Uuid::from_u128(0x1402))
        .build()
        .expect("context with ids builds");
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-start"))
        .security_context(ctx)
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let Err(err) = ready.start().await else {
        panic!("unbound builder cannot open subscriptions");
    };

    assert!(
        err.to_string().contains("broker not wired"),
        "unexpected error: {err}"
    );
}

#[test]
fn consumer_builder_accepts_batch_handler_terminal_method() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-batch"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_default_and_routed_handlers() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~example.orders.*"
        )))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_route_only_with_topic_catch_all() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-route-only"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .handler(NoopHandler);
}

#[test]
fn consumer_builders_keep_independent_profiles_and_listener_settings() {
    let low_latency = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-low"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .profile(ConsumerProfile::low_latency())
        .listener_settings(ConsumerListenerSettings {
            timeout: Duration::from_millis(25),
            channel_capacity: 8,
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let high_throughput = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-high"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.payments.x.x.v1"
        ))])
        .profile(ConsumerProfile::high_throughput())
        .buffering(ConsumerBuffering {
            partition_capacity: 1024,
            high_watermark: 900,
            low_watermark: 512,
        })
        .listener_settings(ConsumerListenerSettings {
            timeout: Duration::from_millis(250),
            channel_capacity: 64,
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Latest))
        .handler(NoopHandler);

    let low_settings = low_latency.builder.effective_settings().unwrap();
    let high_settings = high_throughput.builder.effective_settings().unwrap();

    assert_ne!(low_latency.builder.topics, high_throughput.builder.topics);
    assert_ne!(low_settings.batching, high_settings.batching);
    assert_ne!(low_settings.listener, high_settings.listener);
    assert_eq!(high_settings.buffering.partition_capacity, 1024);
}

#[tokio::test]
async fn routed_consumer_rejects_route_outside_subscription_topics() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "builder-routed-invalid-topic",
        ))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.payments.x.x.v1"
        )))
        .handler(NoopHandler);

    let Err(err) = ready.start().await else {
        panic!("route validation must fail");
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("not part of the configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_rejects_duplicate_routes() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed-duplicate"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler);

    let Err(err) = ready.start().await else {
        panic!("duplicate route validation must fail");
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("duplicate consumer route"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_without_default_rejects_incomplete_routes() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "builder-routed-missing-default",
        ))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler);

    let Err(err) = ready.start().await else {
        panic!("route-only consumer without catch-all must fail");
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("without a default handler"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_rejects_missing_subscription_topics() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed-no-topics"))
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .handler(NoopHandler);

    let Err(err) = ready.start().await else {
        panic!("routed consumer without topics must fail");
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string()
            .contains("requires at least one configured topic"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn consumer_handle_exposes_subscription_inspection_and_stop() {
    let handle = ConsumerHandle::from_consumer(super::runtime::Consumer::new());

    assert!(handle.subscriptions().is_empty());
    handle.stop().await.expect("empty handle stop");
}

/// A consumer that names no group joins a new anonymous group created with its
/// client agent.
#[test]
fn a_consumer_without_a_group_joins_an_anonymous_group_created_with_its_client_agent() {
    let builder = ConsumerBuilder::new_unbound().client_agent("orders-worker/1.4.0");

    assert_eq!(
        builder.effective_group(),
        ConsumerGroupRef::AutoAnonymous {
            alias: "orders-worker/1.4.0".to_owned(),
        }
    );
}

/// A group set on the builder is the one the consumer joins, whatever its client
/// agent.
#[test]
fn a_consumer_joins_the_group_set_on_its_builder() {
    let builder = ConsumerBuilder::new_unbound()
        .client_agent("orders-worker/1.4.0")
        .group(ConsumerGroupRef::id(crate::ids::ConsumerGroupId(
            Uuid::from_u128(0x2201),
        )));

    assert_eq!(
        builder.effective_group(),
        ConsumerGroupRef::Id(crate::ids::ConsumerGroupId(Uuid::from_u128(0x2201)))
    );
}

/// A consumer with no slots would create its anonymous group and never join it, so
/// `start()` refuses it before any broker call.
#[tokio::test]
async fn a_consumer_with_no_parallel_slots_is_refused_and_creates_no_group() {
    let broker = Arc::new(RecordingBroker::listing_no_event_types());
    let ready = ConsumerBuilder::new(broker.clone())
        .security_context(identified_context())
        .client_agent("orders-worker/1.4.0")
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .parallelism(0)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let started = tokio::time::timeout(BOUND, ready.start())
        .await
        .expect("start returns within the bound");

    let Err(error) = started else {
        panic!("a consumer with no slots must be refused at start");
    };
    let EventBrokerError::InvalidConsumerOptions { detail } = &error else {
        panic!("a consumer with no slots must be refused as invalid, got {error:?}");
    };
    assert_eq!(detail, "parallelism must be at least 1");
    assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
}

/// The broker refuses a zero `session_timeout` on every JOIN, so each slot would end
/// at its first JOIN and the anonymous group would be left without members. `start()`
/// refuses it before any broker call.
#[tokio::test]
async fn a_consumer_with_a_zero_session_timeout_is_refused_and_creates_no_group() {
    let broker = Arc::new(RecordingBroker::listing_no_event_types());
    let ready = ConsumerBuilder::new(broker.clone())
        .security_context(identified_context())
        .client_agent("orders-worker/1.4.0")
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .session_timeout(Duration::ZERO)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let started = tokio::time::timeout(BOUND, ready.start())
        .await
        .expect("start returns within the bound");

    let Err(error) = started else {
        panic!("a consumer with a zero session timeout must be refused at start");
    };
    let EventBrokerError::InvalidConsumerOptions { detail } = &error else {
        panic!("a zero session timeout must be refused as invalid, got {error:?}");
    };
    assert_eq!(detail, "session_timeout must be positive");
    assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
}

/// Only a zero duration is refused: a positive one under a second passes the checks
/// `start()` makes before any broker call.
#[test]
fn a_consumer_accepts_a_sub_second_session_timeout() {
    let builder = ConsumerBuilder::new_unbound().session_timeout(Duration::from_millis(1));

    builder
        .effective_settings()
        .expect("a positive sub-second session timeout passes start's checks");

    assert_eq!(builder.session_timeout, Some(Duration::from_millis(1)));
}

/// The event types are listed before the group is resolved, so a start that fails
/// to list them creates no anonymous group.
#[tokio::test]
async fn a_consumer_whose_type_lookup_fails_creates_no_group() {
    let broker = Arc::new(RecordingBroker::refusing_the_event_type_lookup());
    let ready = ConsumerBuilder::new(broker.clone())
        .security_context(identified_context())
        .client_agent("orders-worker/1.4.0")
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let started = tokio::time::timeout(BOUND, ready.start())
        .await
        .expect("start returns within the bound");

    let Err(error) = started else {
        panic!("the failed type lookup must fail the start");
    };
    let EventBrokerError::Unauthorized { detail } = &error else {
        panic!("the start must fail with the type lookup's error, got {error:?}");
    };
    assert_eq!(detail, "the caller may not read event types");
    assert_eq!(broker.calls(), vec![BrokerCall::ListEventTypes]);
}

/// The broker refuses a client agent outside the RFC 9110 User-Agent grammar on
/// every JOIN, so a consumer of an existing group with one would start and never
/// consume. `start()` refuses it before any broker call.
#[tokio::test]
async fn a_consumer_of_an_existing_group_with_an_off_grammar_client_agent_is_refused_at_start() {
    let broker = Arc::new(RecordingBroker::listing_no_event_types());
    let ready = ConsumerBuilder::new(broker.clone())
        .security_context(identified_context())
        .client_agent("agent/")
        .group(ConsumerGroupRef::existing(CREATED_GROUP))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let started = tokio::time::timeout(BOUND, ready.start())
        .await
        .expect("start returns within the bound");

    let Err(error) = started else {
        panic!("a consumer with an off-grammar client agent must be refused at start");
    };
    let EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    } = &error
    else {
        panic!("the client agent must be refused as an invalid text field, got {error:?}");
    };
    assert_eq!(
        (*field, detail.as_str(), *reason),
        (
            "client_agent",
            "must follow the RFC 9110 User-Agent grammar: product *( SP ( product / comment ) )",
            "user_agent_grammar"
        )
    );
    assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
}

/// A consumer that names no group creates its anonymous group with its client
/// agent, which the broker would refuse when over 256 bytes. `start()` refuses
/// it first, so no group is created.
#[tokio::test]
async fn a_consumer_with_an_over_long_client_agent_is_refused_before_creating_its_group() {
    let broker = Arc::new(RecordingBroker::listing_no_event_types());
    let ready = ConsumerBuilder::new(broker.clone())
        .security_context(identified_context())
        .client_agent("a".repeat(257))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let started = tokio::time::timeout(BOUND, ready.start())
        .await
        .expect("start returns within the bound");

    let Err(error) = started else {
        panic!("a consumer with an over-long client agent must be refused at start");
    };
    let EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    } = &error
    else {
        panic!("the client agent must be refused as an invalid text field, got {error:?}");
    };
    assert_eq!(
        (*field, detail.as_str(), *reason),
        (
            "client_agent",
            "must be 1-256 bytes, got 257",
            "field_too_long"
        )
    );
    assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
}

#[cfg(feature = "db")]
mod tx_typestate {

    use async_trait::async_trait;

    use super::*;
    use crate::consumer::{
        CommitOffsetInTx, ConsumerOffsetManager, OffsetManagerError, OffsetStore, Position,
        TxBatchHandler, TxCommitHandle, TxEventHandler, WithTx,
    };
    use crate::ids::ConsumerGroupId;
    use crate::sequence::Sequence;

    #[derive(Default)]
    struct RecordingTxOffsetManager;

    #[async_trait]
    impl OffsetStore for RecordingTxOffsetManager {
        async fn load_position(
            &self,
            _group: &ConsumerGroupId,
            _topic: &GtsInstanceId,
            _partition: u32,
        ) -> Result<Position, OffsetManagerError> {
            Ok(Fallback::Earliest.into())
        }
    }

    #[async_trait]
    impl CommitOffsetInTx for RecordingTxOffsetManager {
        async fn commit_in_tx<TX>(
            &self,
            _txn: &TX,
            _group: &ConsumerGroupId,
            _topic: &GtsInstanceId,
            _partition: u32,
            _offset: Sequence,
        ) -> Result<(), OffsetManagerError>
        where
            TX: toolkit_db::secure::DBRunner + Sync,
        {
            Ok(())
        }
    }

    impl ConsumerOffsetManager for RecordingTxOffsetManager {
        type BuilderState = WithTx<Self>;

        fn into_builder_state(self) -> Self::BuilderState {
            WithTx(self)
        }
    }

    struct NoopTxHandler;

    #[async_trait]
    impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for NoopTxHandler {
        async fn handle(
            &self,
            _event: EventV1,
            _delivery: &Delivery,
            _commit: TxCommitHandle<RecordingTxOffsetManager>,
        ) -> Result<HandlerOutcome, ConsumerError> {
            Ok(HandlerOutcome::Success)
        }
    }

    struct NoopTxBatchHandler;

    #[async_trait]
    impl TxBatchHandler<serde_json::Value, RecordingTxOffsetManager> for NoopTxBatchHandler {
        async fn handle_batch(
            &self,
            _batch: &EventBatch<'_, serde_json::Value>,
            _delivery: &Delivery,
            _commit: TxCommitHandle<RecordingTxOffsetManager>,
        ) -> Result<HandlerOutcome, ConsumerError> {
            Ok(HandlerOutcome::Success)
        }
    }

    /// As for the broker-only runtime, the transactional runtime refuses the default
    /// anonymous context before it calls the panicking broker.
    #[tokio::test]
    async fn a_transactional_consumer_without_an_identity_is_refused_before_it_calls_the_broker() {
        let ready = ConsumerBuilder::new(Arc::new(NoBroker))
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-no-identity"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let Err(EventBrokerError::InvalidConsumerOptions { detail }) = ready.start().await else {
            panic!("a transactional consumer without an identity must be refused at start");
        };

        assert_eq!(
            detail,
            "the consumer has no identity: set a security context with a subject and a tenant, \
             or one with a bearer token"
        );
    }

    /// As for the broker-only runtime, the event types are listed before the group
    /// is resolved, so a start that fails to list them creates no anonymous group.
    #[tokio::test]
    async fn a_transactional_consumer_whose_type_lookup_fails_creates_no_group() {
        let broker = Arc::new(RecordingBroker::refusing_the_event_type_lookup());
        let ready = ConsumerBuilder::new(broker.clone())
            .security_context(identified_context())
            .client_agent("orders-worker/1.4.0")
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let started = tokio::time::timeout(BOUND, ready.start())
            .await
            .expect("start returns within the bound");

        let Err(error) = started else {
            panic!("the failed type lookup must fail the start");
        };
        let EventBrokerError::Unauthorized { detail } = &error else {
            panic!("the start must fail with the type lookup's error, got {error:?}");
        };
        assert_eq!(detail, "the caller may not read event types");
        assert_eq!(broker.calls(), vec![BrokerCall::ListEventTypes]);
    }

    /// As for the broker-only runtime, the transactional runtime refuses a client
    /// agent outside the RFC 9110 User-Agent grammar before any broker call.
    #[tokio::test]
    async fn a_transactional_consumer_with_an_off_grammar_client_agent_is_refused_at_start() {
        let broker = Arc::new(RecordingBroker::listing_no_event_types());
        let ready = ConsumerBuilder::new(broker.clone())
            .security_context(identified_context())
            .client_agent("agent/")
            .group(ConsumerGroupRef::existing(CREATED_GROUP))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let started = tokio::time::timeout(BOUND, ready.start())
            .await
            .expect("start returns within the bound");

        let Err(error) = started else {
            panic!("a transactional consumer with an off-grammar client agent must be refused");
        };
        let EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        } = &error
        else {
            panic!("the client agent must be refused as an invalid text field, got {error:?}");
        };
        assert_eq!(
            (*field, detail.as_str(), *reason),
            (
                "client_agent",
                "must follow the RFC 9110 User-Agent grammar: product *( SP ( product / comment ) )",
                "user_agent_grammar"
            )
        );
        assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
    }

    /// As for the broker-only runtime, the transactional runtime refuses a zero
    /// `session_timeout` before any broker call.
    #[tokio::test]
    async fn a_transactional_consumer_with_a_zero_session_timeout_is_refused_at_start() {
        let broker = Arc::new(RecordingBroker::listing_no_event_types());
        let ready = ConsumerBuilder::new(broker.clone())
            .security_context(identified_context())
            .client_agent("orders-worker/1.4.0")
            .group(ConsumerGroupRef::existing(CREATED_GROUP))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .session_timeout(Duration::ZERO)
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let started = tokio::time::timeout(BOUND, ready.start())
            .await
            .expect("start returns within the bound");

        let Err(error) = started else {
            panic!("a transactional consumer with a zero session timeout must be refused");
        };
        let EventBrokerError::InvalidConsumerOptions { detail } = &error else {
            panic!("a zero session timeout must be refused as invalid, got {error:?}");
        };
        assert_eq!(detail, "session_timeout must be positive");
        assert_eq!(broker.calls(), Vec::<BrokerCall>::new());
    }

    /// The group is resolved once, in `start()` before any slot spawns, so both slots
    /// join the one group created. The test runtime is single-threaded and nothing
    /// is awaited between `start()` returning and the first read, so no slot has run
    /// yet at that read. The test then yields until both JOINs have reached the broker,
    /// so the assert after the stop does not rely on a slot sending its JOIN before it
    /// first checks its cancel. A yield lets each slot run until it next waits, and a
    /// slot sends its JOIN in its first run, so the 16 yields allowed are far more than
    /// the wait needs. The broker refuses each JOIN, which ends that slot, so the
    /// bounded stop returns at once.
    #[tokio::test]
    async fn the_slots_of_one_transactional_consumer_resolve_one_group() {
        let broker = Arc::new(RecordingBroker::listing_no_event_types());
        let ready = ConsumerBuilder::new(broker.clone())
            .security_context(identified_context())
            .client_agent("orders-worker/1.4.0")
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .parallelism(2)
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let handle = tokio::time::timeout(BOUND, ready.start())
            .await
            .expect("start returns within the bound")
            .expect("the consumer starts");

        assert_eq!(
            broker.calls(),
            vec![
                BrokerCall::ListEventTypes,
                BrokerCall::CreateConsumerGroup {
                    client_agent: "orders-worker/1.4.0".to_owned(),
                    description: None,
                },
            ]
        );

        tokio::time::timeout(BOUND, async {
            for _ in 0..16 {
                if broker.calls().len() == 4 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the wait for both JOINs ends within the bound");
        tokio::time::timeout(BOUND, handle.stop())
            .await
            .expect("stop returns within the bound")
            .expect("the consumer stops");

        assert_eq!(
            broker.calls(),
            vec![
                BrokerCall::ListEventTypes,
                BrokerCall::CreateConsumerGroup {
                    client_agent: "orders-worker/1.4.0".to_owned(),
                    description: None,
                },
                BrokerCall::Join {
                    group: CREATED_GROUP
                },
                BrokerCall::Join {
                    group: CREATED_GROUP
                },
            ]
        );
    }

    #[test]
    fn consumer_builder_accepts_transactional_typestate_quadrants() {
        let _single = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-single"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let _batch = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-batch"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .batch_handler(NoopTxBatchHandler);

        let _routed = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-routed"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .default_handler(NoopTxHandler)
            .route()
            .topic(ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            )))
            .event_type(gpat(gts_id!(
                "cf.core.events.event.v1~example.orders.order_created.x.v1~"
            )))
            .batch_handler(NoopTxBatchHandler);
    }
}

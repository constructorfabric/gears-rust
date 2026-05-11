use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use uuid::Uuid;

use super::*;
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, EventTypeId, TopicId};

struct NoSecurityContextHandler;

#[async_trait::async_trait]
impl EventHandler<CommitHandle, HandlerOutcome> for NoSecurityContextHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<HandlerOutcome, EventBrokerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[test]
fn uuid_identity_newtypes_preserve_uuid_values() {
    let topic_uuid = Uuid::new_v4();
    let event_type_uuid = Uuid::new_v4();
    let group_uuid = Uuid::new_v4();

    let topic_id = TopicId::new(topic_uuid);
    let event_type_id = EventTypeId::new(event_type_uuid);
    let group_id = ConsumerGroupId::new(group_uuid);

    assert_eq!(topic_id.as_uuid(), topic_uuid);
    assert_eq!(event_type_id.as_uuid(), event_type_uuid);
    assert_eq!(group_id.as_uuid(), group_uuid);
    assert_eq!(topic_id.to_string(), topic_uuid.to_string());
    assert_eq!(event_type_id.to_string(), event_type_uuid.to_string());
    assert_eq!(group_id.to_string(), group_uuid.to_string());
}

#[test]
fn handler_signature_has_no_security_context_parameter() {
    fn assert_handler<T: EventHandler<CommitHandle, HandlerOutcome>>() {}

    assert_handler::<NoSecurityContextHandler>();
}

#[test]
fn refs_convert_from_resolved_ids() {
    let topic_id = TopicId::new(Uuid::new_v4());
    let event_type_id = EventTypeId::new(Uuid::new_v4());
    let group_id = ConsumerGroupId::new(Uuid::new_v4());

    assert_eq!(TopicRef::from(topic_id), TopicRef::Id(topic_id));
    assert_eq!(
        EventTypeRef::from(event_type_id),
        EventTypeRef::Id(event_type_id)
    );
    assert_eq!(
        ConsumerGroupRef::from(group_id.clone()),
        ConsumerGroupRef::Id(group_id)
    );
}

#[test]
fn subscription_interest_builder_keeps_types_and_filter_per_topic() {
    let interest = SubscriptionInterest::builder()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .types([
            EventTypeRef::gts("gts.cf.core.events.event.v1~orders.OrderCreated"),
            EventTypeRef::gts_pattern("gts.cf.core.events.event.v1~orders.*"),
        ])
        .filter(SubscriptionFilterRef::cel("tenant_id == $tenant_id"))
        .build()
        .expect("interest should be valid");

    assert_eq!(
        interest.topic,
        TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1")
    );
    assert_eq!(interest.event_types.len(), 2);
    assert_eq!(
        interest.filter,
        Some(SubscriptionFilterRef::cel("tenant_id == $tenant_id"))
    );
}

#[test]
fn subscription_interest_builder_rejects_missing_required_fields() {
    let missing_topic = SubscriptionInterest::builder()
        .types([EventTypeRef::gts_pattern(
            "gts.cf.core.events.event.v1~orders.*",
        )])
        .build()
        .expect_err("topic is required");
    assert!(matches!(
        missing_topic,
        EventBrokerError::InvalidConsumerOptions { .. }
    ));

    let missing_types = SubscriptionInterest::builder()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .build()
        .expect_err("event types are required");
    assert!(matches!(
        missing_types,
        EventBrokerError::InvalidConsumerOptions { .. }
    ));
}

#[test]
fn cel_filter_uses_broker_filter_engine_ref() {
    let filter = SubscriptionFilterRef::cel("event.data.amount > 100");

    assert_eq!(filter.expression, "event.data.amount > 100");
    assert_eq!(
        filter.engine,
        FilterEngineRef::gts("gts.cf.core.events.filter.v1~cf.core.expression.cel.v1")
    );
}

#[derive(Default)]
struct ResolverCounters {
    topic_calls: AtomicUsize,
    event_type_calls: AtomicUsize,
    group_calls: AtomicUsize,
}

struct CountingResolver {
    counters: Arc<ResolverCounters>,
}

impl CountingResolver {
    fn new(counters: Arc<ResolverCounters>) -> Self {
        Self { counters }
    }
}

#[async_trait::async_trait]
impl TypeRegistryResolver for CountingResolver {
    async fn resolve_topic(&self, _topic: &TopicRef) -> Result<TopicId, EventBrokerError> {
        self.counters.topic_calls.fetch_add(1, Ordering::SeqCst);
        Ok(TopicId::new(Uuid::from_u128(1)))
    }

    async fn resolve_event_type(
        &self,
        _event_type: &EventTypeRef,
    ) -> Result<EventTypeSelector, EventBrokerError> {
        self.counters
            .event_type_calls
            .fetch_add(1, Ordering::SeqCst);
        Ok(EventTypeSelector::Exact(vec![EventTypeId::new(
            Uuid::from_u128(2),
        )]))
    }

    async fn resolve_consumer_group(
        &self,
        _group: &ConsumerGroupRef,
    ) -> Result<ConsumerGroupId, EventBrokerError> {
        self.counters.group_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ConsumerGroupId::new(Uuid::from_u128(3)))
    }
}

#[tokio::test]
async fn cached_type_registry_resolver_caches_resolved_refs() {
    let counters = Arc::new(ResolverCounters::default());
    let resolver = CachedTypeRegistryResolver::new(CountingResolver::new(counters.clone()));
    let topic = TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1");
    let event_type = EventTypeRef::gts("gts.cf.core.events.event.v1~orders.OrderCreated");
    let group = ConsumerGroupRef::gts("gts.cf.core.events.consumer-group.v1~orders-worker");

    let first_topic = resolver.resolve_topic(&topic).await.unwrap();
    let second_topic = resolver.resolve_topic(&topic).await.unwrap();
    let first_event_type = resolver.resolve_event_type(&event_type).await.unwrap();
    let second_event_type = resolver.resolve_event_type(&event_type).await.unwrap();
    let first_group = resolver.resolve_consumer_group(&group).await.unwrap();
    let second_group = resolver.resolve_consumer_group(&group).await.unwrap();

    assert_eq!(first_topic, second_topic);
    assert_eq!(first_event_type, second_event_type);
    assert_eq!(first_group, second_group);
    assert_eq!(counters.topic_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counters.event_type_calls.load(Ordering::SeqCst), 1);
    assert_eq!(counters.group_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn consumer_profiles_are_distinct_operating_modes() {
    let default = ConsumerProfile::default_profile();
    let low_latency = ConsumerProfile::low_latency();
    let high_throughput = ConsumerProfile::high_throughput();
    let replay = ConsumerProfile::replay();
    let relaxed = ConsumerProfile::relaxed();

    assert_eq!(default.batching.max_events, 1);
    assert_eq!(low_latency.batching.max_wait, Duration::from_millis(0));
    assert!(low_latency.slow_detection.handler_latency < default.slow_detection.handler_latency);

    assert!(high_throughput.batching.max_events > default.batching.max_events);
    assert!(high_throughput.buffering.partition_capacity > default.buffering.partition_capacity);

    assert!(replay.batching.max_events > high_throughput.batching.max_events);
    assert!(replay.slow_detection.handler_latency > high_throughput.slow_detection.handler_latency);

    assert!(relaxed.listener.channel_capacity < default.listener.channel_capacity);
    assert!(relaxed.retry.base_delay > default.retry.base_delay);
}

#[test]
fn consumer_profile_default_matches_named_default_profile() {
    assert_eq!(
        ConsumerProfile::default(),
        ConsumerProfile::default_profile()
    );
}

#[test]
fn consumer_settings_resolve_explicit_override_over_profile() {
    let profile = ConsumerProfile::high_throughput();
    let override_batching = ConsumerBatching {
        max_events: 7,
        max_wait: Duration::from_millis(25),
    };

    let settings = ConsumerSettings::resolve(
        profile.clone(),
        ConsumerSettingsOverrides {
            batching: Some(override_batching),
            ..ConsumerSettingsOverrides::default()
        },
    );

    assert_eq!(settings.batching, override_batching);
    assert_eq!(settings.buffering, profile.buffering);
    assert_eq!(settings.slow_detection, profile.slow_detection);
    assert_eq!(settings.retry, profile.retry);
    assert_eq!(settings.listener, profile.listener);
}

#[test]
fn consumer_settings_validation_rejects_invalid_values() {
    let mut settings = ConsumerSettings::from_profile(ConsumerProfile::default_profile());
    settings.buffering.low_watermark = settings.buffering.high_watermark + 1;
    assert!(matches!(
        settings.validate(),
        Err(EventBrokerError::InvalidConsumerOptions { .. })
    ));

    let mut settings = ConsumerSettings::from_profile(ConsumerProfile::default_profile());
    settings.batching.max_events = 0;
    assert!(matches!(
        settings.validate(),
        Err(EventBrokerError::InvalidConsumerOptions { .. })
    ));

    let mut settings = ConsumerSettings::from_profile(ConsumerProfile::default_profile());
    settings.listener.channel_capacity = 0;
    assert!(matches!(
        settings.validate(),
        Err(EventBrokerError::InvalidConsumerOptions { .. })
    ));
}

#[test]
fn consumer_commit_mode_defaults_to_auto_commit_interval() {
    assert_eq!(
        ConsumerCommitMode::default(),
        ConsumerCommitMode::Auto {
            interval: Duration::from_secs(20),
        }
    );
    assert_eq!(
        ConsumerCommitMode::auto(Duration::from_secs(5)),
        ConsumerCommitMode::Auto {
            interval: Duration::from_secs(5),
        }
    );
    assert_eq!(ConsumerCommitMode::manual(), ConsumerCommitMode::Manual);
}

#[test]
fn consumer_builder_uses_single_commit_mode_setting() {
    let auto = ConsumerBuilder::new_unbound()
        .commit_mode(ConsumerCommitMode::auto(Duration::from_secs(3)));
    assert_eq!(
        auto.commit_mode,
        ConsumerCommitMode::Auto {
            interval: Duration::from_secs(3),
        }
    );

    let manual = ConsumerBuilder::new_unbound().commit_mode(ConsumerCommitMode::manual());
    assert_eq!(manual.commit_mode, ConsumerCommitMode::Manual);
}

use std::sync::Arc;
use std::time::Duration;

use event_broker::test_support::{
    EventBrokerHarness, EventBrokerHarnessBuilder, StaticTypesRegistry,
};
use event_broker_sdk::{Ack, EventBrokerApi, EventV1, GtsIdPattern, GtsInstanceId, GtsTypeId};
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub struct TopicFixture {
    pub broker: Arc<dyn EventBrokerApi>,
    pub ctx: SecurityContext,
    /// Owns the running in-process broker. Its loader and ingest-outbox
    /// background tasks must outlive the test, so the harness is held here and
    /// dropped only when the fixture is.
    _harness: EventBrokerHarness,
}

pub struct PublishJson<'a> {
    pub broker: &'a Arc<dyn EventBrokerApi>,
    pub ctx: &'a SecurityContext,
    /// No topic: the broker resolves the destination stream from `event_type`.
    pub event_type: &'a str,
    pub subject: &'a str,
    /// Value the fixture's partition-key pointer will resolve to. The fixture
    /// points at `/data/partition_key`, so this lands in the payload.
    pub partition_key: Option<&'a str>,
    pub partition: Option<u32>,
    pub data: serde_json::Value,
}

/// Where the fixture's event types take their partition key from. The default
/// points at the tenant, which would land every fixture event on one partition;
/// these fixtures need to place events on a chosen partition. `/source` is the
/// member to point at: no test asserts it, so routing leaves the subject and the
/// payload - which tests do assert - exactly as the caller wrote them.
const FIXTURE_PARTITION_POINTER: &str = "/source";

/// Partition key of an event published with neither a partition nor a key: one
/// fixed value, so such events of a fixture share one partition in every run.
const UNROUTED_PARTITION_KEY: &str = "00000000-0000-0000-0000-000000000000";

/// The subject type every fixture event carries. A real broker validates
/// `subject_type` as a GTS id and rejects it unless the event type declares it
/// in `allowed_subject_types`, so the fixtures publish this one and every
/// catalog entry allows it.
pub const SHOWCASE_SUBJECT_TYPE: &str = "gts.x.eb.showcase.subject.v1~";

/// Builds a fixture over a real in-process broker seeded from `catalog`. The fixture
/// publishes and consumes as the harness principal; every consumer built from the
/// fixture passes `.security_context(fixture.ctx.clone())`,
/// `.subject_tenant(fixture.ctx.subject_tenant_id())` and, since its events are
/// unowned, `.ownership_filter(OwnershipFilter::EntireTenant)`, so a published
/// event and the consumer that joins for it share a tenant.
pub async fn fixture_from_catalog(catalog: StaticTypesRegistry) -> TopicFixture {
    fixture_from_harness(EventBrokerHarness::builder().with_type_registry(catalog)).await
}

async fn fixture_from_harness(builder: EventBrokerHarnessBuilder) -> TopicFixture {
    let harness = builder.build().await;
    let broker = harness.broker();
    let ctx = harness.security_context().clone();
    TopicFixture {
        broker,
        ctx,
        _harness: harness,
    }
}

/// One topic with one event type, the type partitioned by `/source`.
pub async fn topic_fixture(topic: &str, event_type: &str, partitions: u32) -> TopicFixture {
    fixture_from_catalog(topic_catalog(topic, event_type, partitions)).await
}

/// A [`topic_fixture`] whose broker sends an idle stream a heartbeat every
/// `heartbeat` instead of every second.
pub async fn topic_fixture_with_heartbeat(
    topic: &str,
    event_type: &str,
    partitions: u32,
    heartbeat: Duration,
) -> TopicFixture {
    fixture_from_harness(
        EventBrokerHarness::builder()
            .with_type_registry(topic_catalog(topic, event_type, partitions))
            .with_heartbeat(heartbeat),
    )
    .await
}

fn topic_catalog(topic: &str, event_type: &str, partitions: u32) -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": topic, "partitions": partitions },
        {
            "id": event_type,
            "topic": topic,
            "data_schema": { "type": "object" },
            "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
            "partition_key": FIXTURE_PARTITION_POINTER,
        },
    ]))
}

pub async fn publish_json(
    broker: &Arc<dyn EventBrokerApi>,
    ctx: &SecurityContext,
    event_type: &str,
    subject: &str,
    partition: Option<u32>,
    data: serde_json::Value,
) {
    publish_json_with_partition_key(PublishJson {
        broker,
        ctx,
        event_type,
        subject,
        partition_key: None,
        partition,
        data,
    })
    .await;
}

pub async fn publish_json_with_partition_key(request: PublishJson<'_>) {
    let PublishJson {
        broker,
        ctx,
        event_type,
        subject,
        partition_key,
        partition,
        data,
    } = request;
    let resolved_partition_key = partition_key
        .map(str::to_owned)
        .or_else(|| partition.map(partition_key_for_two_partition_fixture))
        .unwrap_or_else(|| UNROUTED_PARTITION_KEY.to_owned());
    let event = EventV1::builder_for(GtsTypeId::new(event_type), data)
        .subject_tenant(ctx.subject_tenant_id())
        // The fixture's types are partitioned by this member.
        .source(resolved_partition_key)
        .subject(subject)
        .subject_type_of(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .build()
        .expect("a valid event");
    broker
        .publish(ctx, &event, Ack::Accepted)
        .await
        .expect("event published");
}

/// An event of `event_type` with a fixed `id` and `subject`, routed to `partition`
/// of a two-partition fixture.
pub fn two_partition_event(
    ctx: &SecurityContext,
    event_type: &str,
    id: Uuid,
    subject: &str,
    partition: u32,
) -> EventV1 {
    EventV1::builder_for(GtsTypeId::new(event_type), json!({}))
        .id(id)
        .subject_tenant(ctx.subject_tenant_id())
        .source(partition_key_for_two_partition_fixture(partition))
        .subject(subject)
        .subject_type_of(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .build()
        .expect("a valid event")
}

fn partition_key_for_two_partition_fixture(target: u32) -> String {
    match target {
        0 => "fixture-partition-key-0-0",
        1 => "fixture-partition-key-1-0",
        _ => panic!("two-partition fixture cannot target partition {target}"),
    }
    .to_owned()
}

/// Test-side constructors for typed GTS ids. Call sites pass a `gts_id!(...)`
/// literal so the id is validated at compile time; these wrap it in the typed
/// value the SDK consumer API requires (it never accepts a bare string).
pub fn topic(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid topic GTS instance id")
}

pub fn event_type(id: &str) -> GtsTypeId {
    GtsTypeId::try_new(id).expect("valid event type GTS id")
}

pub fn event_pattern(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

pub async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was not observed before timeout");
}

/// A fixture with two topics, each with its own event type, for the multi-topic
/// delivery path. A delivered event names no topic, so attributing it correctly
/// depends entirely on its type resolving - and partition 0 exists on both.
pub async fn two_topic_fixture(
    first: (&str, &str),
    second: (&str, &str),
    partitions: u32,
) -> TopicFixture {
    let (first_topic, first_type) = first;
    let (second_topic, second_type) = second;
    fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": first_topic, "partitions": partitions },
        {
            "id": first_type,
            "topic": first_topic,
            "data_schema": { "type": "object" },
            "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
            "partition_key": FIXTURE_PARTITION_POINTER,
        },
        { "id": second_topic, "partitions": partitions },
        {
            "id": second_type,
            "topic": second_topic,
            "data_schema": { "type": "object" },
            "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
            "partition_key": FIXTURE_PARTITION_POINTER,
        },
    ])))
    .await
}

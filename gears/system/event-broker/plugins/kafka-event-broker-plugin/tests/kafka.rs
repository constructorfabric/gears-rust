//! Kafka-specific behaviour the contract leaves to the plugin
//! (`docs/TESTING.md` layer 2): fencing across instances, provisioning
//! states, topic configuration, and where heads sit among commit markers.

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed expectation is the test failure"
)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, Head,
    KeyedEvent, LogClaim, LogId, Partition, Provisioning, Replacing, Retention, TopicPartition,
    TopicSpec, Wait,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position, Sequence};
use rdkafka::ClientConfig;
use rdkafka::admin::{AdminClient, AdminOptions, ResourceSpecifier};
use rdkafka::client::DefaultClientContext;
use rdkafka::consumer::{BaseConsumer, Consumer};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const KEEP: Retention = Retention {
    max_age: Some(Duration::from_hours(24)),
    max_bytes: None,
};

fn topic(name: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "{}cf.kafkatest.{name}.t{}.v1",
        toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        &Uuid::new_v4().simple().to_string()[..12]
    ))
    .expect("valid id")
}

fn event(n: u64) -> KeyedEvent {
    KeyedEvent {
        key: "k".to_owned(),
        event: Event {
            id: Uuid::from_u128(0x6b61_0000 + u128::from(n)),
            type_id: GtsTypeId::new(toolkit_gts::gts_id!(
                "cf.core.events.event.v1~cf.kafkatest.x.happened.v1~"
            )),
            tenant_id: Uuid::from_u128(0x7e4a_0001),
            source: "kafka-test".to_owned(),
            subject: format!("k-{n}"),
            subject_type: GtsTypeId::new(toolkit_gts::gts_id!("cf.kafkatest.x.thing.v1~")),
            occurred_at: Utc::now(),
            trace_parent: None,
            data: Some(serde_json::json!({ "n": n })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

fn fence(key: FenceKey, seq: i64) -> Fence {
    Fence {
        key,
        seq: FenceSeq::new(seq),
    }
}

async fn append(
    backend: &dyn Backend,
    fence: Fence,
    topic: &GtsInstanceId,
    events: &[KeyedEvent],
) -> Appended {
    backend
        .append(&AppendRequest {
            fence,
            topic,
            events,
            flush: Flush::Sync,
        })
        .await
        .expect("append")
}

/// Provisions `spec` under a fresh log id, waiting out `Pending`.
async fn provisioned(backend: &dyn Backend, spec: &TopicSpec) -> LogId {
    let log = LogId::new_v4();
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: log,
    };
    for _ in 0..100 {
        match backend.provision(spec, &claim).await.expect("provision") {
            Provisioning::Ready { .. } => return log,
            Provisioning::Pending => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("provisioning stayed Pending");
}

fn spec(name: &str, partitions: u32, retention: Retention) -> TopicSpec {
    TopicSpec {
        topic: topic(name),
        partitions,
        retention,
    }
}

/// The `n` payload of every event of partition 0.
async fn ns(backend: &dyn Backend, topic: &GtsInstanceId) -> Vec<u64> {
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    backend
        .read(&at, Sequence::NONE, 100, Wait::No)
        .await
        .expect("read")
        .iter()
        .map(|e| e.data.as_ref().expect("payload")["n"].as_u64().expect("n"))
        .collect()
}

fn admin(bootstrap: &str) -> AdminClient<DefaultClientContext> {
    ClientConfig::new()
        .set("bootstrap.servers", bootstrap)
        .create()
        .expect("admin client")
}

/// Instance B opening a fence key's producer fences instance A's. A late copy
/// of the fence B applied, arriving at the zombie A, stores nothing: A's
/// producer is refused by the broker, A opens it again and reads the fence.
/// A's next fence then fences B in turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zombie_producer_is_fenced() {
    let cluster = common::cluster("zombie").await;
    let prefix = common::fresh_prefix();
    let first = common::backend(&cluster.bootstrap, &prefix).await;
    let second = common::backend(&cluster.bootstrap, &prefix).await;
    let spec = spec("zombie", 1, KEEP);
    provisioned(first.as_ref(), &spec).await;
    let key = FenceKey::new(Uuid::new_v4().as_u128());

    let answers = [
        append(first.as_ref(), fence(key, 1), &spec.topic, &[event(1)]).await,
        append(second.as_ref(), fence(key, 2), &spec.topic, &[event(2)]).await,
        append(first.as_ref(), fence(key, 2), &spec.topic, &[event(2)]).await,
        append(first.as_ref(), fence(key, 3), &spec.topic, &[event(3)]).await,
        append(second.as_ref(), fence(key, 3), &spec.topic, &[event(3)]).await,
    ]
    .map(|answer| matches!(answer, Appended::Duplicate));

    assert_eq!(answers, [false, false, true, false, true]);
    assert_eq!(ns(second.as_ref(), &spec.topic).await, [1, 2, 3]);
}

/// Fence state lives in the cluster: after a restart, and after a producer
/// is evicted from a one-producer pool, a replayed fence is a duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_after_a_producer_restart_is_a_duplicate() {
    let cluster = common::cluster("replay").await;
    let prefix = common::fresh_prefix();
    let spec = spec("replay", 1, KEEP);
    let (one, two) = (
        FenceKey::new(Uuid::new_v4().as_u128()),
        FenceKey::new(Uuid::new_v4().as_u128()),
    );

    let first = common::backend(&cluster.bootstrap, &prefix).await;
    provisioned(first.as_ref(), &spec).await;
    append(first.as_ref(), fence(one, 1), &spec.topic, &[event(1)]).await;
    append(first.as_ref(), fence(one, 2), &spec.topic, &[event(2)]).await;
    first.stop(CancellationToken::new()).await;
    drop(first);

    let single = common::built(
        &cluster.bootstrap,
        &prefix,
        serde_json::json!({ "producer_pool": 1 }),
    )
    .await;
    single.start().await.expect("start");
    let answers = [
        append(single.as_ref(), fence(one, 2), &spec.topic, &[event(2)]).await,
        append(single.as_ref(), fence(one, 1), &spec.topic, &[event(1)]).await,
        // Evicts key one's producer.
        append(single.as_ref(), fence(two, 1), &spec.topic, &[event(3)]).await,
        append(single.as_ref(), fence(one, 2), &spec.topic, &[event(2)]).await,
        append(single.as_ref(), fence(one, 3), &spec.topic, &[event(4)]).await,
    ]
    .map(|answer| matches!(answer, Appended::Duplicate));

    assert_eq!(answers, [true, true, false, true, false]);
    assert_eq!(ns(single.as_ref(), &spec.topic).await, [1, 2, 3, 4]);
}

/// The first `provision` creates the Kafka topic and answers `Pending`
/// without claiming the log, so the topic is not provisioned yet; the next
/// one claims it and answers `Ready`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisioning_is_pending_until_the_topic_exists() {
    let cluster = common::cluster("pending").await;
    let backend = common::backend(&cluster.bootstrap, &common::fresh_prefix()).await;
    let spec = spec("pending", 3, KEEP);
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: LogId::new_v4(),
    };

    let first = backend.provision(&spec, &claim).await.expect("provision");
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let between = backend.resolve(&at, Position::Latest).await;
    let mut last = backend.provision(&spec, &claim).await.expect("provision");
    for _ in 0..50 {
        if last != Provisioning::Pending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = backend.provision(&spec, &claim).await.expect("provision");
    }

    assert_eq!(first, Provisioning::Pending);
    assert!(
        matches!(between, Err(BackendError::NotProvisioned { .. })),
        "{between:?}"
    );
    assert_eq!(last, Provisioning::Ready { partitions: 3 });
    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::NONE
    );
}

/// The configuration of the Kafka topic named for `topic` under `prefix`,
/// for the settings the plugin owns.
async fn topic_config(
    bootstrap: &str,
    prefix: &str,
    topic: &GtsInstanceId,
) -> BTreeMap<String, String> {
    let name = format!("{prefix}.{}", topic_uuid(topic).simple());
    let described = admin(bootstrap)
        .describe_configs([&ResourceSpecifier::Topic(&name)], &AdminOptions::new())
        .await
        .expect("describe")
        .into_iter()
        .next()
        .expect("one resource")
        .expect("described");
    let owned = [
        "cleanup.policy",
        "message.timestamp.type",
        "min.insync.replicas",
        "retention.ms",
        "retention.bytes",
        "segment.ms",
    ];
    described
        .entries
        .into_iter()
        .filter(|entry| owned.contains(&entry.name.as_str()))
        .map(|entry| (entry.name, entry.value.unwrap_or_default()))
        .collect()
}

/// A topic is created with the spec's retention, storage-time timestamps and
/// deletion; provisioning it again under its log id with other bounds
/// changes the topic's configuration to them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retention_change_reconfigures_the_topic() {
    let cluster = common::cluster("retention-change").await;
    let prefix = common::fresh_prefix();
    let backend = common::backend(&cluster.bootstrap, &prefix).await;
    let mut spec = spec(
        "retention",
        1,
        Retention {
            max_age: Some(Duration::from_hours(1)),
            max_bytes: None,
        },
    );
    let log = provisioned(backend.as_ref(), &spec).await;
    let before = topic_config(&cluster.bootstrap, &prefix, &spec.topic).await;

    spec.retention = Retention {
        max_age: Some(Duration::from_hours(2)),
        max_bytes: Some(1 << 20),
    };
    let again = backend
        .provision(
            &spec,
            &LogClaim {
                replacing: Replacing::Log(log),
                new: log,
            },
        )
        .await
        .expect("provision again");
    let after = topic_config(&cluster.bootstrap, &prefix, &spec.topic).await;

    let config = |pairs: [(&str, &str); 6]| -> BTreeMap<String, String> {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };
    assert_eq!(again, Provisioning::Ready { partitions: 1 });
    assert_eq!(
        before,
        config([
            ("cleanup.policy", "delete"),
            ("message.timestamp.type", "LogAppendTime"),
            ("min.insync.replicas", "1"),
            ("retention.bytes", "-1"),
            ("retention.ms", "3600000"),
            ("segment.ms", "3600000"),
        ])
    );
    assert_eq!(
        after,
        config([
            ("cleanup.policy", "delete"),
            ("message.timestamp.type", "LogAppendTime"),
            ("min.insync.replicas", "1"),
            ("retention.bytes", "1048576"),
            ("retention.ms", "7200000"),
            ("segment.ms", "7200000"),
        ])
    );
}

/// Events, heads and the log claim survive a stop and a fresh instance; a
/// stopped instance answers `Unavailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_keeps_events_heads_and_the_log() {
    let cluster = common::cluster("restart").await;
    let prefix = common::fresh_prefix();
    let spec = spec("restart", 1, KEEP);
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = FenceKey::new(Uuid::new_v4().as_u128());

    let first = common::backend(&cluster.bootstrap, &prefix).await;
    let log = provisioned(first.as_ref(), &spec).await;
    append(
        first.as_ref(),
        fence(key, 1),
        &spec.topic,
        &[event(1), event(2)],
    )
    .await;
    let head = first.resolve(&at, Position::Latest).await.expect("resolve");
    first.stop(CancellationToken::new()).await;
    let stopped = first.read(&at, Sequence::NONE, 10, Wait::No).await;
    drop(first);

    let second = common::backend(&cluster.bootstrap, &prefix).await;
    let latest = second
        .resolve(&at, Position::Latest)
        .await
        .expect("resolve");
    let conflict = second
        .provision(
            &spec,
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::new_v4(),
            },
        )
        .await;
    let resumed = second
        .provision(
            &spec,
            &LogClaim {
                replacing: Replacing::Nothing,
                new: log,
            },
        )
        .await
        .expect("the same log id is accepted");
    append(second.as_ref(), fence(key, 2), &spec.topic, &[event(3)]).await;

    assert!(
        matches!(stopped, Err(BackendError::Unavailable { .. })),
        "{stopped:?}"
    );
    assert_eq!(latest, head);
    assert!(
        matches!(conflict, Err(BackendError::LogConflict { .. })),
        "{conflict:?}"
    );
    assert_eq!(resumed, Provisioning::Ready { partitions: 1 });
    assert_eq!(ns(second.as_ref(), &spec.topic).await, [1, 2, 3]);
}

/// The last stable offset of partition 0 of `topic`'s Kafka topic, as a
/// `read_committed` consumer sees it.
fn stable_offset(bootstrap: &str, prefix: &str, topic: &GtsInstanceId) -> i64 {
    let consumer: BaseConsumer = ClientConfig::new()
        .set("bootstrap.servers", bootstrap)
        .set("group.id", "kafka-test")
        .set("isolation.level", "read_committed")
        .create()
        .expect("consumer");
    let name = format!("{prefix}.{}", topic_uuid(topic).simple());
    consumer
        .fetch_watermarks(&name, 0, Duration::from_secs(10))
        .expect("watermarks")
        .1
}

/// The head an append reports, `Latest` and the head a watch starts from
/// are the last event's sequence - one below the watermark, whose offset the
/// commit marker took - and gaps follow each transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heads_are_the_last_event_never_the_watermark() {
    let cluster = common::cluster("heads").await;
    let prefix = common::fresh_prefix();
    let backend = common::backend(&cluster.bootstrap, &prefix).await;
    let spec = spec("heads", 1, KEEP);
    provisioned(backend.as_ref(), &spec).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = FenceKey::new(Uuid::new_v4().as_u128());

    let appended = [
        append(
            backend.as_ref(),
            fence(key, 1),
            &spec.topic,
            &[event(1), event(2)],
        )
        .await,
        append(backend.as_ref(), fence(key, 2), &spec.topic, &[event(3)]).await,
    ];
    let latest = backend
        .resolve(&at, Position::Latest)
        .await
        .expect("resolve");
    let fresh = common::backend(&cluster.bootstrap, &prefix).await;
    let fresh_latest = fresh.resolve(&at, Position::Latest).await.expect("resolve");
    let sequences: Vec<i64> = backend
        .read(&at, Sequence::NONE, 10, Wait::No)
        .await
        .expect("read")
        .iter()
        .map(|e| e.sequence.expect("sequence").as_i64())
        .collect();
    let stable = stable_offset(&cluster.bootstrap, &prefix, &spec.topic);

    let head = |sequence| {
        Appended::Heads(vec![Head {
            at: at.clone(),
            sequence: Sequence::assigned(sequence),
        }])
    };
    // Offsets 0 and 1 are events, 2 the first commit marker, 3 an event,
    // 4 the second marker.
    assert_eq!(sequences, [1, 2, 4]);
    assert_eq!(appended, [head(2), head(4)]);
    assert_eq!(
        (latest, fresh_latest),
        (Sequence::assigned(4), Sequence::assigned(4))
    );
    assert_eq!(stable, 5);
}

/// The broker removes events past their age, and `Latest` stays on the last
/// event's sequence - also on an instance that never saw it, which finds
/// nothing left to read - with `Earliest` at the same place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_survives_removal_of_every_event() {
    let cluster = common::cluster("removal").await;
    let prefix = common::fresh_prefix();
    let backend = common::backend(&cluster.bootstrap, &prefix).await;
    let spec = spec(
        "removal",
        1,
        Retention {
            max_age: Some(Duration::from_secs(1)),
            max_bytes: None,
        },
    );
    provisioned(backend.as_ref(), &spec).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = FenceKey::new(Uuid::new_v4().as_u128());
    append(
        backend.as_ref(),
        fence(key, 1),
        &spec.topic,
        &[event(1), event(2)],
    )
    .await;
    append(backend.as_ref(), fence(key, 2), &spec.topic, &[event(3)]).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !ns(backend.as_ref(), &spec.topic).await.is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "events still stored a minute past a one-second age"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let fresh = common::backend(&cluster.bootstrap, &prefix).await;
    let positions = [
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        fresh.resolve(&at, Position::Latest).await.expect("resolve"),
        fresh
            .resolve(&at, Position::Earliest)
            .await
            .expect("resolve"),
    ];

    assert_eq!(positions, [Sequence::assigned(4); 3]);
}

/// Two prefixes on one cluster are two backends: a topic provisioned under
/// one is not provisioned under the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefixes_keep_backends_apart() {
    let cluster = common::cluster("prefixes").await;
    let one = common::backend(&cluster.bootstrap, &common::fresh_prefix()).await;
    let other = common::backend(&cluster.bootstrap, &common::fresh_prefix()).await;
    let spec = spec("prefixes", 1, KEEP);
    provisioned(one.as_ref(), &spec).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    let answers = [
        one.resolve(&at, Position::Latest)
            .await
            .map_err(|e| e.to_string()),
        other
            .resolve(&at, Position::Latest)
            .await
            .map_err(|e| e.to_string()),
    ];

    assert_eq!(
        answers,
        [
            Ok(Sequence::NONE),
            Err("topic not provisioned on this backend".to_owned())
        ]
    );
}

/// The fences topic's partition count is the cluster's, not the setting's: an
/// instance configured with fewer or more fence partitions than the topic
/// was created with still finds a key's fence, and a replay stays a
/// duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mismatched_fence_partitions_setting_keeps_exactly_once() {
    let cluster = common::cluster("fence-partitions").await;
    let prefix = common::fresh_prefix();
    let created = common::backend(&cluster.bootstrap, &prefix).await;
    let spec = spec("fence_partitions", 1, KEEP);
    provisioned(created.as_ref(), &spec).await;
    // Partition 3 of the 4 created, 1 of 2, 7 of 8.
    let key = FenceKey::new((Uuid::new_v4().as_u128() << 4) | 7);
    append(created.as_ref(), fence(key, 1), &spec.topic, &[event(1)]).await;

    let mut answers = Vec::new();
    for partitions in [2, 8] {
        let other = common::backend_with(
            &cluster.bootstrap,
            &prefix,
            serde_json::json!({ "fence_partitions": partitions }),
        )
        .await;
        answers.push(
            other
                .append(&AppendRequest {
                    fence: fence(key, 1),
                    topic: &spec.topic,
                    events: &[event(1)],
                    flush: Flush::Sync,
                })
                .await
                .map_err(|e| e.to_string()),
        );
    }

    assert_eq!(answers, [Ok(Appended::Duplicate), Ok(Appended::Duplicate)]);
    assert_eq!(ns(created.as_ref(), &spec.topic).await, [1]);
}

/// An append whose caller gives up mid-transaction leaves no open
/// transaction behind: another key's append right after it is readable at
/// once, long before the 30 s transaction timeout would end it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_append_does_not_hold_back_readers() {
    let cluster = common::cluster("dropped").await;
    let prefix = common::fresh_prefix();
    let backend = common::backend_with(
        &cluster.bootstrap,
        &prefix,
        serde_json::json!({ "transaction_timeout_ms": 30_000 }),
    )
    .await;
    let spec = spec("dropped", 1, KEEP);
    provisioned(backend.as_ref(), &spec).await;
    let (one, two) = (
        FenceKey::new(Uuid::new_v4().as_u128()),
        FenceKey::new(Uuid::new_v4().as_u128()),
    );
    append(backend.as_ref(), fence(one, 1), &spec.topic, &[event(1)]).await;
    append(backend.as_ref(), fence(two, 1), &spec.topic, &[event(2)]).await;
    let batch: Vec<_> = (100..1100).map(event).collect();

    let dropped = tokio::time::timeout(
        Duration::from_millis(3),
        backend.append(&AppendRequest {
            fence: fence(one, 2),
            topic: &spec.topic,
            events: &batch,
            flush: Flush::Sync,
        }),
    )
    .await;
    let started = tokio::time::Instant::now();
    append(backend.as_ref(), fence(two, 2), &spec.topic, &[event(3)]).await;
    let elapsed = started.elapsed();
    let reader = common::backend(&cluster.bootstrap, &prefix).await;
    let seen = ns(reader.as_ref(), &spec.topic).await;

    assert!(dropped.is_err(), "the append finished within 3 ms");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    assert_eq!(seen, [1, 2, 3]);
}

/// Once `stop` returns, an append it overlapped has ended: nothing commits
/// afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_waits_for_an_append_in_flight() {
    let cluster = common::cluster("stop-drain").await;
    let prefix = common::fresh_prefix();
    let backend = common::backend(&cluster.bootstrap, &prefix).await;
    let spec = spec("stop_drain", 1, KEEP);
    provisioned(backend.as_ref(), &spec).await;
    let key = FenceKey::new(Uuid::new_v4().as_u128());
    append(backend.as_ref(), fence(key, 1), &spec.topic, &[event(1)]).await;
    let batch: Vec<_> = (100..1100).map(event).collect();

    let appending = {
        let backend = Arc::clone(&backend);
        let topic = spec.topic.clone();
        tokio::spawn(async move {
            backend
                .append(&AppendRequest {
                    fence: fence(key, 2),
                    topic: &topic,
                    events: &batch,
                    flush: Flush::Sync,
                })
                .await
                .map(|_| ())
        })
    };
    tokio::time::sleep(Duration::from_millis(2)).await;
    backend.stop(CancellationToken::new()).await;
    let at_stop = stable_offset(&cluster.bootstrap, &prefix, &spec.topic);
    let outcome = appending.await.expect("append task");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = stable_offset(&cluster.bootstrap, &prefix, &spec.topic);

    assert_eq!(
        later, at_stop,
        "an append committed after stop returned: {outcome:?}"
    );
}

/// On a broker that has never run a transaction - its transaction state log
/// is created by the first `init_transactions` - eight topics provisioned at
/// once, each first call cancelled after 50 ms: every later answer comes
/// within 5 s (the gear drops a call at 10 s), only as `Ready`, `Pending` or
/// `Unavailable`, and every topic is `Ready` within 90 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cold_broker_answers_provision_quickly_and_survives_a_cancel() {
    let cluster = common::cluster("cold").await;
    let backend = common::built(
        &cluster.bootstrap,
        &common::fresh_prefix(),
        serde_json::json!({}),
    )
    .await;
    backend.start().await.expect("start");
    let specs: Vec<TopicSpec> = (0..8).map(|_| spec("cold", 2, KEEP)).collect();

    let outcomes = futures_util::future::join_all(specs.iter().map(|spec| {
        let backend = Arc::clone(&backend);
        async move {
            let claim = LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::new_v4(),
            };
            let cancelled =
                tokio::time::timeout(Duration::from_millis(50), backend.provision(spec, &claim))
                    .await
                    .is_err();
            let started = tokio::time::Instant::now();
            let mut answers = Vec::new();
            loop {
                let asked = tokio::time::Instant::now();
                let answer =
                    tokio::time::timeout(Duration::from_secs(10), backend.provision(spec, &claim))
                        .await
                        .unwrap_or_else(|_| {
                            Err(BackendError::Internal("no answer within 10 s".to_owned()))
                        })
                        .map_err(|e| e.to_string());
                let took = asked.elapsed();
                let fine = took < Duration::from_secs(5)
                    && answer
                        .as_ref()
                        .map_or_else(|e| e.starts_with("backend unavailable"), |_| true);
                answers.push(format!("{took:?} {answer:?}"));
                if !fine {
                    return (cancelled, None, answers);
                }
                if let Ok(Provisioning::Ready { partitions }) = answer {
                    return (cancelled, Some(partitions), answers);
                }
                if started.elapsed() > Duration::from_secs(90) {
                    return (cancelled, None, answers);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }))
    .await;

    for (cancelled, ready, answers) in outcomes {
        assert!(cancelled, "a first call finished within 50 ms");
        assert_eq!(ready, Some(2), "{answers:?}");
    }
}

/// `stop` with its deadline already cancelled returns at once, with an
/// append in flight, and nothing it left behind keeps the runtime from
/// shutting down.
#[test]
fn stop_and_shutdown_are_prompt_past_the_deadline() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    // The container lives on a runtime of its own: dropping it needs one.
    let host = tokio::runtime::Runtime::new().expect("container runtime");
    let cluster = host.block_on(common::cluster("prompt-stop"));
    let prefix = common::fresh_prefix();
    let (stopped_in, backend) = runtime.block_on(async {
        let backend = common::backend(&cluster.bootstrap, &prefix).await;
        let spec = spec("prompt", 1, KEEP);
        provisioned(backend.as_ref(), &spec).await;
        let key = FenceKey::new(Uuid::new_v4().as_u128());
        append(backend.as_ref(), fence(key, 1), &spec.topic, &[event(1)]).await;
        let batch: Vec<_> = (100..1100).map(event).collect();
        let appending = {
            let backend = Arc::clone(&backend);
            let topic = spec.topic.clone();
            tokio::spawn(async move {
                backend
                    .append(&AppendRequest {
                        fence: fence(key, 2),
                        topic: &topic,
                        events: &batch,
                        flush: Flush::Sync,
                    })
                    .await
                    .map(|_| ())
            })
        };
        tokio::time::sleep(Duration::from_millis(2)).await;
        let deadline = CancellationToken::new();
        deadline.cancel();
        let started = std::time::Instant::now();
        backend.stop(deadline).await;
        let stopped_in = started.elapsed();
        appending.abort();
        (stopped_in, backend)
    });
    drop(backend);
    let started = std::time::Instant::now();
    drop(runtime);
    let shutdown_in = started.elapsed();
    // Removed from a worker thread: the container's drop blocks in place.
    host.block_on(host.spawn(async move { drop(cluster) }))
        .expect("container removed");

    assert!(
        stopped_in < Duration::from_millis(200),
        "stop took {stopped_in:?}"
    );
    assert!(
        shutdown_in < Duration::from_secs(2),
        "shutdown took {shutdown_in:?}"
    );
}

//! Helpers every scenario builds on.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::support::poll_heads;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, Head,
    KeyedEvent, LogClaim, LogId, Partition, Provisioning, Replacing, Retention, TopicPartition,
    TopicSpec, Wait, Watch,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position, Sequence};
use futures_util::stream::BoxStream;
use uuid::Uuid;

/// A topic no other scenario uses.
pub fn topic(scenario: &str) -> GtsInstanceId {
    let unique = Uuid::new_v4().simple().to_string();
    GtsInstanceId::try_new(&format!(
        "{}cf.conformance.{scenario}.t{}.v1",
        toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        &unique[..12]
    ))
    .expect("a scenario topic is a valid GTS instance id")
}

/// An event whose payload names its key and its place in the scenario's
/// publish order, claiming to have occurred now.
pub fn event(key: &str, n: u64) -> KeyedEvent {
    event_at(key, n, Utc::now())
}

/// The same, claiming to have occurred at `occurred_at`.
pub fn event_at(key: &str, n: u64, occurred_at: DateTime<Utc>) -> KeyedEvent {
    KeyedEvent {
        key: key.to_owned(),
        event: Event {
            id: Uuid::new_v4(),
            type_id: GtsTypeId::new(toolkit_gts::gts_id!(
                "cf.core.events.event.v1~cf.conformance.x.happened.v1~"
            )),
            tenant_id: Uuid::from_u128(0x7e4a_0001),
            source: "conformance".to_owned(),
            subject: format!("{key}-{n}"),
            subject_type: GtsTypeId::new(toolkit_gts::gts_id!("cf.conformance.x.thing.v1~")),
            occurred_at,
            trace_parent: None,
            data: Some(serde_json::json!({ "key": key, "n": n })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

/// Retention long enough that nothing a scenario writes is removed by it.
pub const KEEP: Retention = Retention {
    max_age: Some(Duration::from_hours(24)),
    max_bytes: None,
};

/// Appends under one fence key of the scenario's own, each call under the next
/// seq.
pub struct Writer {
    key: FenceKey,
    seq: AtomicI64,
}

impl Writer {
    /// A writer under a fresh fence key, starting at seq 1.
    pub fn new() -> Self {
        Self {
            key: FenceKey::new(Uuid::new_v4().as_u128()),
            seq: AtomicI64::new(0),
        }
    }

    /// The fence the next [`append`](Self::append) uses.
    pub fn next_fence(&self) -> Fence {
        Fence {
            key: self.key,
            seq: FenceSeq::new(self.seq.fetch_add(1, Ordering::SeqCst) + 1),
        }
    }

    /// Appends `events` under the next fence; panics if the backend refuses.
    pub async fn append(
        &self,
        backend: &dyn Backend,
        topic: &GtsInstanceId,
        events: &[KeyedEvent],
    ) -> Appended {
        append_fenced(backend, self.next_fence(), topic, events).await
    }
}

/// Appends `events` under `fence` with a synchronous flush; panics if the
/// backend refuses.
pub async fn append_fenced(
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

/// `backend` started and answering: [`Backend::start`], then `resolve` until
/// the medium answers something other than `Unavailable`, for up to a minute.
pub async fn started(backend: Arc<dyn Backend>) -> Arc<dyn Backend> {
    backend.start().await.expect("start");
    let probe = TopicPartition::new(topic("probe"), Partition::new(0));
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    while let Err(BackendError::Unavailable { .. }) =
        backend.resolve(&probe, Position::Latest).await
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "still Unavailable a minute after start"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    backend
}

/// Provisions a fresh topic for `scenario` with `partitions` partitions and
/// `retention`, under a fresh log id, waiting out `Pending`.
pub async fn provisioned(
    backend: &dyn Backend,
    scenario: &str,
    partitions: u32,
    retention: Retention,
) -> TopicSpec {
    let spec = TopicSpec {
        topic: topic(scenario),
        partitions,
        retention,
    };
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: LogId::new_v4(),
    };
    settle(backend, &spec, &claim).await.expect("provision");
    spec
}

/// `provision` until it stops answering `Pending`.
pub async fn settle(
    backend: &dyn Backend,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<u32, event_broker_sdk::backend::BackendError> {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    loop {
        match backend.provision(spec, claim).await? {
            Provisioning::Ready { partitions } => return Ok(partitions),
            Provisioning::Pending => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "provisioning stayed Pending for 60s"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// The head of every partition of `spec`: from `Heads` where `appended`
/// reported one, from `resolve(Latest)` otherwise.
pub async fn heads_after(
    backend: &dyn Backend,
    spec: &TopicSpec,
    appended: &Appended,
) -> Vec<Sequence> {
    let mut heads = Vec::new();
    for partition in (0..spec.partitions).map(Partition::new) {
        let at = TopicPartition::new(spec.topic.clone(), partition);
        let reported = match appended {
            Appended::Heads(heads) => heads.iter().find(|h| h.at == at).map(|h| h.sequence),
            Appended::Accepted | Appended::Duplicate => None,
        };
        heads.push(match reported {
            Some(head) => head,
            None => backend
                .resolve(&at, Position::Latest)
                .await
                .expect("resolve"),
        });
    }
    heads
}

/// Everything stored in `at`, oldest first.
pub async fn read_all(backend: &dyn Backend, at: &TopicPartition) -> Vec<Event> {
    let mut all = Vec::new();
    let mut offset = Sequence::NONE;
    loop {
        let batch = backend.read(at, offset, 500, Wait::No).await.expect("read");
        let Some(last) = batch.last() else {
            return all;
        };
        offset = last.sequence.expect("a read event carries its sequence");
        all.extend(batch);
    }
}

/// Reads `at` until it holds `count` events or 10 s pass: a backend answering
/// `Accepted` makes events readable later.
pub async fn wait_for_count(
    backend: &dyn Backend,
    at: &TopicPartition,
    count: usize,
) -> Vec<Event> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = read_all(backend, at).await;
        if events.len() >= count || tokio::time::Instant::now() >= deadline {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `(key, n)` of each event, from the payload [`event`] wrote.
pub fn keys_and_ns(events: &[Event]) -> Vec<(String, u64)> {
    events
        .iter()
        .map(|e| {
            let data = e.data.as_ref().expect("payload");
            (
                data["key"].as_str().expect("key").to_owned(),
                data["n"].as_u64().expect("n"),
            )
        })
        .collect()
}

/// The sequence of each event, in order.
pub fn sequences(events: &[Event]) -> Vec<i64> {
    events
        .iter()
        .map(|e| e.sequence.expect("sequence").as_i64())
        .collect()
}

/// Whether `sequences` are what the contract promises of one partition's
/// events in read order: each at least 1 and each above the one before. Gaps
/// are allowed, so nothing ties a sequence to an event count.
pub fn rising_from_one(sequences: &[i64]) -> bool {
    sequences.first().is_none_or(|first| *first >= 1)
        && sequences.windows(2).all(|pair| pair[0] < pair[1])
}

/// `resolve(Latest)` of `at` once it rises above `previous`, for up to 10 s: a
/// backend answering `Accepted` assigns the sequence later.
pub async fn latest_past(
    backend: &dyn Backend,
    at: &TopicPartition,
    previous: Sequence,
) -> Sequence {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let latest = backend
            .resolve(at, Position::Latest)
            .await
            .expect("resolve");
        if latest > previous {
            return latest;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Latest still {latest:?} 10s after an append past {previous:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A head stream over `partitions`, native or polled - whichever the backend
/// declares.
pub fn head_stream(
    backend: &Arc<dyn Backend>,
    partitions: &[TopicPartition],
) -> BoxStream<'static, Result<Head, event_broker_sdk::backend::BackendError>> {
    match backend.watch() {
        Watch::Native(watch) => watch.watch(partitions),
        Watch::Poll(interval) => poll_heads(Arc::clone(backend), partitions, interval),
    }
}

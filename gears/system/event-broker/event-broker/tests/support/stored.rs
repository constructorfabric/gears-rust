//! What a storage backend holds, read back for a test's final check.

use std::time::Duration;

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{Backend, Partition, TopicPartition, Wait};
use gts::GtsInstanceId;

/// The ids of every event `backend` holds on `topic`, partition by partition
/// in sequence order, once at least `expected` have drained into it (or after
/// thirty seconds, with whatever is there).
pub async fn stored_ids(
    backend: &dyn Backend,
    topic: &str,
    partitions: u32,
    expected: usize,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let mut ids = Vec::new();
        for partition in 0..partitions {
            let at = TopicPartition::new(
                GtsInstanceId::try_new(topic).expect("a valid topic id"),
                Partition::new(partition),
            );
            let mut offset = Sequence::NONE;
            loop {
                let events = backend
                    .read(&at, offset, 1024, Wait::No)
                    .await
                    .expect("read the stored events");
                let Some(last) = events.last() else {
                    break;
                };
                offset = last.sequence.expect("a stored event has a sequence");
                ids.extend(events.iter().map(|event| event.id.to_string()));
            }
        }
        if ids.len() >= expected || tokio::time::Instant::now() >= deadline {
            return ids;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

//! Segment introspection (feature 0003): the envelope of a matched range.

use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{Partition, TopicPartition};
use event_broker_sdk::models::{PartitionRange, TopicSegment};

use crate::fixture::{KEEP, Writer, event, provisioned, sequences, started, wait_for_count};
use crate::harness::Harness;

/// The contract fields of each returned segment: `segments` is the backend's
/// own (feature 0003), so it is left out.
type Envelope = (
    Sequence,
    Sequence,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

fn envelopes(segments: &[TopicSegment]) -> Vec<Envelope> {
    segments
        .iter()
        .map(|s| (s.start_sequence, s.end_sequence, s.start_time, s.end_time))
        .collect()
}

/// `start_sequence`/`end_sequence` are the first and last event the range
/// and the limit match, `start_time`/`end_time` those events' storage times;
/// a range past the head matches nothing.
pub async fn query_envelope_is_the_first_and_last_event_in_range(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "query_envelope", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let everything = PartitionRange {
        start_offset: None,
        end_offset: None,
        limit: u32::MAX,
    };
    let empty = backend.query(&at, everything).await.expect("query");
    assert_eq!(envelopes(&empty), Vec::<Envelope>::new());
    // One append per event, 2 ms apart, so the storage times differ even at
    // millisecond resolution.
    let writer = Writer::new();
    for n in 0..10 {
        writer
            .append(backend.as_ref(), &spec.topic, &[event("k", n)])
            .await;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let stored = wait_for_count(backend.as_ref(), &at, 10).await;
    assert_eq!(stored.len(), 10, "{stored:?}");
    let all = sequences(&stored);
    let seq = |i: usize| Sequence::assigned(all[i]);
    let time = |i: usize| stored[i].sequence_time;
    let query = |range: PartitionRange| {
        let backend = backend.clone();
        let at = at.clone();
        async move { envelopes(&backend.query(&at, range).await.expect("query")) }
    };

    assert_eq!(
        query(everything).await,
        vec![(seq(0), seq(9), time(0), time(9))]
    );
    // The ends' storage times differ, so a backend answering the range's
    // earliest time as `end_time` fails here.
    assert_ne!(time(2), time(6), "{stored:?}");
    assert_eq!(
        query(PartitionRange {
            start_offset: Some(seq(2)),
            end_offset: Some(seq(6)),
            limit: u32::MAX,
        })
        .await,
        vec![(seq(2), seq(6), time(2), time(6))]
    );
    assert_eq!(
        query(PartitionRange {
            start_offset: Some(seq(2)),
            end_offset: None,
            limit: 2,
        })
        .await,
        vec![(seq(2), seq(3), time(2), time(3))]
    );
    assert_eq!(
        query(PartitionRange {
            start_offset: Some(Sequence::assigned(all[9] + 1)),
            end_offset: None,
            limit: u32::MAX,
        })
        .await,
        Vec::<Envelope>::new()
    );
}

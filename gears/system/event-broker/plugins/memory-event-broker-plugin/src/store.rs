//! The state one memory store holds, and every operation on it.
//!
//! One lock covers events, heads and fence state, so an append's fence check,
//! its sequence assignment and its fence advance are one atomic step, and an
//! event is visible exactly when its sequence is assigned.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::support::{LocalHeads, partition_for};
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, FenceKey, FenceSeq, Head, LogClaim, LogId, Partition,
    Provisioning, Retention, TopicPartition, TopicSpec,
};
use event_broker_sdk::{Event, GtsInstanceId, PartitionRange, Position, Sequence, TopicSegment};

/// Bytes counted per event on top of its variable-length fields: the fixed
/// fields and bookkeeping a stored event costs.
const FIXED_EVENT_BYTES: u64 = 64;

/// The logs and fences of one named store, shared by every entry naming it.
#[derive(Default)]
pub struct Store {
    state: Mutex<State>,
    /// Heads committed to this store, feeding its watches and waiting reads.
    pub heads: LocalHeads,
}

#[derive(Default)]
struct State {
    logs: HashMap<GtsInstanceId, Log>,
    /// Last applied seq per fence key.
    fences: HashMap<FenceKey, FenceSeq>,
}

struct Log {
    id: LogId,
    retention: Retention,
    partitions: Vec<PartitionLog>,
}

#[derive(Default)]
struct PartitionLog {
    /// Highest sequence ever assigned; never lowered by removal.
    head: i64,
    events: VecDeque<StoredEvent>,
    stored_bytes: u64,
}

struct StoredEvent {
    event: Event,
    bytes: u64,
}

impl Store {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the topic's log for `claim`; `LogConflict` when the stored log
    /// id is not one the claim accepts.
    pub fn provision(
        &self,
        spec: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        let mut state = self.state();
        let stored = state.logs.get(&spec.topic).map(|log| log.id);
        if !claim.accepts(stored) {
            return Err(BackendError::log_conflict("provision"));
        }
        let log = state.logs.entry(spec.topic.clone()).or_insert_with(|| Log {
            id: claim.new,
            retention: spec.retention,
            partitions: (0..spec.partitions.max(1))
                .map(|_| PartitionLog::default())
                .collect(),
        });
        log.id = claim.new;
        log.retention = spec.retention;
        Ok(Provisioning::Ready {
            partitions: partition_count(log),
        })
    }

    /// Stores the request's events and answers with the heads it assigned,
    /// which the caller publishes once the lock is released.
    pub fn append(
        &self,
        request: &AppendRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<Appended, BackendError> {
        let mut state = self.state();
        let fence_key = request.fence.key;
        if state
            .fences
            .get(&fence_key)
            .is_some_and(|applied| *applied >= request.fence.seq)
        {
            return Ok(Appended::Duplicate);
        }
        let log = state
            .logs
            .get_mut(request.topic)
            .ok_or_else(|| BackendError::not_provisioned("append"))?;
        let count = partition_count(log);
        let mut touched: Vec<Partition> = Vec::new();
        for keyed in request.events {
            let index = partition_for(&keyed.key, count);
            let partition = &mut log.partitions[index.value() as usize];
            partition.head += 1;
            let mut event = keyed.event.clone();
            event.partition = Some(index.value());
            event.sequence = Some(Sequence::assigned(partition.head));
            event.sequence_time = Some(now);
            event.meta = None;
            let bytes = stored_bytes(&event);
            partition.stored_bytes += bytes;
            partition.events.push_back(StoredEvent { event, bytes });
            if !touched.contains(&index) {
                touched.push(index);
            }
        }
        touched.sort_unstable();
        let heads = touched
            .into_iter()
            .map(|index| Head {
                at: TopicPartition::new(request.topic.clone(), index),
                sequence: Sequence::assigned(log.partitions[index.value() as usize].head),
            })
            .collect();
        state.fences.insert(fence_key, request.fence.seq);
        Ok(Appended::Heads(heads))
    }

    /// Up to `max_events` stored events after `offset`.
    pub fn read(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        max_events: usize,
    ) -> Result<Vec<Event>, BackendError> {
        let state = self.state();
        let partition = partition(&state, at, "read")?;
        Ok(partition
            .events
            .iter()
            .filter(|stored| stored_sequence(stored) > offset.as_i64())
            .take(max_events)
            .map(|stored| stored.event.clone())
            .collect())
    }

    /// The sequence `position` names, checked against the partition's floor
    /// and head.
    pub fn resolve(
        &self,
        at: &TopicPartition,
        position: &Position,
    ) -> Result<Sequence, BackendError> {
        let state = self.state();
        let partition = partition(&state, at, "resolve")?;
        let ceiling = Sequence::assigned(partition.head);
        // One below the oldest stored event; with nothing stored, the ceiling
        // is the only position a cursor can hold.
        let floor = partition.events.front().map_or(ceiling, |oldest| {
            Sequence::assigned(stored_sequence(oldest) - 1)
        });
        match position {
            Position::Latest => Ok(ceiling),
            Position::Earliest => Ok(floor),
            Position::At(at) => Ok(partition
                .events
                .iter()
                .find(|stored| {
                    stored
                        .event
                        .sequence_time
                        .is_some_and(|stored_at| stored_at >= *at)
                })
                .map_or(ceiling, |stored| {
                    Sequence::assigned(stored_sequence(stored) - 1)
                })),
            Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
            Position::Exact(requested) => {
                Err(BackendError::out_of_range(*requested, floor, ceiling))
            }
        }
    }

    /// The current head of each named partition, for a watch's first items.
    pub fn heads(&self, partitions: &[TopicPartition]) -> Result<Vec<Head>, BackendError> {
        let state = self.state();
        partitions
            .iter()
            .map(|at| {
                partition(&state, at, "watch").map(|partition| Head {
                    at: at.clone(),
                    sequence: Sequence::assigned(partition.head),
                })
            })
            .collect()
    }

    /// The matched range as one synthetic segment; this backend has no real
    /// segments.
    pub fn query(
        &self,
        at: &TopicPartition,
        range: &PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        let state = self.state();
        let partition = partition(&state, at, "query")?;
        let start = range.start_offset.map_or(i64::MIN, Sequence::as_i64);
        let end = range.end_offset.map_or(i64::MAX, Sequence::as_i64);
        let matched: Vec<&StoredEvent> = partition
            .events
            .iter()
            .filter(|stored| (start..=end).contains(&stored_sequence(stored)))
            .take(usize::try_from(range.limit).unwrap_or(usize::MAX))
            .collect();
        let (Some(first), Some(last)) = (matched.first(), matched.last()) else {
            return Ok(Vec::new());
        };
        let start_sequence = Sequence::assigned(stored_sequence(first));
        let end_sequence = Sequence::assigned(stored_sequence(last));
        // One synthetic segment over the matched range: this backend has no
        // real segments.
        Ok(vec![TopicSegment {
            topic: at.topic.to_string(),
            partition: at.partition.value(),
            start_sequence,
            end_sequence,
            start_time: first.event.sequence_time,
            end_time: last.event.sequence_time,
            segments: vec![serde_json::json!({
                "start_sequence": start_sequence.as_i64(),
                "end_sequence": end_sequence.as_i64(),
                "event_count": matched.len(),
            })],
        }])
    }

    /// One retention pass over every partition of every topic: removes, oldest
    /// first, events stored longer ago than the topic's `max_age`, then events
    /// until the partition holds at most `max_bytes`.
    ///
    /// Age is the event's `sequence_time` - when this backend stored it - never
    /// its `occurred_at`: that is the producer's claim, and a malfunctioning
    /// producer could set any time, deleting an event on arrival or keeping it
    /// forever.
    pub fn enforce_retention(&self, now: DateTime<Utc>) {
        let mut state = self.state();
        for log in state.logs.values_mut() {
            let oldest_permitted = log
                .retention
                .max_age
                .and_then(|age| chrono::Duration::from_std(age).ok())
                .map(|age| now - age);
            for partition in &mut log.partitions {
                if let Some(oldest_permitted) = oldest_permitted {
                    partition.remove_while(|stored, _| {
                        stored
                            .event
                            .sequence_time
                            .is_some_and(|stored_at| stored_at < oldest_permitted)
                    });
                }
                if let Some(max_bytes) = log.retention.max_bytes {
                    partition.remove_while(|_, stored_bytes| stored_bytes > max_bytes);
                }
            }
        }
    }

    /// Removes every event of `at` up to and including `through`, as retention
    /// would; the head stays where it is.
    pub fn truncate(&self, at: &TopicPartition, through: Sequence) -> Result<(), BackendError> {
        let mut state = self.state();
        let log = state
            .logs
            .get_mut(&at.topic)
            .ok_or_else(|| BackendError::not_provisioned("truncate"))?;
        let partition = log
            .partitions
            .get_mut(at.partition.value() as usize)
            .ok_or_else(|| partition_not_found("truncate"))?;
        partition.remove_while(|stored, _| stored_sequence(stored) <= through.as_i64());
        Ok(())
    }
}

impl PartitionLog {
    /// Removes events from the front while `expired(oldest, stored_bytes)`
    /// holds, so what remains is always a suffix of what was stored.
    fn remove_while(&mut self, expired: impl Fn(&StoredEvent, u64) -> bool) {
        while let Some(oldest) = self.events.front() {
            if !expired(oldest, self.stored_bytes) {
                return;
            }
            self.stored_bytes -= oldest.bytes;
            self.events.pop_front();
        }
    }
}

fn partition<'s>(
    state: &'s State,
    at: &TopicPartition,
    operation: &str,
) -> Result<&'s PartitionLog, BackendError> {
    state
        .logs
        .get(&at.topic)
        .ok_or_else(|| BackendError::not_provisioned(operation))?
        .partitions
        .get(at.partition.value() as usize)
        .ok_or_else(|| partition_not_found(operation))
}

fn partition_not_found(operation: &str) -> BackendError {
    BackendError::PartitionNotFound {
        detail: format!("{operation}: the topic has no such partition on this backend"),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn partition_count(log: &Log) -> u32 {
    // Built from a `u32` count, so it fits.
    log.partitions.len() as u32
}

fn stored_sequence(stored: &StoredEvent) -> i64 {
    stored.event.sequence.map_or(0, Sequence::as_i64)
}

fn stored_bytes(event: &Event) -> u64 {
    let data = event.data.as_ref().map_or(0, |data| {
        serde_json::to_vec(data).map_or(0, |bytes| bytes.len())
    });
    let variable = event.source.len()
        + event.subject.len()
        + event.type_id.as_ref().len()
        + event.subject_type.as_ref().len()
        + event.trace_parent.as_ref().map_or(0, String::len)
        + data;
    FIXED_EVENT_BYTES + u64::try_from(variable).unwrap_or(u64::MAX)
}

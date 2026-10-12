//! What the partition cache's two hot paths cost, and whether either scales
//! with something it should not.
//!
//! Run with `cargo bench -p cf-gears-event-broker --bench partition_cache`.
//!
//! Both benchmarks guard a scaling property that only a curve reveals.
//! `absorb_into_resident` sweeps how much the cache is already holding:
//! absorbing must not touch resident events, so cost must not rise with
//! residency, and the adjacent-segment case the sweep drives is the steady
//! state. `read_one_batch` sweeps payload size: an event's size comes from a
//! cumulative index summed once at absorb, never from serializing the payload
//! per read, so cost must not rise with payload bytes.
//!
//! A flat line is the result for both. A rising one means one of those two
//! properties has been lost.
//!
//! `read_across_segments` is the counterweight, and it is *not* flat by design:
//! never merging means a read spanning several fetches has to visit each of
//! them, and it measures what that costs. Around thirty nanoseconds per extra
//! segment crossed, against a per-absorb saving that grows without bound with
//! residency, is the trade this design makes.

#![allow(clippy::expect_used)]

use chrono::Utc;
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;
use uuid::Uuid;

use event_broker::domain::model::{Event, Sequence};
use event_broker::domain::streaming::read::{MaxBytes, MaxEvents, ReadLimit};
use event_broker::infra::partition_cache::cache::{AbsorbedFetch, PartitionCache};
use event_broker::infra::partition_cache::reclaim::{
    GapThresholdEvents, ReclaimPolicy, ResidencyLimitBytes,
};
use event_broker::infra::partition_cache::segment::Segment;
use gts::GtsTypeId;
use toolkit_gts::GtsInstanceId;

/// Events per fetch, and so events per resident segment.
const BATCH: i64 = 256;

fn event(sequence: i64, payload_bytes: usize) -> Event {
    let sequence = Sequence::assigned(sequence);
    Event {
        id: Uuid::nil(),
        r#type: GtsTypeId::try_new("gts.cf.core.events.event.v1~x.eb.o.created.v1~")
            .expect("static gts type id is valid"),
        topic: GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1")
            .expect("static gts id is valid"),
        tenant_id: Uuid::nil(),
        source: "bench".to_owned(),
        subject: "order".to_owned(),
        subject_type: GtsTypeId::new("gts.x.eb.test.subject.v1~"),
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!({ "n": sequence, "body": "x".repeat(payload_bytes) }),
        meta: None,
        partition: Some(0),
        sequence: Some(sequence),
        sequence_time: None,
    }
}

/// A cache that never reclaims, so a measurement sees only the path under test.
///
/// No reader is registered, so nothing is ever dead or gapped, and the byte
/// limit is wide enough that absorbing never trims.
fn quiescent_cache() -> std::sync::Arc<PartitionCache> {
    PartitionCache::with_reclaim_policy(ReclaimPolicy::new(
        GapThresholdEvents(usize::MAX),
        ResidencyLimitBytes(usize::MAX),
    ))
}

fn absorb_span(cache: &PartitionCache, from: i64, through: i64, payload_bytes: usize) {
    let segment = Segment::builder()
        .from(Sequence::assigned(from))
        .through(Sequence::assigned(through))
        .events(
            (from..=through)
                .map(|at| event(at, payload_bytes))
                .collect(),
        )
        .build();
    cache.absorb(AbsorbedFetch::builder(segment).build());
}

/// A cache holding `resident` events, plus the sequence a further fetch would
/// start at. Untimed setup.
fn prefilled(resident: i64, payload_bytes: usize) -> (std::sync::Arc<PartitionCache>, i64) {
    let cache = quiescent_cache();
    let mut produced: i64 = 0;
    while produced < resident {
        let from = produced + 1;
        let through = from + BATCH - 1;
        absorb_span(&cache, from, through, payload_bytes);
        produced = through;
    }
    (cache, produced + 1)
}

fn batch_limit() -> ReadLimit {
    ReadLimit::new(
        MaxEvents(usize::try_from(BATCH).unwrap_or(usize::MAX)),
        MaxBytes(1024 * 1024),
    )
}

/// Absorbing one fetch adjacent to what is already resident, swept by how much
/// that is. Flat is the result; rising means adjacency has become physical
/// again.
fn absorb_into_resident(c: &mut Criterion) {
    let mut group = c.benchmark_group("absorb_into_resident");
    // Setup builds the resident state and dwarfs the measurement, so fewer,
    // larger samples rather than criterion's default hundred.
    group.sample_size(20);

    for resident in [256, 1024, 4096, 8192, 16384] {
        group.bench_with_input(
            BenchmarkId::from_parameter(resident),
            &resident,
            |b, &resident| {
                b.iter_batched(
                    || {
                        // The fetch is built here, not in the routine:
                        // constructing 256 events costs orders of magnitude
                        // more than absorbing them, and would bury the curve
                        // this benchmark exists to show.
                        let (cache, next) = prefilled(resident, 1024);
                        let segment = Segment::builder()
                            .from(Sequence::assigned(next))
                            .through(Sequence::assigned(next + BATCH - 1))
                            .events((next..next + BATCH).map(|at| event(at, 1024)).collect())
                            .build();
                        (cache, AbsorbedFetch::builder(segment).build())
                    },
                    |(cache, fetch)| {
                        cache.absorb(fetch);
                        cache
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

/// Serving one batch to one reader, swept by payload size. Flat is the result;
/// rising with payload means the read path is measuring bytes it should have
/// been told.
fn read_one_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_one_batch");

    for payload_bytes in [256, 1024, 8192] {
        let (cache, _) = prefilled(8192, payload_bytes);
        // No throughput figure on purpose: a read hands back a borrowed view
        // rather than copying events, so an events-per-second rate would invite
        // reading it as a processing rate it is not.
        group.bench_with_input(
            BenchmarkId::from_parameter(payload_bytes),
            &payload_bytes,
            |b, _| {
                // Always the same 256 events from the head of the resident
                // span, so only the payload size varies between runs.
                b.iter(|| cache.read_from(Sequence::assigned(0), batch_limit()));
            },
        );
    }
    group.finish();
}

/// Serving one read swept by how many segments the walk has to cross, which is
/// the cost never merging introduces.
///
/// Residency is fixed and the *read limit* is swept, because sweeping residency
/// would not cross anything: a 256-event limit fills from the first segment
/// whether there is one behind it or sixty-three.
fn read_across_segments(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_across_segments");
    let (cache, _) = prefilled(16384, 1024);

    for wanted in [256_usize, 1024, 4096] {
        let segments_crossed = wanted
            .checked_div(usize::try_from(BATCH).unwrap_or(1))
            .unwrap_or(0);
        group.bench_with_input(
            BenchmarkId::new("segments", segments_crossed),
            &wanted,
            |b, &wanted| {
                let limit = ReadLimit::new(MaxEvents(wanted), MaxBytes(64 * 1024 * 1024));
                b.iter(|| cache.read_from(Sequence::assigned(0), limit));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    absorb_into_resident,
    read_one_batch,
    read_across_segments
);
criterion_main!(benches);

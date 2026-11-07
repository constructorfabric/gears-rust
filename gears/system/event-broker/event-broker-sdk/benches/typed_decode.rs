//! Typed vs raw per-event cost on the consumer path (design-r6 section 7, "Measurement").
//!
//! The typed handler adapters are `pub(crate)`, so this target measures the one step they
//! add per event through the public API. A raw single-event handler receives an
//! `EventV1::clone()`; a typed one receives `EventV1::decode::<P>()`. A raw batch handler
//! borrows the buffered batch (no per-event work); a typed one decodes each event into a
//! new `Vec`.
//!
//! - Single: `clone()` vs `decode::<P>()` at 4 KiB and 256 KiB of `data`. The verdict is
//!   `raw_median / typed_median >= 0.90`.
//! - Batch: the cost of decoding one 128-event batch (ns per batch and per event). The 90%
//!   ratio does not apply, since the raw batch path does no per-event work.
//! - Memory: the resident-set growth over one decoded 128-event batch, against
//!   `1.1 x 128 x S`.
//!
//! Run: `cargo bench -p cf-gears-event-broker-sdk --bench typed_decode`. Without `--bench`
//! (as under `cargo test --benches`) only the memory pass and the correctness checks run.

use std::hint::black_box;
use std::time::{Duration, Instant};

use event_broker_sdk::{EventV1, GtsTypeId};
use gts::GtsSchema;
use toolkit_gts::{gts_id, gts_type_schema};
use uuid::Uuid;

const BATCH: usize = 128;
const SIZES: [usize; 2] = [4_096, 262_144];
const WARM_UP: Duration = Duration::from_secs(1);
const SAMPLES: usize = 15;
const SAMPLE_MIN: Duration = Duration::from_millis(200);
/// Design 7: typed throughput must stay at or above this share of raw.
const MIN_RATIO: f64 = 0.90;
/// Design 7: peak memory over a decoded batch may exceed `BATCH x S` by this factor.
const MEMORY_FACTOR: f64 = 1.1;
const LINE_LEN: usize = 64;

/// The bench payload: a declared event type whose `data` is large through `lines`.
#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~fabrikam.bench.orders.order_lines.v1~"),
    description = "Bench payload: an order with many text lines.",
    properties = "order_id,total_cents,currency,lines",
    traits = serde_json::json!({
        "topic": gts_id!("cf.core.events.topic.v1~fabrikam.bench._.orders.v1"),
        "allowed_subject_types": ["gts.fabrikam.bench.orders.order.v1~"],
    }),
)]
pub struct OrderLinesV1 {
    pub order_id: String,
    pub total_cents: i64,
    pub currency: String,
    pub lines: Vec<String>,
}

/// Reads this process's resident set.
struct Rss;

impl Rss {
    /// Resident set in bytes, from `/proc` (Linux); `None` where it cannot be read or
    /// parsed. Memory the allocator reuses after a free can make a delta under-report.
    fn read() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("VmRSS:"))
                    .and_then(|kb| kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
    }
}

/// Raw events of one payload size.
struct Fixture {
    /// Requested size of the serialized `data`.
    target: usize,
    /// Exact size of `serde_json::to_vec(&data)`.
    size: usize,
    data: serde_json::Value,
}

impl Fixture {
    fn new(target: usize) -> Self {
        let payload_with = |lines: usize| OrderLinesV1 {
            order_id: "order-0001".to_owned(),
            total_cents: 429_900,
            currency: "EUR".to_owned(),
            lines: (0..lines)
                .map(|i| format!("{i:0>width$}", width = LINE_LEN))
                .collect(),
        };
        let json_len = |lines: usize| {
            serde_json::to_vec(&::gts::GtsSerializeWrapper(&payload_with(lines)))
                .expect("payload serializes")
                .len()
        };
        let base = json_len(0);
        let per_line = json_len(1) - base;
        let lines = (target - base) / per_line;
        let data = serde_json::to_value(::gts::GtsSerializeWrapper(&payload_with(lines)))
            .expect("payload serializes");
        let size = serde_json::to_vec(&data).expect("data serializes").len();
        let (low, high) = (target * 95 / 100, target * 105 / 100);
        assert!(
            (low..=high).contains(&size),
            "data is {size} bytes, outside 5% of {target}"
        );
        Self { target, size, data }
    }

    /// The `i`-th raw event: fixed ids and times, so every run builds the same input.
    fn event(&self, i: usize) -> EventV1 {
        let n = u128::try_from(i).expect("index fits");
        EventV1::builder_for(GtsTypeId::new(OrderLinesV1::TYPE_ID), self.data.clone())
            .id(Uuid::from_u128(
                0x0193_0000_0000_7000_8000_0000_0000_0000 + n,
            ))
            .tenant_id(Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_ffff))
            .source("bench")
            .subject(format!("order-{i}"))
            .subject_type_id(GtsTypeId::new("gts.fabrikam.bench.orders.order.v1~"))
            .occurred_at(
                chrono::DateTime::from_timestamp(1_793_000_000, 0).expect("fixed timestamp"),
            )
            .build()
            .expect("the fixture event is valid")
    }

    fn batch(&self) -> Vec<EventV1> {
        (0..BATCH).map(|i| self.event(i)).collect()
    }

    /// Panics when the typed route does not give back what the raw event carries.
    fn check(&self) {
        let raw = self.event(0);
        let typed = raw.decode::<OrderLinesV1>().expect("decode is Ok");
        let back = typed
            .data()
            .map(|payload| serde_json::to_value(::gts::GtsSerializeWrapper(payload)))
            .transpose()
            .expect("decoded data serializes");
        assert_eq!(back.as_ref(), Some(&self.data), "data round-trips");
        assert_eq!(typed.id(), raw.id(), "id is carried");
        assert_eq!(typed.event_type(), raw.event_type(), "type is carried");

        let batch = self.batch();
        let decoded: Vec<_> = batch.iter().map(EventV1::decode::<OrderLinesV1>).collect();
        assert_eq!(decoded.len(), BATCH);
        assert!(decoded.iter().all(Result::is_ok), "all {BATCH} decode Ok");
    }

    /// Resident-set growth over decoding one batch kept alive, and the design bound.
    fn memory(&self) -> Memory {
        let batch = self.batch();
        let before = Rss::read();
        let decoded: Vec<_> = batch
            .iter()
            .map(|event| event.decode::<OrderLinesV1>().expect("decode is Ok"))
            .collect();
        let after = Rss::read();
        black_box(&decoded);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a byte count compared as a float"
        )]
        let bound = MEMORY_FACTOR * (BATCH * self.size) as f64;
        Memory {
            delta: before
                .zip(after)
                .map(|(before, after)| after.saturating_sub(before)),
            bound,
        }
    }
}

struct Memory {
    /// `None` when the resident set is unavailable.
    delta: Option<u64>,
    bound: f64,
}

impl Memory {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a byte count compared as a float"
    )]
    fn passes(&self) -> Option<bool> {
        self.delta.map(|delta| delta as f64 <= self.bound)
    }
}

/// ns per operation across the samples of one case.
struct Stats {
    median: f64,
    min: f64,
    max: f64,
}

impl Stats {
    /// Warms `op` up for [`WARM_UP`], then takes [`SAMPLES`] samples of at least
    /// [`SAMPLE_MIN`] each, with the iteration count taken from the warm-up.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "iteration counts and ns values are small and only used as estimates"
    )]
    fn measure(mut op: impl FnMut()) -> Self {
        let warm = Instant::now();
        let mut done = 0_u64;
        while warm.elapsed() < WARM_UP {
            op();
            done += 1;
        }
        let per_op = warm.elapsed().as_secs_f64() / done as f64;
        let iters = ((SAMPLE_MIN.as_secs_f64() / per_op).ceil() as u64).max(1);

        let mut samples: Vec<f64> = (0..SAMPLES)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..iters {
                    op();
                }
                start.elapsed().as_nanos() as f64 / iters as f64
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        Self {
            median: samples[SAMPLES / 2],
            min: samples[0],
            max: samples[SAMPLES - 1],
        }
    }
}

/// One finished timing row for the table.
struct Row {
    case: &'static str,
    size: usize,
    stats: Stats,
    note: String,
}

impl Row {
    #[expect(clippy::print_stdout, reason = "a bench reports on stdout")]
    fn print(&self) {
        println!(
            "{:<14} {:>8} {:>14.0} {:>14.0} {:>14.0}  {}",
            self.case, self.size, self.stats.median, self.stats.min, self.stats.max, self.note
        );
    }
}

#[expect(clippy::print_stdout, reason = "a bench reports on stdout")]
fn main() {
    let timing = std::env::args().any(|arg| arg == "--bench");
    let fixtures: Vec<Fixture> = SIZES.iter().map(|&s| Fixture::new(s)).collect();

    println!(
        "memory (one decoded {BATCH}-event batch, VmRSS delta vs {MEMORY_FACTOR} x {BATCH} x S):"
    );
    for fixture in &fixtures {
        let memory = fixture.memory();
        // Test mode runs this pass too; on Linux the resident set is always readable,
        // so the `n/a` branch is unreachable there.
        let (delta, verdict) = match (memory.delta, memory.passes()) {
            (Some(delta), Some(true)) => (format!("{delta} B"), "PASS"),
            (Some(delta), _) => (format!("{delta} B"), "OVER design 7"),
            (None, _) => ("n/a".to_owned(), ""),
        };
        println!(
            "  S={:>7}  delta={:>13}  bound={:>13.0} B  {}",
            fixture.size, delta, memory.bound, verdict
        );
    }

    // After the memory pass: the checks allocate and free batches, and the allocator
    // would then reuse that memory and hide the delta.
    for fixture in &fixtures {
        println!(
            "payload target {} bytes, exact {}",
            fixture.target, fixture.size
        );
        fixture.check();
    }
    println!("correctness checks: ok");
    if !timing {
        return;
    }

    println!(
        "{:<14} {:>8} {:>14} {:>14} {:>14}  verdict (ns/op)",
        "case", "size", "median", "min", "max"
    );
    for fixture in &fixtures {
        let raw_event = fixture.event(0);
        let raw = Stats::measure(|| {
            black_box(black_box(&raw_event).clone());
        });
        let typed = Stats::measure(|| {
            black_box(
                black_box(&raw_event)
                    .decode::<OrderLinesV1>()
                    .expect("decode is Ok"),
            );
        });
        let ratio = raw.median / typed.median;
        let verdict = if ratio >= MIN_RATIO {
            "PASS"
        } else {
            "BELOW design 7"
        };
        Row {
            case: "single clone",
            size: fixture.target,
            stats: raw,
            note: String::new(),
        }
        .print();
        Row {
            case: "single decode",
            size: fixture.target,
            stats: typed,
            note: format!("ratio raw/typed = {ratio:.3} {verdict}"),
        }
        .print();

        let batch = fixture.batch();
        let stats = Stats::measure(|| {
            let decoded: Vec<_> = black_box(&batch)
                .iter()
                .map(|event| event.decode::<OrderLinesV1>().expect("decode is Ok"))
                .collect();
            black_box(decoded);
        });
        #[expect(clippy::cast_precision_loss, reason = "a small count as a float")]
        let per_event = stats.median / BATCH as f64;
        Row {
            case: "batch decode",
            size: fixture.target,
            stats,
            note: format!("{per_event:.0} ns/event"),
        }
        .print();
    }
}

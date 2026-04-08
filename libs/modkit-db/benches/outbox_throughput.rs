#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::integer_division,
    clippy::let_underscore_must_use,
    clippy::non_ascii_literal,
    clippy::manual_assert,
    dead_code
)]

//! Profile-driven outbox throughput benchmarks.
//!
//! Each benchmark is defined as a [`BenchProfile`] — a declarative struct
//! capturing queue topology, producer mode, instance count, and workload
//! distribution.  A single generic runner ([`run_profile`]) executes any
//! profile against any database engine.
//!
//! **Tiers** (controlled by env vars):
//!   - Validation (default) — 1p1c, 16p16c, 4q×64p. ~100K msgs, ~90s/engine.
//!   - Longhaul (`BENCH_LONGHAUL=1`) — 1M msgs, single-queue.
//!   - Stress  (`BENCH_STRESS=1`)  — multi-queue (10Q/100Q), multi-instance,
//!     and realistic (hot/cold) load distributions. 1M msgs.
//!
//! **Engines**: Postgres, MySQL, MariaDB, SQLite.
//! Engines are selected via Cargo features (`pg`, `mysql`, `sqlite`).
//! SQLite only runs profiles with `num_producers <= 1` (single-writer).
//!
//! **Custom profiles**: `BENCH_PROFILES=/path/to/profiles.json` loads
//! additional profiles. See [`BenchProfile`] for the JSON schema.
//!
//! **Verification**: Every iteration checks per-partition message ordering
//! (DB seq must be strictly monotonic) and completeness (no lost messages).
//! Multi-instance profiles verify that competing outbox instances correctly
//! share partitions via DB-level leases without duplication.
//!
//! **Run examples:**
//!   ```sh
//!   cargo bench --bench outbox_throughput --features preview-outbox,pg
//!   cargo bench --bench outbox_throughput --features preview-outbox,pg -- "16p16c"
//!   BENCH_LONGHAUL=1 cargo bench --bench outbox_throughput --features preview-outbox,pg
//!   BENCH_STRESS=1 cargo bench --bench outbox_throughput --features preview-outbox,pg -- "2i"
//!   ```

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::Notify;

use modkit_db::outbox::{
    EnqueueMessage, Handler, HandlerResult, Outbox, OutboxHandle, OutboxMessage, OutboxProfile,
    Partitions, outbox_migrations,
};
use modkit_db::{ConnectOpts, Db, connect_db, migration_runner::run_migrations_for_testing};
use tokio_util::sync::CancellationToken;

// Global counter — ensures process-wide unique queue names across iterations.
static GLOBAL_ITER_COUNTER: AtomicU64 = AtomicU64::new(0);

const BATCH_SIZE: usize = 100;

// ---------------------------------------------------------------------------
// BenchProfile — single source of truth for benchmark configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchProfile {
    /// Benchmark name shown in criterion output (e.g. "16p16c_single").
    name: String,
    /// Tier controls when the profile runs: validation (always), longhaul, stress.
    #[serde(default)]
    tier: Tier,

    // -- Producer config --
    producer_mode: ProducerMode,
    #[serde(default = "default_producers")]
    num_producers: usize,

    // -- Outbox config --
    #[serde(default = "default_processors")]
    num_processors: usize,
    #[serde(default)]
    maintenance: MaintenanceConfig,

    // -- Queue topology --
    #[serde(default = "default_one")]
    num_queues: usize,
    #[serde(default = "default_partitions")]
    partitions_per_queue: u16,

    // -- Multi-instance --
    /// Number of competing outbox instances sharing the same DB.
    /// Each instance gets its own sequencer/processor pool. Partitions are
    /// claimed via DB-level leases, so instances compete for work.
    #[serde(default = "default_one")]
    num_instances: usize,

    // -- Workload --
    /// Target message count. Automatically rounded up to be evenly divisible
    /// by `num_queues * partitions_per_queue`.
    message_count: usize,
    #[serde(default)]
    load_distribution: LoadDistribution,

    // -- Pool sizing (None = auto-calculate) --
    #[serde(default)]
    pool_size: Option<u32>,

    // -- Criterion settings --
    #[serde(default)]
    criterion: CriterionSettings,
}

fn default_producers() -> usize {
    16
}
fn default_processors() -> usize {
    8
}
fn default_one() -> usize {
    1
}
fn default_partitions() -> u16 {
    64
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Tier {
    #[default]
    Validation,
    Longhaul,
    Stress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProducerMode {
    /// One message per transaction.
    Single,
    /// Custom batch size per transaction.
    SmallBatch(usize),
    /// Full batch per transaction (`BATCH_SIZE` = 100 messages).
    FullBatch,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LoadDistribution {
    /// All queues receive equal share of messages.
    #[default]
    Uniform,
    /// `hot_queues` get ~90% of load, remaining queues share ~10%.
    Realistic { hot_queues: usize },
}

/// Two-tier maintenance worker budget (per outbox instance).
///
/// - `guaranteed`: permits reserved exclusively for sequencer workers.
/// - `shared`: permits shared between sequencers and vacuum workers.
///   Sequencers steal shared permits when vacuum is idle.
///
/// Total sequencer workers = `guaranteed + shared`.
/// Vacuum workers = `shared`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct MaintenanceConfig {
    #[serde(default = "default_guaranteed")]
    guaranteed: usize,
    #[serde(default = "default_shared")]
    shared: usize,
}

fn default_guaranteed() -> usize {
    2
}
fn default_shared() -> usize {
    1
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            guaranteed: 2,
            shared: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct CriterionSettings {
    #[serde(default = "default_samples")]
    samples: usize,
    #[serde(default = "default_measurement_secs")]
    measurement_secs: u64,
    #[serde(default = "default_warmup_secs")]
    warmup_secs: u64,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

fn default_samples() -> usize {
    10
}
fn default_measurement_secs() -> u64 {
    15
}
fn default_warmup_secs() -> u64 {
    2
}
fn default_timeout_secs() -> u64 {
    120
}

impl Default for CriterionSettings {
    fn default() -> Self {
        Self {
            samples: 10,
            measurement_secs: 15,
            warmup_secs: 2,
            timeout_secs: 120,
        }
    }
}

impl BenchProfile {
    fn total_partitions(&self) -> usize {
        self.num_queues * self.partitions_per_queue as usize
    }

    /// Round message_count up to be evenly divisible by total_partitions.
    fn aligned_message_count(&self) -> usize {
        let tp = self.total_partitions();
        let remainder = self.message_count % tp;
        if remainder == 0 {
            self.message_count
        } else {
            self.message_count + tp - remainder
        }
    }

    fn effective_pool_size(&self, is_sqlite: bool) -> u32 {
        if is_sqlite {
            return 1;
        }
        if let Some(size) = self.pool_size {
            return size;
        }
        // Per instance: sequencers (guaranteed + shared) + processor semaphore
        //   + vacuum workers (shared) + cold reconciler (1).
        // Shared across instances: producers + margin.
        let sequencers = self.maintenance.guaranteed + self.maintenance.shared;
        let vacuums = self.maintenance.shared;
        let per_instance = sequencers + self.num_processors + vacuums + 1;
        (per_instance * self.num_instances + self.num_producers + 10) as u32
    }

    fn batch_size(&self) -> usize {
        match self.producer_mode {
            ProducerMode::Single => 1,
            ProducerMode::SmallBatch(n) => n,
            ProducerMode::FullBatch => BATCH_SIZE,
        }
    }
}

// ---------------------------------------------------------------------------
// Predefined profiles
// ---------------------------------------------------------------------------

fn validation_settings() -> CriterionSettings {
    CriterionSettings::default()
}

fn longhaul_settings() -> CriterionSettings {
    CriterionSettings {
        measurement_secs: 120,
        warmup_secs: 1,
        timeout_secs: 300,
        ..validation_settings()
    }
}

fn validation_profiles() -> Vec<BenchProfile> {
    let criterion = validation_settings();
    vec![
        BenchProfile {
            name: "1p1c_single".into(),
            tier: Tier::Validation,
            producer_mode: ProducerMode::Single,
            num_producers: 1,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 10_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "16p16c_single".into(),
            tier: Tier::Validation,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "16p16c_batch".into(),
            tier: Tier::Validation,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "4q16p16c_single".into(),
            tier: Tier::Validation,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 4,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "4q16p16c_batch".into(),
            tier: Tier::Validation,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 4,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
    ]
}

fn longhaul_profiles() -> Vec<BenchProfile> {
    let criterion = longhaul_settings();
    vec![
        BenchProfile {
            name: "1m_16p_single".into(),
            tier: Tier::Longhaul,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "1m_16p_batch".into(),
            tier: Tier::Longhaul,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
    ]
}

fn stress_profiles() -> Vec<BenchProfile> {
    let criterion = longhaul_settings();
    let validation = validation_settings();
    vec![
        BenchProfile {
            name: "10q_1m_single".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 10,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "10q_1m_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 10,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "100q_1m_single".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 32,
            maintenance: MaintenanceConfig {
                guaranteed: 8,
                shared: 2,
            },
            num_queues: 100,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "100q_1m_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 32,
            maintenance: MaintenanceConfig {
                guaranteed: 8,
                shared: 2,
            },
            num_queues: 100,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "100q_realistic_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 32,
            maintenance: MaintenanceConfig {
                guaranteed: 8,
                shared: 2,
            },
            num_queues: 100,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 1,
            load_distribution: LoadDistribution::Realistic { hot_queues: 8 },
            pool_size: None,
            criterion,
        },
        // --- Multi-instance tier (competing outbox instances) ---
        BenchProfile {
            name: "2i_16p16c_single".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 2,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion: validation,
        },
        BenchProfile {
            name: "2i_16p16c_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 2,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion: validation,
        },
        BenchProfile {
            name: "2i_4q_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 4,
            partitions_per_queue: 64,
            message_count: 100_000,
            num_instances: 2,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion: validation,
        },
        BenchProfile {
            name: "2i_1m_single".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::Single,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 2,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
        BenchProfile {
            name: "2i_1m_batch".into(),
            tier: Tier::Stress,
            producer_mode: ProducerMode::FullBatch,
            num_producers: 16,
            num_processors: 8,
            maintenance: MaintenanceConfig::default(),
            num_queues: 1,
            partitions_per_queue: 64,
            message_count: 1_000_000,
            num_instances: 2,
            load_distribution: LoadDistribution::Uniform,
            pool_size: None,
            criterion,
        },
    ]
}

fn builtin_profiles() -> Vec<BenchProfile> {
    let mut profiles = validation_profiles();
    profiles.extend(longhaul_profiles());
    profiles.extend(stress_profiles());
    profiles
}

fn load_profiles() -> Vec<BenchProfile> {
    let mut profiles = builtin_profiles();

    // Load additional profiles from JSON file.
    if let Ok(path) = std::env::var("BENCH_PROFILES") {
        let content =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
        let file_profiles: Vec<BenchProfile> = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("Failed to parse {path}: {e}"));
        profiles.extend(file_profiles);
    }

    // Filter by enabled tiers.
    let longhaul = std::env::var("BENCH_LONGHAUL").is_ok();
    let stress = std::env::var("BENCH_STRESS").is_ok();
    profiles.retain(|p| match p.tier {
        Tier::Validation => true,
        Tier::Longhaul => longhaul,
        Tier::Stress => stress,
    });

    profiles
}

// ---------------------------------------------------------------------------
// BenchState — shared state for handler + verification
// ---------------------------------------------------------------------------

struct BenchState {
    consumed: Arc<DashMap<i64, Vec<u64>>>,
    db_seqs: Arc<DashMap<i64, Vec<i64>>>,
    counter: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    expected_total: usize,
}

impl BenchState {
    fn new(expected_total: usize) -> Self {
        Self {
            consumed: Arc::new(DashMap::new()),
            db_seqs: Arc::new(DashMap::new()),
            counter: Arc::new(AtomicUsize::new(0)),
            notify: Arc::new(Notify::new()),
            expected_total,
        }
    }
}

// ---------------------------------------------------------------------------
// BenchHandler — captures consumed messages for verification
// ---------------------------------------------------------------------------

struct BenchHandler {
    consumed: Arc<DashMap<i64, Vec<u64>>>,
    db_seqs: Arc<DashMap<i64, Vec<i64>>>,
    counter: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    expected_total: usize,
}

#[async_trait::async_trait]
impl Handler for BenchHandler {
    async fn handle(&self, msgs: &[OutboxMessage], _cancel: CancellationToken) -> HandlerResult {
        for msg in msgs {
            let payload = std::str::from_utf8(&msg.payload).unwrap();
            let (_, seq_str) = payload.split_once(':').unwrap();
            let seq: u64 = seq_str.parse().unwrap();

            self.consumed.entry(msg.partition_id).or_default().push(seq);
            self.db_seqs
                .entry(msg.partition_id)
                .or_default()
                .push(msg.seq);

            let prev = self.counter.fetch_add(1, Ordering::Relaxed);
            if prev + 1 >= self.expected_total {
                self.notify.notify_one();
            }
        }
        HandlerResult::Success
    }
}

fn make_handler(state: &BenchState) -> BenchHandler {
    BenchHandler {
        consumed: Arc::clone(&state.consumed),
        db_seqs: Arc::clone(&state.db_seqs),
        counter: Arc::clone(&state.counter),
        notify: Arc::clone(&state.notify),
        expected_total: state.expected_total,
    }
}

// ---------------------------------------------------------------------------
// Wait + verification
// ---------------------------------------------------------------------------

async fn wait_for_completion(state: &BenchState, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if state.counter.load(Ordering::Acquire) >= state.expected_total {
            return;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            let got = state.counter.load(Ordering::Acquire);
            panic!(
                "Timeout: expected {} messages consumed, got {got} ({} missing)",
                state.expected_total,
                state.expected_total - got
            );
        }
        let _ = tokio::time::timeout(remaining, state.notify.notified()).await;
    }
}

/// Verify per-partition ordering and completeness.
///
/// For uniform distributions, checks exact per-partition counts.
/// For realistic distributions, checks total count and per-partition ordering only.
fn verify_results(state: &BenchState, profile: &BenchProfile) {
    let msg_count = profile.aligned_message_count();
    let total_parts = profile.total_partitions();

    // DB-seq ordering: must be strictly monotonically increasing per partition
    for entry in state.db_seqs.iter() {
        let partition = *entry.key();
        let db_seqs = entry.value();
        for i in 1..db_seqs.len() {
            assert!(
                db_seqs[i] > db_seqs[i - 1],
                "partition {partition}: DB seq ordering violation at position {i}: \
                 seq[{}]={} >= seq[{i}]={}",
                i - 1,
                db_seqs[i - 1],
                db_seqs[i]
            );
        }
    }

    match profile.load_distribution {
        LoadDistribution::Uniform => {
            let msgs_per_partition = msg_count / total_parts;

            assert_eq!(
                state.consumed.len(),
                total_parts,
                "expected {total_parts} partitions, got {}",
                state.consumed.len()
            );

            for entry in state.consumed.iter() {
                let partition = *entry.key();
                let seqs = entry.value();

                assert_eq!(
                    seqs.len(),
                    msgs_per_partition,
                    "partition {partition}: expected {msgs_per_partition} messages, got {}",
                    seqs.len()
                );

                let actual: HashSet<u64> = seqs.iter().copied().collect();
                let expected: HashSet<u64> = (0..msgs_per_partition as u64).collect();
                assert_eq!(
                    actual,
                    expected,
                    "partition {partition}: payload sequence mismatch — \
                     missing: {:?}, extra: {:?}",
                    expected.difference(&actual).collect::<Vec<_>>(),
                    actual.difference(&expected).collect::<Vec<_>>()
                );
            }
        }
        LoadDistribution::Realistic { .. } => {
            // With skewed distribution, just verify total count and per-partition
            // sequence completeness (no gaps within each partition).
            let total_consumed: usize = state.consumed.iter().map(|e| e.value().len()).sum();
            assert_eq!(
                total_consumed, msg_count,
                "expected {msg_count} total messages, got {total_consumed}"
            );

            for entry in state.consumed.iter() {
                let partition = *entry.key();
                let seqs = entry.value();
                let actual: HashSet<u64> = seqs.iter().copied().collect();
                let expected: HashSet<u64> = (0..seqs.len() as u64).collect();
                assert_eq!(
                    actual,
                    expected,
                    "partition {partition}: payload sequence gap — \
                     missing: {:?}, extra: {:?}",
                    expected.difference(&actual).collect::<Vec<_>>(),
                    actual.difference(&expected).collect::<Vec<_>>()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Message addressing — maps global index to (queue, local_partition, seq)
// ---------------------------------------------------------------------------

/// Per-partition atomic counters for non-uniform (Realistic) distributions.
///
/// Uniform distribution guarantees a bijection between global index and
/// (partition, seq) via arithmetic, so counters are unnecessary. Realistic
/// distribution skews messages to hot queues, breaking that bijection —
/// multiple global indices map to the same partition, requiring explicit
/// counters to produce unique, gap-free per-partition sequence numbers.
struct PartitionCounters {
    counters: Vec<AtomicU64>,
}

impl PartitionCounters {
    fn new(total_partitions: usize) -> Self {
        Self {
            counters: (0..total_partitions).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    fn next_seq(&self, global_partition: usize) -> u64 {
        self.counters[global_partition].fetch_add(1, Ordering::Relaxed)
    }
}

/// Resolves which queue and local partition a message belongs to.
///
/// For `Uniform` distribution, the seq is computed arithmetically from the
/// global index (no counters needed). For `Realistic`, the caller must
/// supply `PartitionCounters` to generate unique per-partition sequences.
fn message_addr(
    i: usize,
    queue_prefix: &str,
    profile: &BenchProfile,
    counters: Option<&PartitionCounters>,
) -> (String, u32, u64) {
    let nq = profile.num_queues;
    let pp = profile.partitions_per_queue as usize;
    let total_parts = nq * pp;

    match profile.load_distribution {
        LoadDistribution::Uniform => {
            // Round-robin across all global partitions: i % total_parts gives a
            // global partition index. Divide by pp to get queue, mod pp for local.
            let global_part = i % total_parts;
            let queue_idx = global_part / pp;
            let local_part = (global_part % pp) as u32;
            let seq = (i / total_parts) as u64;

            let queue_name = if nq == 1 {
                queue_prefix.to_owned()
            } else {
                format!("{queue_prefix}_q{queue_idx}")
            };
            (queue_name, local_part, seq)
        }
        LoadDistribution::Realistic { hot_queues } => {
            // 90% of messages to hot queues, 10% to cold.
            let hot = hot_queues.min(nq);
            let cold = nq - hot;
            let queue_idx = if (i % 10) < 9 && hot > 0 {
                i / 10 % hot
            } else if cold > 0 {
                hot + (i / 10 % cold)
            } else {
                i % nq
            };
            let local_part = (i / nq % pp) as u32;
            let global_part = queue_idx * pp + local_part as usize;
            let seq = counters
                .expect("Realistic distribution requires PartitionCounters")
                .next_seq(global_part);

            let queue_name = if nq == 1 {
                queue_prefix.to_owned()
            } else {
                format!("{queue_prefix}_q{queue_idx}")
            };
            (queue_name, local_part, seq)
        }
    }
}

// ---------------------------------------------------------------------------
// Unified producer
// ---------------------------------------------------------------------------

async fn produce_range(
    outbox: &Arc<Outbox>,
    db: &Db,
    queue_prefix: &str,
    profile: &BenchProfile,
    counters: Option<&PartitionCounters>,
    start: usize,
    end: usize,
) {
    let batch_size = profile.batch_size();

    if batch_size <= 1 {
        // Single-message path
        for i in start..end {
            let (queue, partition, seq) = message_addr(i, queue_prefix, profile, counters);
            let payload = format!("{partition}:{seq}").into_bytes();
            let o = Arc::clone(outbox);
            let (_, result) = o
                .transaction(db.clone(), |tx| {
                    let o2 = Arc::clone(&o);
                    Box::pin(async move {
                        o2.enqueue(tx, &queue, partition, payload, "bench/seq")
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                        Ok(())
                    })
                })
                .await;
            result.unwrap();
        }
    } else {
        // Batched path — bucket messages by queue, then enqueue_batch per queue.
        for chunk_start in (start..end).step_by(batch_size) {
            let chunk_end = (chunk_start + batch_size).min(end);

            // Bucket by queue index.
            let mut buckets: Vec<(String, Vec<EnqueueMessage<'_>>)> = Vec::new();
            for i in chunk_start..chunk_end {
                let (queue, local_part, seq) = message_addr(i, queue_prefix, profile, counters);
                let msg = EnqueueMessage {
                    partition: local_part,
                    payload: format!("{local_part}:{seq}").into_bytes(),
                    payload_type: "bench/seq",
                };
                if let Some(bucket) = buckets.iter_mut().find(|(q, _)| q == &queue) {
                    bucket.1.push(msg);
                } else {
                    buckets.push((queue, vec![msg]));
                }
            }

            for (queue, msgs) in buckets {
                let o = Arc::clone(outbox);
                let (_, result) = o
                    .transaction(db.clone(), |tx| {
                        let o2 = Arc::clone(&o);
                        Box::pin(async move {
                            o2.enqueue_batch(tx, &queue, &msgs)
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))?;
                            Ok(())
                        })
                    })
                    .await;
                result.unwrap();
            }
        }
    }
}

/// Produce messages, distributing producers round-robin across outbox instances.
/// Each producer task picks one outbox instance to enqueue through, ensuring
/// all instances receive enqueue traffic (and thus trigger their prioritizers).
async fn produce(outboxes: &[Arc<Outbox>], db: &Db, queue_prefix: &str, profile: &BenchProfile) {
    let total = profile.aligned_message_count();
    let np = profile.num_producers.max(1);

    // Realistic distributions need shared atomic counters for unique per-partition seqs.
    let counters: Option<Arc<PartitionCounters>> = if matches!(
        profile.load_distribution,
        LoadDistribution::Realistic { .. }
    ) {
        Some(Arc::new(PartitionCounters::new(profile.total_partitions())))
    } else {
        None
    };

    let per_producer = total / np;
    let mut handles = Vec::with_capacity(np);
    for pid in 0..np {
        let start = pid * per_producer;
        let end = if pid == np - 1 {
            total
        } else {
            start + per_producer
        };
        // Round-robin: producer pid uses outbox instance pid % num_instances
        let outbox = Arc::clone(&outboxes[pid % outboxes.len()]);
        let db = db.clone();
        let qp = queue_prefix.to_owned();
        let profile = profile.clone();
        let counters = counters.clone();
        handles.push(tokio::spawn(async move {
            produce_range(&outbox, &db, &qp, &profile, counters.as_deref(), start, end).await;
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Pipeline setup
// ---------------------------------------------------------------------------

fn iter_queue_name(iter: u64) -> String {
    format!("bench_{iter}")
}

/// Build queue names for a given prefix and profile.
fn queue_names(queue_prefix: &str, profile: &BenchProfile) -> Vec<String> {
    (0..profile.num_queues)
        .map(|qi| {
            if profile.num_queues == 1 {
                queue_prefix.to_owned()
            } else {
                format!("{queue_prefix}_q{qi}")
            }
        })
        .collect()
}

/// Start one outbox instance registering all queues with shared BenchState.
async fn start_instance(
    db: &Db,
    profile: &BenchProfile,
    queue_prefix: &str,
    state: &BenchState,
) -> OutboxHandle {
    let mut builder = Outbox::builder(db.clone())
        .profile(OutboxProfile::high_throughput())
        .processors(profile.num_processors)
        .maintenance(profile.maintenance.guaranteed, profile.maintenance.shared)
        .stats_interval(Duration::from_secs(10));

    for name in queue_names(queue_prefix, profile) {
        builder = builder
            .queue(&name, Partitions::of(profile.partitions_per_queue))
            .batch_decoupled(make_handler(state));
    }

    builder.start().await.unwrap()
}

/// Set up N outbox instances sharing the same database and queues.
///
/// All instances register the same queue names and compete for partition
/// leases via `locked_by`/`locked_until` in the processor table.
/// Returns the shared DB pool, all handles, and the shared verification state.
/// Producers distribute enqueue calls round-robin across all instances.
async fn setup_pipeline(
    db_url: &str,
    profile: &BenchProfile,
    queue_prefix: &str,
) -> (Db, Vec<OutboxHandle>, Arc<BenchState>) {
    let is_sqlite = db_url.starts_with("sqlite");
    let max_conns = profile.effective_pool_size(is_sqlite);
    let msg_count = profile.aligned_message_count();

    let db = connect_db(
        db_url,
        ConnectOpts {
            max_conns: Some(max_conns),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let state = Arc::new(BenchState::new(msg_count));

    let mut handles = Vec::with_capacity(profile.num_instances);
    for _ in 0..profile.num_instances {
        handles.push(start_instance(&db, profile, queue_prefix, &state).await);
    }

    (db, handles, state)
}

/// Delete all data from outbox tables after each iteration.
async fn cleanup_outbox_tables(db_url: &str) {
    use sea_orm::{ConnectionTrait, Database, Statement};

    const TABLES: &[&str] = &[
        "modkit_outbox_dead_letters",
        "modkit_outbox_outgoing",
        "modkit_outbox_incoming",
        "modkit_outbox_vacuum_counter",
        "modkit_outbox_processor",
        "modkit_outbox_body",
        "modkit_outbox_partitions",
    ];

    let db = Database::connect(db_url).await.unwrap();
    let backend = db.get_database_backend();
    for table in TABLES {
        let sql = match backend {
            sea_orm::DbBackend::Postgres => format!("TRUNCATE TABLE {table} CASCADE"),
            sea_orm::DbBackend::Sqlite | sea_orm::DbBackend::MySql => {
                format!("DELETE FROM {table}")
            }
        };
        db.execute(Statement::from_string(backend, sql))
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Generic benchmark runner
// ---------------------------------------------------------------------------

fn run_profile(
    c: &mut Criterion,
    profile: &BenchProfile,
    db_url: &str,
    engine: &str,
    rt: &Runtime,
) {
    let group_name = match profile.tier {
        Tier::Validation => engine.to_owned(),
        Tier::Longhaul => format!("{engine}_longhaul"),
        Tier::Stress => format!("{engine}_stress"),
    };
    let msg_count = profile.aligned_message_count();
    let timeout = Duration::from_secs(profile.criterion.timeout_secs);

    let mut group = c.benchmark_group(&group_name);
    group.throughput(Throughput::Elements(msg_count as u64));
    group.sample_size(profile.criterion.samples);
    group.measurement_time(Duration::from_secs(profile.criterion.measurement_secs));
    group.warm_up_time(Duration::from_secs(profile.criterion.warmup_secs));

    let profile = profile.clone();
    let db_url = db_url.to_owned();

    group.bench_function(&profile.name, |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let iter_id = GLOBAL_ITER_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let queue_prefix = iter_queue_name(iter_id);

                    let (db, handles, state) =
                        setup_pipeline(&db_url, &profile, &queue_prefix).await;

                    // Collect outbox refs from all instances for producer distribution.
                    let outboxes: Vec<Arc<Outbox>> =
                        handles.iter().map(|h| Arc::clone(h.outbox())).collect();

                    let start = Instant::now();
                    produce(&outboxes, &db, &queue_prefix, &profile).await;
                    wait_for_completion(&state, timeout).await;
                    total += start.elapsed();

                    verify_results(&state, &profile);
                    for h in handles {
                        h.stop().await;
                    }
                    drop(db);
                    cleanup_outbox_tables(&db_url).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                total
            })
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Container helpers
// ---------------------------------------------------------------------------

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("Timeout waiting for {host}:{port}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// --- Postgres ---

#[cfg(feature = "pg")]
mod pg_container {
    use super::{
        ConnectOpts, Duration, OnceLock, Runtime, connect_db, outbox_migrations,
        run_migrations_for_testing, wait_for_tcp,
    };
    use testcontainers::{ContainerAsync, ContainerRequest, ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;

    pub struct PgContainer {
        pub url: String,
        _container: ContainerAsync<Postgres>,
    }

    static PG: OnceLock<PgContainer> = OnceLock::new();

    pub fn get_pg(rt: &Runtime) -> &'static PgContainer {
        PG.get_or_init(|| {
            rt.block_on(async {
                let container = ContainerRequest::from(Postgres::default())
                    .with_env_var("POSTGRES_PASSWORD", "pass")
                    .with_env_var("POSTGRES_USER", "user")
                    .with_env_var("POSTGRES_DB", "bench")
                    .start()
                    .await
                    .unwrap();
                let port = container.get_host_port_ipv4(5432).await.unwrap();
                wait_for_tcp("127.0.0.1", port, Duration::from_secs(30)).await;
                let url = format!("postgres://user:pass@127.0.0.1:{port}/bench");

                let db = connect_db(&url, ConnectOpts::default()).await.unwrap();
                run_migrations_for_testing(&db, outbox_migrations())
                    .await
                    .unwrap();

                PgContainer {
                    url,
                    _container: container,
                }
            })
        })
    }
}

// --- MySQL ---

#[cfg(feature = "mysql")]
mod mysql_container {
    use super::{
        ConnectOpts, Duration, OnceLock, Runtime, connect_db, outbox_migrations,
        run_migrations_for_testing, wait_for_tcp,
    };
    use testcontainers::{ContainerAsync, ContainerRequest, ImageExt, runners::AsyncRunner};
    use testcontainers_modules::mysql::Mysql;

    pub struct MysqlContainer {
        pub url: String,
        _container: ContainerAsync<Mysql>,
    }

    static MYSQL: OnceLock<MysqlContainer> = OnceLock::new();

    pub fn get_mysql(rt: &Runtime) -> &'static MysqlContainer {
        MYSQL.get_or_init(|| {
            rt.block_on(async {
                // MySQL tuning to reduce InnoDB overhead in benchmarks:
                // - READ-COMMITTED: eliminates gap-lock deadlocks on concurrent
                //   claim/delete from adjacent partitions
                // - skip-log-bin: disables binary log fsync — the #1 bottleneck
                //   (COMMIT is 105x slower with binlog enabled vs PG)
                // - innodb-flush-log-at-trx-commit=2: flush redo log once/sec
                //   instead of per-commit (safe for benchmarks, not production)
                let container = ContainerRequest::from(Mysql::default())
                    .with_env_var("MYSQL_ROOT_PASSWORD", "root")
                    .with_env_var("MYSQL_USER", "user")
                    .with_env_var("MYSQL_PASSWORD", "pass")
                    .with_env_var("MYSQL_DATABASE", "bench")
                    .with_cmd([
                        "--transaction-isolation=READ-COMMITTED",
                        "--skip-log-bin",
                        "--innodb-flush-log-at-trx-commit=2",
                    ])
                    .start()
                    .await
                    .unwrap();
                let port = container.get_host_port_ipv4(3306).await.unwrap();
                wait_for_tcp("127.0.0.1", port, Duration::from_secs(60)).await;
                let url = format!("mysql://user:pass@127.0.0.1:{port}/bench");

                let db = connect_db(&url, ConnectOpts::default()).await.unwrap();
                run_migrations_for_testing(&db, outbox_migrations())
                    .await
                    .unwrap();

                MysqlContainer {
                    url,
                    _container: container,
                }
            })
        })
    }
}

// --- MariaDB ---

#[cfg(feature = "mysql")]
mod mariadb_container {
    use super::{
        ConnectOpts, Duration, OnceLock, Runtime, connect_db, outbox_migrations,
        run_migrations_for_testing, wait_for_tcp,
    };
    use testcontainers::core::{ContainerPort, WaitFor};
    use testcontainers::{
        ContainerAsync, ContainerRequest, GenericImage, ImageExt, runners::AsyncRunner,
    };

    pub struct MariaContainer {
        pub url: String,
        _container: ContainerAsync<GenericImage>,
    }

    static MARIADB: OnceLock<MariaContainer> = OnceLock::new();

    fn mariadb_image() -> GenericImage {
        // MariaDB prints "ready for connections" twice: once during the
        // temporary bootstrap server, once for the real server. We match
        // the version line that only appears in the final startup message.
        GenericImage::new("mariadb", "lts")
            .with_wait_for(WaitFor::message_on_stderr(
                "mariadb.org binary distribution",
            ))
            .with_exposed_port(ContainerPort::Tcp(3306))
    }

    pub fn get_mariadb(rt: &Runtime) -> &'static MariaContainer {
        MARIADB.get_or_init(|| {
            rt.block_on(async {
                let container = ContainerRequest::from(mariadb_image())
                    .with_env_var("MYSQL_ROOT_PASSWORD", "root")
                    .with_env_var("MYSQL_USER", "user")
                    .with_env_var("MYSQL_PASSWORD", "pass")
                    .with_env_var("MYSQL_DATABASE", "bench")
                    .with_cmd([
                        "--transaction-isolation=READ-COMMITTED",
                        "--skip-log-bin",
                        "--innodb-flush-log-at-trx-commit=2",
                    ])
                    .start()
                    .await
                    .unwrap();
                let port = container.get_host_port_ipv4(3306).await.unwrap();
                wait_for_tcp("127.0.0.1", port, Duration::from_secs(60)).await;
                let url = format!("mysql://user:pass@127.0.0.1:{port}/bench");

                // MariaDB may accept TCP before auth is ready. Retry connect.
                let db = {
                    let mut attempts = 0;
                    loop {
                        match connect_db(&url, ConnectOpts::default()).await {
                            Ok(db) => break db,
                            Err(e) => {
                                attempts += 1;
                                if attempts >= 10 {
                                    panic!("MariaDB connect failed after {attempts} attempts: {e}");
                                }
                                tokio::time::sleep(Duration::from_secs(2)).await;
                            }
                        }
                    }
                };
                run_migrations_for_testing(&db, outbox_migrations())
                    .await
                    .unwrap();

                MariaContainer {
                    url,
                    _container: container,
                }
            })
        })
    }
}

// --- SQLite ---

#[cfg(feature = "sqlite")]
mod sqlite_setup {
    use super::{
        ConnectOpts, Db, OnceLock, Runtime, connect_db, outbox_migrations,
        run_migrations_for_testing,
    };

    pub struct SqliteDb {
        pub url: String,
        _db: Db,
    }

    static SQLITE: OnceLock<SqliteDb> = OnceLock::new();

    pub fn get_sqlite(rt: &Runtime) -> &'static SqliteDb {
        SQLITE.get_or_init(|| {
            rt.block_on(async {
                let url = "sqlite:file:outbox_bench?mode=memory&cache=shared".to_owned();

                let db = connect_db(
                    &url,
                    ConnectOpts {
                        max_conns: Some(1),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
                run_migrations_for_testing(&db, outbox_migrations())
                    .await
                    .unwrap();

                SqliteDb { url, _db: db }
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Tracing setup — logs to /tmp/outbox_bench.log
// ---------------------------------------------------------------------------

fn setup_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let log_file = std::fs::File::create("/tmp/outbox_bench.log")
        .expect("failed to create /tmp/outbox_bench.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(log_file);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_level(true),
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    eprintln!("Benchmark logs → /tmp/outbox_bench.log");
    guard
}

// ---------------------------------------------------------------------------
// Main — tracing + profile-driven benchmarks
// ---------------------------------------------------------------------------

#[cfg(not(any(feature = "pg", feature = "mysql", feature = "sqlite")))]
compile_error!(
    "outbox_throughput benchmark requires at least one database feature: pg, mysql, or sqlite"
);

fn main() {
    let guard = setup_tracing();
    let profiles = load_profiles();

    let mut criterion = Criterion::default().configure_from_args();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(16)
        .enable_all()
        .build()
        .unwrap();

    #[cfg(feature = "pg")]
    {
        let pg = pg_container::get_pg(&rt);
        for profile in &profiles {
            run_profile(&mut criterion, profile, &pg.url, "postgres", &rt);
        }
    }

    #[cfg(feature = "mysql")]
    {
        let mysql = mysql_container::get_mysql(&rt);
        for profile in &profiles {
            run_profile(&mut criterion, profile, &mysql.url, "mysql", &rt);
        }
        let maria = mariadb_container::get_mariadb(&rt);
        for profile in &profiles {
            run_profile(&mut criterion, profile, &maria.url, "mariadb", &rt);
        }
    }

    #[cfg(feature = "sqlite")]
    {
        let sq = sqlite_setup::get_sqlite(&rt);
        for profile in profiles.iter().filter(|p| p.num_producers <= 1) {
            run_profile(&mut criterion, profile, &sq.url, "sqlite", &rt);
        }
    }

    criterion.final_summary();
    drop(guard);
}

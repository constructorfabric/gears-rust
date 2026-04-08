#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Worker infrastructure overhead benchmarks.
//!
//! Measures the cost of the worker loop itself — scheduling, directive
//! handling, notifier wakeup, semaphore acquire — with no-op actions.
//! This isolates infrastructure overhead from action (business logic) cost.
//!
//! Run: `cargo bench -p cf-modkit-db --features preview-outbox --bench worker_overhead`

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::runtime::Runtime;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use modkit_db::outbox::taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, Directive, WorkerAction,
    WorkerBuilder,
};

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// No-op action that returns a fixed directive for N calls, then cancels.
struct NoOpAction {
    directive: Directive,
    remaining: u64,
    cancel: CancellationToken,
}

impl WorkerAction for NoOpAction {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.remaining -= 1;
        if self.remaining == 0 {
            self.cancel.cancel();
            Ok(Directive::sleep(Duration::from_secs(1)))
        } else {
            Ok(self.directive)
        }
    }
}

/// Action that alternates between Proceed and Idle.
struct AlternatingAction {
    remaining: u64,
    flip: bool,
    cancel: CancellationToken,
}

impl WorkerAction for AlternatingAction {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.remaining -= 1;
        if self.remaining == 0 {
            self.cancel.cancel();
            return Ok(Directive::sleep(Duration::from_secs(1)));
        }
        self.flip = !self.flip;
        if self.flip {
            Ok(Directive::proceed())
        } else {
            Ok(Directive::idle())
        }
    }
}

// ---------------------------------------------------------------------------
// Benchmark: Continue loop throughput (pure scheduling overhead)
// ---------------------------------------------------------------------------

fn bench_continue_loop(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("worker/continue_loop");

    for &n in &[1_000u64, 10_000, 100_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cancel = CancellationToken::new();
                        let action = NoOpAction {
                            directive: Directive::proceed(),
                            remaining: n,
                            cancel: cancel.clone(),
                        };
                        // Poker breaks the initial Idle.
                        let notify = Arc::new(Notify::new());
                        notify.notify_one();
                        let worker = WorkerBuilder::new("bench", cancel.clone())
                            .notifier(notify)
                            .build(action);

                        let start = Instant::now();
                        worker.run().await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Notify-driven wakeup (Idle → notify → execute)
// ---------------------------------------------------------------------------

fn bench_notify_wakeup(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("worker/notify_wakeup");

    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cancel = CancellationToken::new();
                        let notify = Arc::new(Notify::new());

                        let action = NoOpAction {
                            directive: Directive::idle(),
                            remaining: n,
                            cancel: cancel.clone(),
                        };
                        let worker = WorkerBuilder::new("bench", cancel.clone())
                            .notifier(notify.clone())
                            .build(action);

                        // Pump notifications continuously.
                        let notify_c = notify.clone();
                        let cancel_c = cancel.clone();
                        tokio::spawn(async move {
                            notify_c.notify_one();
                            loop {
                                tokio::task::yield_now().await;
                                if cancel_c.is_cancelled() {
                                    break;
                                }
                                notify_c.notify_one();
                            }
                        });

                        let start = Instant::now();
                        worker.run().await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Semaphore-gated loop (acquire + release per iteration)
// ---------------------------------------------------------------------------

fn bench_semaphore_gated(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("worker/semaphore_gated");

    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cancel = CancellationToken::new();
                        let sem = Arc::new(Semaphore::new(1));

                        let bulkhead = Bulkhead::new(
                            "bench",
                            BulkheadConfig {
                                semaphore: ConcurrencyLimit::Fixed(sem),
                                backoff: BackoffConfig {
                                    initial: Duration::from_millis(10),
                                    max: Duration::from_secs(60),
                                    multiplier: 2.0,
                                    jitter: 0.0,
                                },
                            },
                        );

                        let action = NoOpAction {
                            directive: Directive::proceed(),
                            remaining: n,
                            cancel: cancel.clone(),
                        };
                        let notify = Arc::new(Notify::new());
                        notify.notify_one();
                        let worker = WorkerBuilder::new("bench", cancel.clone())
                            .bulkhead(bulkhead)
                            .notifier(notify)
                            .build(action);

                        let start = Instant::now();
                        worker.run().await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Alternating Continue/Idle (realistic mixed workload)
// ---------------------------------------------------------------------------

fn bench_alternating(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("worker/alternating");

    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cancel = CancellationToken::new();
                        let notify = Arc::new(Notify::new());

                        let action = AlternatingAction {
                            remaining: n,
                            flip: false,
                            cancel: cancel.clone(),
                        };
                        let worker = WorkerBuilder::new("bench", cancel.clone())
                            .notifier(notify.clone())
                            .build(action);

                        let notify_c = notify.clone();
                        let cancel_c = cancel.clone();
                        tokio::spawn(async move {
                            notify_c.notify_one();
                            loop {
                                tokio::task::yield_now().await;
                                if cancel_c.is_cancelled() {
                                    break;
                                }
                                notify_c.notify_one();
                            }
                        });

                        let start = Instant::now();
                        worker.run().await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Multi-notifier wakeup (3 sources, select_all overhead)
// ---------------------------------------------------------------------------

fn bench_multi_notifier(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("worker/multi_notifier");

    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cancel = CancellationToken::new();
                        let n1 = Arc::new(Notify::new());
                        let n2 = Arc::new(Notify::new());
                        let n3 = Arc::new(Notify::new());

                        let action = NoOpAction {
                            directive: Directive::idle(),
                            remaining: n,
                            cancel: cancel.clone(),
                        };
                        let worker = WorkerBuilder::new("bench", cancel.clone())
                            .notifier(n1.clone())
                            .notifier(n2.clone())
                            .notifier(n3.clone())
                            .build(action);

                        // Rotate notifications across sources.
                        let cancel_c = cancel.clone();
                        tokio::spawn(async move {
                            let sources = [&n1, &n2, &n3];
                            let mut i = 0usize;
                            sources[0].notify_one();
                            loop {
                                tokio::task::yield_now().await;
                                if cancel_c.is_cancelled() {
                                    break;
                                }
                                i = (i + 1) % 3;
                                sources[i].notify_one();
                            }
                        });

                        let start = Instant::now();
                        worker.run().await;
                        total += start.elapsed();
                    }
                    total
                })
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_continue_loop,
    bench_notify_wakeup,
    bench_semaphore_gated,
    bench_alternating,
    bench_multi_notifier,
);
criterion_main!(benches);

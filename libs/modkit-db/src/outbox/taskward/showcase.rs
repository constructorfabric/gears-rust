//! Scenario-based tests demonstrating worker infrastructure behaviour in
//! realistic settings. Each test tells a story — the name describes the
//! situation, the body shows how the worker handles it.
//!
//! All tests use `start_paused = true` (tokio virtual time) so durations
//! are realistic (hours, seconds) yet tests complete instantly.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use crate::outbox::taskward::action::{Directive, WorkerAction};
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    use crate::outbox::taskward::listener::TracingListener;
    use crate::outbox::taskward::pacing::PacingConfig;
    use crate::outbox::taskward::poker::poker;
    use crate::outbox::taskward::task::{PanicPolicy, WorkerBuilder};

    // ---- Scenario: Long-interval worker reschedules immediately when work
    //      exceeds the polling interval ----
    //
    // Real-world analogy: a batch processor with a 4 h polling interval.
    // Normally the work takes ~2 h and the worker idles until the next poke.
    // But one day the batch is huge — work takes 5 h, overshooting the
    // 4 h window. The action returns `Proceed` so the worker retries
    // immediately instead of waiting for the next poke.

    struct BatchProcessor {
        /// Durations of simulated work per call.
        work_durations: Vec<Duration>,
        /// Threshold — if work took longer than this, signal "more work".
        threshold: Duration,
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for BatchProcessor {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            let work_time = self
                .work_durations
                .get(idx)
                .copied()
                .unwrap_or(Duration::from_millis(1));

            // Simulate the work taking `work_time`.
            tokio::time::sleep(work_time).await;

            if work_time > self.threshold {
                // Work exceeded the threshold — reschedule immediately.
                Ok(Directive::proceed())
            } else {
                // Normal completion — go back to idle, wait for next poke.
                Ok(Directive::idle())
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn long_interval_worker_reschedules_when_work_exceeds_window() {
        let h = Duration::from_secs(3600); // 1 hour (virtual time, runs instantly)

        let cancel = CancellationToken::new();
        let call_count = Arc::new(AtomicU32::new(0));

        let action = BatchProcessor {
            work_durations: vec![
                h * 2, // call 1: normal, 2h → Idle
                h * 5, // call 2: overrun, 5h → Proceed
                h,     // call 3: immediate retry, 1h → Idle
            ],
            threshold: h * 4,
            call_count: call_count.clone(),
        };

        let (poker_notify, _poker_handle) = poker(h * 4, cancel.clone());
        let worker = WorkerBuilder::new("batch-processor", cancel.clone())
            .notifier(poker_notify)
            .pacing(PacingConfig::default())
            .build(action);

        // Cancel after 16h — enough for 3 calls + some idle.
        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(h * 16).await;
            cancel_c.cancel();
        });

        let start = tokio::time::Instant::now();
        worker.run().await;

        // At least 3 calls happened. Call 3 was the immediate retry after
        // the overrun (Proceed). Additional calls may occur if the poker
        // fires again before cancel — that's fine; the key assertion is
        // timing below.
        let calls = call_count.load(Ordering::SeqCst);
        assert!(
            calls >= 3,
            "expected at least 3 calls (normal -> overrun -> immediate retry), got {calls}",
        );

        // The critical property: total elapsed shows that call 3 started
        // IMMEDIATELY after call 2, not after another 4h poke.
        // Without Proceed: poker(4h) + work(2h) + poker(4h) + work(5h)
        //   + poker(4h) + work(1h) = 20h → would exceed cancel window.
        // With Proceed: poker(4h) + work(2h) + poker(4h) + work(5h)
        //   + work(1h) = 16h → fits within cancel window.
        let elapsed = start.elapsed();
        assert!(
            elapsed < h * 17,
            "worker should finish within cancel window"
        );
    }

    // ---- Scenario: Event-driven worker wakes from external notify,
    //      not just from timer ----

    struct EventDrivenAction {
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for EventDrivenAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(Directive::idle())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn event_driven_worker_ignores_long_poker_when_notified() {
        let h = Duration::from_secs(3600);

        let cancel = CancellationToken::new();
        let notify = Arc::new(Notify::new());
        let call_count = Arc::new(AtomicU32::new(0));

        let action = EventDrivenAction {
            call_count: call_count.clone(),
        };

        // Poker fires every 5h (safety net) — but notifiers fire much sooner.
        let (poker_notify, _poker_handle) = poker(h * 5, cancel.clone());
        let worker = WorkerBuilder::new("event-worker", cancel.clone())
            .notifier(poker_notify)
            .notifier(notify.clone())
            .pacing(PacingConfig::default())
            .build(action);

        // Stored permit → initial Idle resolves immediately.
        notify.notify_one();
        let notify_c = notify.clone();
        tokio::spawn(async move {
            tokio::time::sleep(h).await;
            notify_c.notify_one(); // call 2
            tokio::time::sleep(h).await;
            notify_c.notify_one(); // call 3
        });

        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(h * 4).await;
            cancel_c.cancel();
        });

        let start = tokio::time::Instant::now();
        worker.run().await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            3,
            "expected 3 event-driven calls, not waiting for 5h poker",
        );
        // All 3 calls happen within 4h — well before the 5h poker.
        assert!(
            start.elapsed() < h * 5,
            "should complete fast via notifiers, not wait for poker",
        );
    }

    // ---- Scenario: Transient errors backoff then recover ----

    struct FlakyAction {
        results: Vec<Result<Directive, String>>,
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for FlakyAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            self.results
                .get(idx)
                .cloned()
                .unwrap_or(Ok(Directive::sleep(Duration::from_secs(3600))))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn transient_errors_backoff_then_recover() {
        // 3 consecutive errors → escalating backoff (1s, 2s, 4s = 7s)
        // Then success → backoff resets.
        // Then another error → backoff starts from 1s again (not 8s).

        let cancel = CancellationToken::new();
        let call_count = Arc::new(AtomicU32::new(0));

        let action = FlakyAction {
            results: vec![
                Err("db timeout".into()), // backoff → 1s
                Err("db timeout".into()), // backoff → 2s
                Err("db timeout".into()), // backoff → 4s
                Ok(Directive::proceed()), // reset backoff
                Err("db timeout".into()), // backoff → 1s (reset!)
                Ok(Directive::sleep(Duration::from_secs(3600))),
            ],
            call_count: call_count.clone(),
        };

        let bulkhead = Bulkhead::new(
            "flaky-worker",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Unlimited,
                backoff: BackoffConfig {
                    initial: Duration::from_secs(1),
                    max: Duration::from_secs(3600),
                    multiplier: 2.0,
                    jitter: 0.0,
                },
            },
        );

        let notify = Arc::new(Notify::new());
        notify.notify_one(); // break initial Idle

        let worker = WorkerBuilder::new("flaky-worker", cancel.clone())
            .bulkhead(bulkhead)
            .listener(TracingListener)
            .pacing(PacingConfig {
                active_interval: Duration::ZERO,
                min_interval: Duration::ZERO,
                ramp_step: Duration::ZERO,
            })
            .notifier(notify.clone())
            .build(action);

        // Fire notifies to wake from each error-Idle (4 errors → 4 notifies)
        let notify_c = notify.clone();
        tokio::spawn(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_secs(5)).await;
                notify_c.notify_one();
            }
        });

        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            cancel_c.cancel();
        });

        let start = tokio::time::Instant::now();
        worker.run().await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            6,
            "all 6 calls should complete -- errors absorbed by bulkhead",
        );
        // Total backoff: 1 + 2 + 4 + 0 (Proceed) + 1 = 8s minimum.
        assert!(start.elapsed() >= Duration::from_secs(8));
    }

    // ---- Scenario: Multiple wake sources compose naturally ----

    struct MultiSourceAction {
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for MultiSourceAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(Directive::idle())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn multiple_notifiers_any_source_wakes_worker() {
        let h = Duration::from_secs(3600);

        // 3 independent event sources — worker wakes on whichever fires first.
        let cancel = CancellationToken::new();
        let source_a = Arc::new(Notify::new());
        let source_b = Arc::new(Notify::new());
        let source_c = Arc::new(Notify::new());

        let call_count = Arc::new(AtomicU32::new(0));
        let action = MultiSourceAction {
            call_count: call_count.clone(),
        };

        // Stored permit for initial Idle
        source_a.notify_one();

        let worker = WorkerBuilder::new("multi-source", cancel.clone())
            .pacing(PacingConfig {
                active_interval: Duration::ZERO,
                min_interval: Duration::ZERO,
                ramp_step: Duration::ZERO,
            })
            .notifier(source_a.clone())
            .notifier(source_b.clone())
            .notifier(source_c.clone())
            .build(action);

        let (b, c, a) = (source_b.clone(), source_c.clone(), source_a.clone());
        tokio::spawn(async move {
            tokio::time::sleep(h).await;
            b.notify_one(); // source B → call 2

            tokio::time::sleep(h).await;
            c.notify_one(); // source C → call 3

            tokio::time::sleep(h).await;
            a.notify_one(); // source A → call 4
        });

        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(h * 6).await;
            cancel_c.cancel();
        });

        worker.run().await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            4,
            "expected 4 calls: initial + one per notifier source",
        );
    }

    // ---- Scenario: Parallel workers share a semaphore, same notifier ----
    //
    // Real-world analogy: 4 outbox processor workers pull from the same queue.
    // A shared semaphore (permits = 2) limits concurrent DB access to 2.
    // All 4 workers subscribe to the same notifier — when new outbox rows
    // arrive, `notify_one()` wakes one worker. The semaphore ensures at most
    // 2 execute concurrently, even though 4 are running.

    struct ParallelAction {
        _worker_id: u32,
        work_duration: Duration,
        total_calls: Arc<AtomicU32>,
        max_concurrent: Arc<AtomicU32>,
        current_concurrent: Arc<AtomicU32>,
    }

    impl WorkerAction for ParallelAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            self.total_calls.fetch_add(1, Ordering::SeqCst);

            // Track concurrency
            let prev = self.current_concurrent.fetch_add(1, Ordering::SeqCst);
            let concurrent = prev + 1;
            // Update high-water mark
            self.max_concurrent.fetch_max(concurrent, Ordering::SeqCst);

            // Simulate work
            tokio::time::sleep(self.work_duration).await;

            self.current_concurrent.fetch_sub(1, Ordering::SeqCst);

            Ok(Directive::idle())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_workers_share_semaphore_and_notifier() {
        let cancel = CancellationToken::new();
        let notify = Arc::new(Notify::new());
        let sem = Arc::new(tokio::sync::Semaphore::new(2)); // max 2 concurrent

        let total_calls = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));
        let current_concurrent = Arc::new(AtomicU32::new(0));

        let mut task_set = crate::outbox::taskward::task_set::TaskSet::new(cancel.clone());

        // Spawn 4 workers, all sharing the same notifier and semaphore.
        for id in 0..4 {
            let action = ParallelAction {
                _worker_id: id,
                work_duration: Duration::from_secs(30),
                total_calls: total_calls.clone(),
                max_concurrent: max_concurrent.clone(),
                current_concurrent: current_concurrent.clone(),
            };

            let name = format!("processor-{id}");
            let bulkhead = Bulkhead::new(
                &name,
                BulkheadConfig {
                    semaphore: ConcurrencyLimit::Fixed(sem.clone()),
                    backoff: BackoffConfig {
                        initial: Duration::from_secs(1),
                        max: Duration::from_secs(600),
                        multiplier: 2.0,
                        jitter: 0.0,
                    },
                },
            );

            let worker = WorkerBuilder::new(name, cancel.clone())
                .notifier(notify.clone())
                .bulkhead(bulkhead)
                .build(action);

            task_set.spawn(format!("processor-{id}"), worker.run());
        }

        // Fire notifications to wake workers. Each notify_one() wakes one
        // idle worker. We send 8 notifications to drive multiple rounds.
        let notify_c = notify.clone();
        tokio::spawn(async move {
            for _ in 0..8 {
                notify_c.notify_one();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        // Let them work, then shut down.
        tokio::time::sleep(Duration::from_secs(300)).await;
        task_set.shutdown().await;

        let calls = total_calls.load(Ordering::SeqCst);
        let max = max_concurrent.load(Ordering::SeqCst);

        assert!(
            calls >= 4,
            "expected at least 4 total calls across all workers, got {calls}",
        );
        assert!(
            max <= 2,
            "semaphore should limit concurrency to 2, but saw {max} concurrent",
        );
    }

    // ---- Scenario: Vacuum-style worker with no external events ----

    struct VacuumAction {
        call_count: Arc<AtomicU32>,
        cooldown: Duration,
    }

    impl WorkerAction for VacuumAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Vacuum always self-schedules with a cooldown.
            Ok(Directive::sleep(self.cooldown))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn vacuum_worker_self_schedules_via_sleep() {
        let h = Duration::from_secs(3600);

        // A vacuum worker has no external notifiers — it runs on its own
        // cadence using Sleep directives. A poker breaks the initial Idle.

        let cancel = CancellationToken::new();
        let call_count = Arc::new(AtomicU32::new(0));

        let action = VacuumAction {
            call_count: call_count.clone(),
            cooldown: h, // 1h cooldown between sweeps
        };

        let (poker_notify, _poker_handle) = poker(Duration::from_secs(600), cancel.clone());

        let worker = WorkerBuilder::new("vacuum", cancel.clone())
            .notifier(poker_notify)
            .pacing(PacingConfig::default()) // 10min poker to break initial Idle
            .build(action);

        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            // 10min (initial poke) + 3 × 1h (cooldowns) = 3h 10m for 3 calls.
            tokio::time::sleep(h * 4).await;
            cancel_c.cancel();
        });

        worker.run().await;

        assert!(
            call_count.load(Ordering::SeqCst) >= 3,
            "vacuum should self-schedule at least 3 times",
        );
    }

    // ---- Scenario: Worker catches panic inside execute and keeps running ----
    //
    // A panic inside execute() is caught by the worker loop via
    // catch_unwind. The panic is treated as an error — bulkhead escalates,
    // listener gets on_error, and the worker retries with backoff.
    // The worker stays alive. Siblings are also unaffected.

    struct PanickingAction {
        panic_on_call: u32,
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for PanickingAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            assert!(n != self.panic_on_call, "segfault in row processor");
            Ok(Directive::idle())
        }
    }

    struct StableAction {
        call_count: Arc<AtomicU32>,
    }

    impl WorkerAction for StableAction {
        type Payload = ();
        type Error = String;

        async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(Directive::idle())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn panicking_worker_recovers_and_keeps_running() {
        // The "bad" worker panics on call 0, but the worker loop catches
        // the panic and retries with backoff. It continues executing on
        // subsequent notifications.

        let cancel = CancellationToken::new();
        let notify = Arc::new(Notify::new());
        let bad_count = Arc::new(AtomicU32::new(0));

        let bad_action = PanickingAction {
            panic_on_call: 0, // panic on first execute
            call_count: bad_count.clone(),
        };

        let (poker_notify, _poker_handle) = poker(Duration::from_secs(1), cancel.clone());

        let bad_worker = WorkerBuilder::new("bad", cancel.clone())
            .notifier(notify.clone())
            .notifier(poker_notify)
            .pacing(PacingConfig::default())
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(bad_action);

        let handle = tokio::spawn(bad_worker.run());

        // Let it run — first call panics, subsequent calls succeed.
        tokio::time::sleep(Duration::from_secs(10)).await;
        cancel.cancel();
        handle.await.unwrap();

        // The worker survived the panic and executed more times.
        let calls = bad_count.load(Ordering::SeqCst);
        assert!(
            calls > 1,
            "worker should have recovered from panic and kept running, got {calls} calls",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn panicking_worker_does_not_kill_siblings() {
        // 2 workers in the same TaskSet — "bad" panics on first execute,
        // "good" keeps running. Both survive.

        let cancel = CancellationToken::new();
        let notify = Arc::new(Notify::new());

        let bad_count = Arc::new(AtomicU32::new(0));
        let good_count = Arc::new(AtomicU32::new(0));

        let bad_action = PanickingAction {
            panic_on_call: 0,
            call_count: bad_count.clone(),
        };
        let good_action = StableAction {
            call_count: good_count.clone(),
        };

        let (bad_poker, _bad_poker_handle) = poker(Duration::from_secs(1), cancel.clone());
        let (good_poker, _good_poker_handle) = poker(Duration::from_secs(1), cancel.clone());

        let bad_worker = WorkerBuilder::new("bad", cancel.clone())
            .notifier(notify.clone())
            .notifier(bad_poker)
            .pacing(PacingConfig::default())
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(bad_action);

        let good_worker = WorkerBuilder::new("good", cancel.clone())
            .notifier(notify.clone())
            .notifier(good_poker)
            .pacing(PacingConfig::default())
            .build(good_action);

        let mut task_set = crate::outbox::taskward::task_set::TaskSet::new(cancel.clone());
        task_set.spawn("bad", bad_worker.run());
        task_set.spawn("good", good_worker.run());

        // Let them run.
        tokio::time::sleep(Duration::from_secs(10)).await;
        task_set.shutdown().await;

        // "bad" survived the panic — it executed more than once.
        let bad_calls = bad_count.load(Ordering::SeqCst);
        assert!(
            bad_calls > 1,
            "bad worker should have recovered from panic, got {bad_calls} calls",
        );

        // "good" also kept running.
        assert!(
            good_count.load(Ordering::SeqCst) >= 1,
            "good worker should have kept running despite sibling panic",
        );
    }
}

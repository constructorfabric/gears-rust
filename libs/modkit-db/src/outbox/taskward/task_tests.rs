use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use super::*;

// ---- Inline structs (moved to top to satisfy items_after_statements) ----

struct AlwaysContinue(Arc<AtomicU32>);
impl WorkerAction for AlwaysContinue {
    type Payload = ();
    type Error = String;
    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Directive::proceed())
    }
}

struct CheckCancel {
    saw_cancelled: bool,
}
impl WorkerAction for CheckCancel {
    type Payload = ();
    type Error = String;
    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, String> {
        if cancel.is_cancelled() {
            self.saw_cancelled = true;
        }
        Ok(Directive::sleep(Duration::from_secs(60)))
    }
}

struct OrderedListener {
    id: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}
impl WorkerListener for OrderedListener {
    fn on_start(&self) {
        self.log.lock().unwrap().push(format!("{}:start", self.id));
    }
    fn on_stop(&self) {
        self.log.lock().unwrap().push(format!("{}:stop", self.id));
    }
}

// ---- Mock Action ----

struct MockAction {
    results: VecDeque<Result<Directive, String>>,
    call_count: Arc<AtomicU32>,
}

impl MockAction {
    fn new(results: Vec<Result<Directive, String>>) -> Self {
        Self {
            results: results.into(),
            call_count: Arc::new(AtomicU32::new(0)),
        }
    }

    fn call_count(&self) -> Arc<AtomicU32> {
        self.call_count.clone()
    }
}

impl WorkerAction for MockAction {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.results
            .pop_front()
            .unwrap_or(Ok(Directive::sleep(Duration::from_secs(60))))
    }
}

/// Zero-pacing config — Proceed is immediate, no auto-poker.
fn zero_pacing() -> PacingConfig {
    PacingConfig {
        min_interval: Duration::ZERO,
        active_interval: Duration::ZERO,
        ramp_step: Duration::ZERO,
    }
}

/// Build a worker with a stored-permit notifier to break the initial Idle
/// and zero adaptive pacing. No poker — Idle blocks until explicit notify
/// or cancel.
fn worker_with_stored_permit(
    action: MockAction,
    cancel: CancellationToken,
) -> (WorkerTask<MockAction>, Arc<Notify>) {
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // stored permit breaks initial Idle
    let worker = WorkerBuilder::new("test", cancel)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);
    (worker, notify)
}

fn worker_with_notifier(
    action: MockAction,
    notify: Arc<Notify>,
    cancel: CancellationToken,
) -> WorkerTask<MockAction> {
    WorkerBuilder::new("test", cancel)
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action)
}

// ---- Builder Tests ----

#[test]
fn builder_no_notifiers() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![]);
    // idle_interval=ZERO suppresses auto-poker
    let worker = WorkerBuilder::new("test", cancel)
        .pacing(PacingConfig {
            ..Default::default()
        })
        .build(action);
    assert!(worker.notifiers.is_empty());
}

#[test]
fn builder_single_notifier() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .pacing(PacingConfig {
            ..Default::default()
        })
        .notifier(notify)
        .build(action);
    assert_eq!(worker.notifiers.len(), 1);
}

#[test]
fn builder_multiple_notifiers() {
    let cancel = CancellationToken::new();
    let n1 = Arc::new(Notify::new());
    let n2 = Arc::new(Notify::new());
    let n3 = Arc::new(Notify::new());
    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .pacing(PacingConfig {
            ..Default::default()
        })
        .notifier(n1)
        .notifier(n2)
        .notifier(n3)
        .build(action);
    assert_eq!(worker.notifiers.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn builder_tuning_idle_interval_defers_poker_to_run() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .pacing(PacingConfig {
            ..Default::default()
        })
        .build(action);
    // Poker is deferred to run() — build() must not tokio::spawn.
    assert_eq!(worker.notifiers.len(), 0);
}

#[tokio::test(start_paused = true)]
async fn builder_notifier_plus_deferred_poker() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .notifier(notify)
        .pacing(PacingConfig {
            ..Default::default()
        })
        .build(action);
    // Only the explicit notifier at build time; poker deferred to run().
    assert_eq!(worker.notifiers.len(), 1);
}

#[test]
fn builder_with_bulkhead() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(60),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![]);
    let _worker = WorkerBuilder::new("test", cancel)
        .pacing(zero_pacing())
        .bulkhead(bulkhead)
        .build(action);
}

// ---- Scheduling Tests ----

#[tokio::test(start_paused = true)]
async fn continue_executes_immediately() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let action = MockAction::new(vec![
        Ok(Directive::proceed()),
        Ok(Directive::proceed()),
        Ok(Directive::proceed()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    // Zero pacing so Proceed is truly immediate, no poker
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn sleep_respects_duration() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![
        Ok(Directive::sleep(Duration::from_millis(100))),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let (worker, _notify) = worker_with_stored_permit(action, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() >= Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn sleep_zero_is_immediate() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![
        Ok(Directive::sleep(Duration::ZERO)),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let (worker, _notify) = worker_with_stored_permit(action, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn idle_wakes_on_notify() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Store permit for initial Idle
    notify.notify_one();
    let action = MockAction::new(vec![
        Ok(Directive::idle()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .notifier(notify.clone())
        .build(action);

    // Send notify to wake from the action-returned Idle
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn idle_with_no_notifiers_blocks_until_cancel() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![]);
    let count = action.call_count();
    // No notifiers at all — Idle blocks forever, only cancel wakes
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(PacingConfig {
            ..Default::default()
        })
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn sleep_wakes_early_on_notify() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Store permit for initial Idle
    notify.notify_one();
    let action = MockAction::new(vec![
        Ok(Directive::sleep(Duration::from_millis(100))),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify.clone(), cancel.clone());

    // Send notify during Sleep — soft sleep wakes early at 20ms
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        notify_c.notify_one();
    });

    // Cancel shortly after the early wake — proves sleep ended before 100ms
    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    // Second call happened because the 100ms sleep was interrupted at 20ms
    assert_eq!(count.load(Ordering::SeqCst), 2);
    // Total elapsed: ~50ms (cancel), well under 100ms of original sleep
    assert!(start.elapsed() < Duration::from_millis(100));
}

// ---- Multi-Notifier Tests ----

#[tokio::test(start_paused = true)]
async fn single_notifier_wakes() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // for initial Idle
    let action = MockAction::new(vec![
        Ok(Directive::idle()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify.clone(), cancel.clone());

    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn multiple_notifiers_first_one_wakes() {
    let cancel = CancellationToken::new();
    let n1 = Arc::new(Notify::new());
    let n2 = Arc::new(Notify::new());
    let n3 = Arc::new(Notify::new());
    // Store permit on n1 for initial Idle
    n1.notify_one();
    let action = MockAction::new(vec![
        Ok(Directive::idle()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .notifier(n1)
        .notifier(n2.clone())
        .notifier(n3)
        .build(action);

    // Fire n2 to wake from the action-returned Idle
    let n2_c = n2.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        n2_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn zero_notifiers_blocks_until_cancel() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(PacingConfig {
            ..Default::default()
        })
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn stored_permit_consumed_immediately() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Store permit before worker starts
    notify.notify_one();

    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    // Stored permit resolved initial Idle immediately
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ---- Cancellation Tests ----

#[tokio::test(start_paused = true)]
async fn cancel_during_idle() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Do NOT store permit — initial Idle blocks until cancel fires
    let action = MockAction::new(vec![]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(start.elapsed() < Duration::from_millis(150));
}

#[tokio::test(start_paused = true)]
async fn cancel_during_sleep() {
    let cancel = CancellationToken::new();
    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(10)))]);
    let (worker, _notify) = worker_with_stored_permit(action, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[tokio::test]
async fn cancel_between_continues() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle

    let count = Arc::new(AtomicU32::new(0));
    let count_c = count.clone();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(zero_pacing())
        .notifier(notify)
        .build(AlwaysContinue(count_c));

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert!(count.load(Ordering::SeqCst) > 0);
}

#[tokio::test(start_paused = true)]
async fn cancel_visible_in_execute() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(PacingConfig { ..zero_pacing() })
        .notifier(notify)
        .build(CheckCancel {
            saw_cancelled: false,
        });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_c.cancel();
    });

    worker.run().await;
}

#[tokio::test(start_paused = true)]
async fn already_cancelled() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let action = MockAction::new(vec![]);
    let count = action.call_count();
    let (worker, _notify) = worker_with_stored_permit(action, cancel);

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

// ---- Error Absorption Tests ----

#[tokio::test(start_paused = true)]
async fn error_triggers_escalation_and_retry() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let action = MockAction::new(vec![
        Err("boom".to_owned()),
        Ok(Directive::proceed()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Fire notify to wake from error-Idle
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn error_on_first_call_retries() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let action = MockAction::new(vec![
        Err("fail".to_owned()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Fire notify to wake from error-Idle
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn error_applies_backoff_floor() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_millis(100),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![
        Err("fail".to_owned()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Fire notify to wake from error-Idle (after backoff floor)
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() >= Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn consecutive_errors_escalate() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_millis(10),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![
        Err("e1".to_owned()),
        Err("e2".to_owned()),
        Err("e3".to_owned()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Fire notifies to wake from each error-Idle
    let notify_c = notify.clone();
    tokio::spawn(async move {
        for delay_ms in [20, 50, 100] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            notify_c.notify_one();
        }
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 4);
    // 3 errors: backoff 10ms + 20ms + 40ms = 70ms minimum
    assert!(start.elapsed() >= Duration::from_millis(70));
}

#[tokio::test(start_paused = true)]
async fn success_resets_bulkhead() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_millis(50),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![
        Err("e1".to_owned()),     // backoff → 50ms
        Err("e2".to_owned()),     // backoff → 100ms
        Ok(Directive::proceed()), // reset
        Err("e3".to_owned()),     // backoff → 50ms (reset, not 200ms)
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Fire notifies to wake from each error-Idle (3 errors → 3 notifies)
    let notify_c = notify.clone();
    tokio::spawn(async move {
        for delay_ms in [60, 120, 10] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            notify_c.notify_one();
        }
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 5);
}

#[tokio::test(start_paused = true)]
async fn error_sets_directive_to_idle() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Store permit for initial Idle
    notify.notify_one();
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_millis(10),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![
        Err("fail".to_owned()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    // Send notify after error — should wake from the Idle
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        notify_c.notify_one();
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ---- Error Backoff Floor Tests ----

#[tokio::test(start_paused = true)]
async fn error_backoff_delays_idle() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    let mut bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_millis(200),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    bulkhead.escalate();
    let action = MockAction::new(vec![
        Ok(Directive::idle()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .notifier(notify.clone())
        .pacing(zero_pacing())
        .build(action);

    // Notify to wake from initial Idle (after error backoff floor)
    // and from the action-returned Idle.
    let notify_c = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        notify_c.notify_one(); // wake from initial Idle
        tokio::time::sleep(Duration::from_millis(50)).await;
        notify_c.notify_one(); // wake from action-returned Idle
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    // Initial Idle waits for error backoff floor (200ms) then notify at 250ms,
    // execute returns Idle, second Idle waits for notify at 300ms.
    assert!(start.elapsed() >= Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn adaptive_pacing_ramps_down_on_proceed() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let action = MockAction::new(vec![
        Ok(Directive::proceed()),
        Ok(Directive::proceed()),
        Ok(Directive::proceed()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    // active_interval=30ms, min_interval=10ms, ramp_step=10ms
    // Proceed pacing: 30ms → 20ms → 10ms
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(PacingConfig {
            active_interval: Duration::from_millis(30),
            min_interval: Duration::from_millis(10),
            ramp_step: Duration::from_millis(10),
        })
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 4);
    // Total pacing: 30 + 20 + 10 = 60ms minimum
    assert!(start.elapsed() >= Duration::from_millis(60));
}

#[tokio::test(start_paused = true)]
async fn zero_pacing_no_delay() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let action = MockAction::new(vec![
        Ok(Directive::proceed()),
        Ok(Directive::proceed()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert!(start.elapsed() < Duration::from_millis(200));
}

// ---- Semaphore Integration Tests ----

#[tokio::test(start_paused = true)]
async fn semaphore_acquired_before_execute() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::from_millis(100),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn semaphore_blocks_until_released() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = sem.clone().acquire_owned().await.unwrap();
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::from_millis(100),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(permit);
    });

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(start.elapsed() >= Duration::from_millis(50));
}

#[tokio::test(start_paused = true)]
async fn cancel_during_semaphore_wait() {
    use crate::outbox::taskward::bulkhead::{
        BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit,
    };
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let sem = Arc::new(tokio::sync::Semaphore::new(0));
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::from_millis(100),
                max: Duration::from_secs(10),
                multiplier: 2.0,
                jitter: 0.0,
            },
        },
    );
    let action = MockAction::new(vec![]);
    let count = action.call_count();
    let worker = WorkerBuilder::new("test", cancel.clone())
        .bulkhead(bulkhead)
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(start.elapsed() < Duration::from_millis(150));
}

// ---- Notify Semantics Tests ----

#[tokio::test(start_paused = true)]
async fn stored_permit() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // Store a permit BEFORE the worker starts — consumed by initial Idle
    notify.notify_one();

    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn multiple_coalesce() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    // 5 notifications coalesce to one stored permit
    for _ in 0..5 {
        notify.notify_one();
    }

    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let count = action.call_count();
    let worker = worker_with_notifier(action, notify, cancel.clone());

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    let start = Instant::now();
    worker.run().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ---- Listener Integration Tests ----

#[derive(Default)]
struct RecordingListener {
    events: Arc<Mutex<Vec<String>>>,
}

impl RecordingListener {
    fn events(&self) -> Arc<Mutex<Vec<String>>> {
        self.events.clone()
    }
}

impl WorkerListener for RecordingListener {
    fn on_start(&self) {
        self.events.lock().unwrap().push("start".into());
    }
    fn on_stop(&self) {
        self.events.lock().unwrap().push("stop".into());
    }
    fn on_execute_start(&self) {
        self.events.lock().unwrap().push("execute_start".into());
    }
    fn on_complete(&self, _dur: Duration, _dir: &Directive) {
        self.events.lock().unwrap().push("complete".into());
    }
    fn on_error(&self, _dur: Duration, _err: &str, _fails: u32, _backoff: Duration) {
        self.events.lock().unwrap().push("error".into());
    }
    fn on_idle(&self) {
        self.events.lock().unwrap().push("idle".into());
    }
    fn on_sleep(&self, _dur: Duration) {
        self.events.lock().unwrap().push("sleep".into());
    }
}

#[tokio::test(start_paused = true)]
async fn listener_called_on_start_stop() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let listener = RecordingListener::default();
    let events = listener.events();
    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .listener(listener)
        .build(action);

    worker.run().await;
    let events = events.lock().unwrap();
    assert_eq!(events.first(), Some(&"start".to_owned()));
    assert_eq!(events.last(), Some(&"stop".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn listener_called_on_complete() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let listener = RecordingListener::default();
    let events = listener.events();
    let action = MockAction::new(vec![Ok(Directive::sleep(Duration::from_secs(60)))]);
    let worker = WorkerBuilder::new("test", cancel.clone())
        .listener(listener)
        .pacing(zero_pacing())
        .notifier(notify)
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    let events = events.lock().unwrap();
    assert!(events.contains(&"complete".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn listener_called_on_error_with_context() {
    let cancel = CancellationToken::new();
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // break initial Idle
    let listener = RecordingListener::default();
    let events = listener.events();
    let action = MockAction::new(vec![
        Err("boom".to_owned()),
        Ok(Directive::sleep(Duration::from_secs(60))),
    ]);
    let worker = WorkerBuilder::new("test", cancel.clone())
        .listener(listener)
        .pacing(zero_pacing())
        .notifier(notify.clone())
        .build(action);

    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel_c.cancel();
    });

    worker.run().await;
    let events = events.lock().unwrap();
    assert!(events.contains(&"error".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn multiple_listeners_called_in_order() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let shared = Arc::new(Mutex::new(Vec::new()));

    let action = MockAction::new(vec![]);
    let worker = WorkerBuilder::new("test", cancel)
        .listener(OrderedListener {
            id: "A",
            log: shared.clone(),
        })
        .listener(OrderedListener {
            id: "B",
            log: shared.clone(),
        })
        .build(action);

    worker.run().await;
    let log = shared.lock().unwrap();
    assert_eq!(&log[..], &["A:start", "B:start", "A:stop", "B:stop"]);
}

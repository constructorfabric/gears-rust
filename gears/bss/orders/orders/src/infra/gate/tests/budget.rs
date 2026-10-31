//! `bounded` under paused tokio time. In-crate: runs in the `orders-pg` CI lane with
//! `--features gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use crate::config::{BreakerConfig, CommercialIdentityConfig, GateConfig, OrdersConfig};
use crate::domain::gate::breaker::{BreakerPolicy, BreakerState};
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::infra::gate::budget::{
    ACCEPTANCE_DEADLINE, AcceptanceBudget, Budget, CallError, PORT_BULKHEAD, RETRY_PAUSE,
    RunBudget, bounded, to_unavailable,
};
use crate::infra::gate::signals::{GateObservation, NoGateSignals, RecordingGateSignals};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::{CanonicalError, resource_error};

#[resource_error(toolkit_gts::gts_id!("cf.bss.orders.order.v1~"))]
struct TestError;

const PORT: GatePort = GatePort::Resolve;

fn budget_with(deadline_ms: u32) -> Budget {
    let mut config = GateConfig::default();
    config.deadline_ms.insert(PORT, deadline_ms);
    config.validate().unwrap();
    Budget::from_config(&config, Arc::new(NoGateSignals))
}

fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable().create()
}

fn invalid_argument() -> CanonicalError {
    TestError::invalid_argument()
        .with_field_violation("sku", "unknown", "UNKNOWN_SKU")
        .create()
}

/// The baseline gate config with `policy` as its enabled breaker (whole seconds).
fn gate_config_with_breaker(policy: BreakerPolicy) -> GateConfig {
    let seconds = |d: Duration| u32::try_from(d.as_secs()).unwrap();
    GateConfig {
        breaker: BreakerConfig {
            enabled: true,
            failure_rate: policy.failure_rate,
            window_seconds: seconds(policy.window),
            open_seconds: seconds(policy.open_for),
            min_calls: policy.min_calls,
        },
        ..GateConfig::default()
    }
}

/// A minimal valid gear config carrying `gate`.
fn full_config_with_gate(gate: GateConfig) -> OrdersConfig {
    let mut config: OrdersConfig = serde_json::from_value(serde_json::json!({
        "lock_route": "direct", "idempotency_lease_seconds": 30, "dependency_timeout_ms": 1000,
        "service_principals": [{"role": "workflow", "subject_id": uuid::Uuid::from_u128(77), "tenant_id": uuid::Uuid::from_u128(1)}],
        "events": {"producer_subject_id": uuid::Uuid::from_u128(78), "producer_tenant_id": uuid::Uuid::from_u128(1)},
        "audit_minimization": {"key_id": "t1", "key": "0123456789abcdef0123456789abcdef"}
    }))
    .unwrap();
    config.gate = gate;
    config
}

fn identity(seller: u128) -> CommercialIdentityConfig {
    CommercialIdentityConfig {
        seller_tenant_id: uuid::Uuid::from_u128(seller),
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: Vec::new(),
    }
}

/// Let every spawned task run to its first park (paused time advances only when all are idle).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

#[test]
fn config_rejects_unknown_key_zero_and_duplicate_identity() {
    let mut config = GateConfig::default();
    config.deadline_ms.insert(GatePort::Resolve, 0);
    assert!(config.validate().is_err());
    let mut config = GateConfig {
        commercial_identities: vec![identity(1), identity(2)],
        ..GateConfig::default()
    };
    assert!(config.validate().is_ok());
    config.commercial_identities.push(identity(1));
    assert!(config.validate().is_err());
    let baseline = Budget::from_config(&GateConfig::default(), Arc::new(NoGateSignals));
    assert_eq!(
        baseline.deadline(GatePort::Evaluation),
        Duration::from_millis(500)
    );
    assert_eq!(
        baseline.authority_exchange_deadline(),
        Duration::from_millis(250)
    );
    let mut config = GateConfig {
        authority_exchange_deadline_ms: Some(0),
        ..GateConfig::default()
    };
    assert!(config.validate().is_err());
    config.authority_exchange_deadline_ms = Some(120);
    config.validate().unwrap();
    assert_eq!(
        Budget::from_config(&config, Arc::new(NoGateSignals)).authority_exchange_deadline(),
        Duration::from_millis(120)
    );
}

#[test]
fn config_refuses_an_unresolved_empty_or_blank_commercial_identity() {
    type Break = fn(&mut CommercialIdentityConfig);
    let seller = uuid::Uuid::from_u128(7);
    let cases: [(&str, Break); 4] = [
        ("unresolved", |i| {
            i.client_secret = secrecy::SecretString::from("${ORDERS_SECRET_NOT_SET}");
        }),
        ("empty", |i| {
            i.client_secret = secrecy::SecretString::from("");
        }),
        ("blank client_id", |i| {
            i.client_id = "  ".to_owned();
        }),
        ("blank scope", |i| {
            i.scopes = vec!["read".to_owned(), " ".to_owned()];
        }),
    ];
    for (name, break_it) in cases {
        let mut id = identity(7);
        break_it(&mut id);
        let config = GateConfig {
            commercial_identities: vec![id],
            ..GateConfig::default()
        };
        let message = config.validate().unwrap_err().to_string();
        assert!(message.contains(&seller.to_string()), "{name}: {message}");
        assert!(
            !message.contains("ORDERS_SECRET_NOT_SET"),
            "{name}: {message}"
        );
    }
}

#[test]
fn config_refuses_gate_deadlines_above_sixty_seconds() {
    for (ms, ok) in [(60_000, true), (60_001, false)] {
        let mut config = GateConfig::default();
        config.deadline_ms.insert(GatePort::Resolve, ms);
        assert_eq!(config.validate().is_ok(), ok, "port {ms}");
        let config = GateConfig {
            authority_exchange_deadline_ms: Some(ms),
            ..GateConfig::default()
        };
        assert_eq!(config.validate().is_ok(), ok, "authority {ms}");
        let config = GateConfig {
            acceptance_deadline_ms: Some(ms),
            ..GateConfig::default()
        };
        assert_eq!(config.validate().is_ok(), ok, "acceptance {ms}");
    }
}

#[test]
fn gate_config_refuses_an_unknown_port_at_parse() {
    let error =
        serde_json::from_value::<GateConfig>(serde_json::json!({"deadline_ms": {"nope": 10}}))
            .unwrap_err()
            .to_string();
    assert!(error.contains("`nope`"), "{error}");
    assert!(error.contains("current-revision"), "{error}");
    assert!(error.contains("indicative-tax"), "{error}");
    let config =
        serde_json::from_value::<GateConfig>(serde_json::json!({"deadline_ms": {"resolve": 300}}))
            .unwrap();
    assert_eq!(config.deadline_ms.get(&GatePort::Resolve), Some(&300));
    assert_eq!(config.deadline_ms.len(), 1);
}

#[test]
fn every_gate_port_token_round_trips_through_deserialize() {
    for port in GatePort::ALL {
        let parsed: GatePort = serde_json::from_value(serde_json::json!(port.token())).unwrap();
        assert_eq!(parsed, port);
        assert_eq!(GatePort::from_token(port.token()), Some(port));
    }
    assert_eq!(GatePort::from_token("Resolve"), None);
    assert_eq!(GatePort::from_token(""), None);
}

#[tokio::test(start_paused = true)]
async fn deadline_fires_at_the_configured_value() {
    let budget = budget_with(300);
    let run = RunBudget::new();
    let start = Instant::now();
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(start.elapsed(), Duration::from_millis(300));
    assert_eq!(to_unavailable(&CallError::Deadline), Unavailable::Deadline);
}

#[tokio::test(start_paused = true)]
async fn transient_failure_retries_once_then_succeeds() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result = bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(unavailable())
        } else {
            Ok(7_u8)
        }
    })
    .await;
    assert_eq!(result.unwrap(), 7);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_second_transient_failure_is_not_retried_again() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(unavailable())
        })
        .await;
    let error = result.unwrap_err();
    assert!(matches!(error, CallError::Transient(_)));
    assert_eq!(to_unavailable(&error), Unavailable::Outage);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn transient_failure_after_the_deadline_does_not_retry() {
    let budget = budget_with(100);
    let run = RunBudget::new();
    let calls = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&calls);
    // The failure lands exactly at the deadline: tokio's `timeout_at` polls the inner future
    // before the timer, so the transient error wins, and no time remains for a second attempt.
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Err(unavailable())
            }
        })
        .await;
    assert!(matches!(result, Err(CallError::Transient(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn sequential_calls_on_one_port_share_the_run_deadline() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    let start = Instant::now();
    bounded(&budget, &run, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(())
    })
    .await
    .unwrap();
    // The second call starts 200 ms into the port's 250 ms run deadline: 50 ms remain.
    let result: Result<(), CallError> = bounded(&budget, &run, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(())
    })
    .await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(start.elapsed(), Duration::from_millis(250));
    // Past the deadline a further call is refused at once and spends no subrequest.
    let spent = run.subrequests(PORT);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &cancel, || async { Ok(()) }).await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(run.subrequests(PORT), spent);
    assert_eq!(start.elapsed(), Duration::from_millis(250));
}

#[tokio::test(start_paused = true)]
async fn the_retry_runs_under_the_first_attempts_deadline() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let start = Instant::now();
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Err(unavailable())
            } else {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(())
            }
        })
        .await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(start.elapsed(), Duration::from_millis(250));
}

#[tokio::test(start_paused = true)]
async fn different_ports_in_one_run_have_independent_deadlines() {
    // Resolve: 250 ms (configured); Evaluation: 500 ms (baseline).
    let budget = budget_with(250);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    let start = Instant::now();
    bounded(&budget, &run, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(())
    })
    .await
    .unwrap();
    // Evaluation's deadline starts at its own first attempt (200 ms), not at Resolve's.
    let result: Result<(), CallError> =
        bounded(&budget, &run, GatePort::Evaluation, &cancel, || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(start.elapsed(), Duration::from_millis(700));
    // A fresh run starts every port's deadline afresh.
    let fresh = RunBudget::new();
    let value = bounded(&budget, &fresh, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(1_u8)
    })
    .await
    .unwrap();
    assert_eq!(value, 1);
}

#[tokio::test(start_paused = true)]
async fn permanent_error_never_retries() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(TestError::permission_denied()
                .with_reason("DENIED")
                .create())
        })
        .await;
    let error = result.unwrap_err();
    assert!(matches!(error, CallError::Permanent(_)));
    assert_eq!(to_unavailable(&error), Unavailable::Denied);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn an_aborted_with_a_business_code_is_permanent() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(TestError::aborted("conflict")
                .with_reason("REVISION_NOT_PUBLISHED")
                .create())
        })
        .await;
    assert!(matches!(result, Err(CallError::Permanent(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn an_aborted_without_a_business_code_retries_once() {
    // "No business code" is an empty `ctx.reason`; the SDK builder always sets one, so this is
    // the shape of a wire-decoded abort.
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result = bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TestError::aborted("conflict").with_reason("").create())
        } else {
            Ok(3_u8)
        }
    })
    .await;
    assert_eq!(result.unwrap(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn an_invalid_argument_is_permanent_and_malformed() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(invalid_argument())
        })
        .await;
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        CallError::Permanent(CanonicalError::InvalidArgument { .. })
    ));
    assert_eq!(to_unavailable(&error), Unavailable::Malformed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Park `n` calls on `port`, counting each admission into the closure in `entered`; return their
/// handles plus the gate that releases them.
fn park(
    budget: &Arc<Budget>,
    run: &Arc<RunBudget>,
    port: GatePort,
    n: usize,
    entered: &Arc<AtomicUsize>,
) -> (Vec<tokio::task::JoinHandle<()>>, CancellationToken) {
    let release = CancellationToken::new();
    let handles = (0..n)
        .map(|_| {
            let (budget, run, release, entered) = (
                Arc::clone(budget),
                Arc::clone(run),
                release.clone(),
                Arc::clone(entered),
            );
            tokio::spawn(async move {
                drop(
                    bounded(&budget, &run, port, &CancellationToken::new(), || async {
                        entered.fetch_add(1, Ordering::SeqCst);
                        release.cancelled().await;
                        Ok(())
                    })
                    .await,
                );
            })
        })
        .collect();
    (handles, release)
}

/// Park 32 calls on [`PORT`] over eight runs of four (the in-run limit never binds).
fn saturate_port(
    budget: &Arc<Budget>,
    entered: &Arc<AtomicUsize>,
) -> (Vec<tokio::task::JoinHandle<()>>, Vec<CancellationToken>) {
    let mut all = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..8 {
        let run = Arc::new(RunBudget::new());
        let (h, r) = park(budget, &run, PORT, 4, entered);
        all.extend(h);
        releases.push(r);
    }
    (all, releases)
}

#[tokio::test(start_paused = true)]
async fn the_33rd_concurrent_call_on_one_port_is_backpressure() {
    let budget = Arc::new(budget_with(60_000));
    let entered = Arc::new(AtomicUsize::new(0));
    let (all, releases) = saturate_port(&budget, &entered);
    settle().await;
    assert_eq!(entered.load(Ordering::SeqCst), 32);
    let extra = RunBudget::new();
    let result: Result<(), CallError> =
        bounded(&budget, &extra, PORT, &CancellationToken::new(), || async {
            entered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Backpressure)));
    assert_eq!(entered.load(Ordering::SeqCst), 32);
    assert_eq!(
        to_unavailable(&CallError::Backpressure),
        Unavailable::Outage
    );
    for r in releases {
        r.cancel();
    }
    for h in all {
        h.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn a_port_permit_is_released_when_its_call_completes() {
    let budget = Arc::new(budget_with(60_000));
    let entered = Arc::new(AtomicUsize::new(0));
    let (all, releases) = saturate_port(&budget, &entered);
    settle().await;
    assert_eq!(entered.load(Ordering::SeqCst), 32);
    for r in releases {
        r.cancel();
    }
    for h in all {
        h.await.unwrap();
    }
    let result = bounded(
        &budget,
        &RunBudget::new(),
        PORT,
        &CancellationToken::new(),
        || async { Ok(5_u8) },
    )
    .await;
    assert_eq!(result.unwrap(), 5);
}

/// Park eight calls on [`PORT`] in `run`, each with its own release.
fn fill_port_in_run(
    budget: &Arc<Budget>,
    run: &Arc<RunBudget>,
    entered: &Arc<AtomicUsize>,
) -> (Vec<tokio::task::JoinHandle<()>>, Vec<CancellationToken>) {
    let mut handles = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..8 {
        let (h, r) = park(budget, run, PORT, 1, entered);
        handles.extend(h);
        releases.push(r);
    }
    (handles, releases)
}

#[tokio::test(start_paused = true)]
async fn the_9th_concurrent_call_on_one_port_in_one_run_waits_for_a_slot() {
    let budget = Arc::new(budget_with(60_000));
    let run = Arc::new(RunBudget::new());
    let entered = Arc::new(AtomicUsize::new(0));
    let (handles, releases) = fill_port_in_run(&budget, &run, &entered);
    settle().await;
    assert_eq!(entered.load(Ordering::SeqCst), 8);
    let ninth = {
        let (budget, run, entered) = (Arc::clone(&budget), Arc::clone(&run), Arc::clone(&entered));
        tokio::spawn(async move {
            bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
                entered.fetch_add(1, Ordering::SeqCst);
                Ok(9_u8)
            })
            .await
        })
    };
    settle().await;
    // Waiting, not refused: nothing entered yet.
    assert_eq!(entered.load(Ordering::SeqCst), 8);
    assert!(!ninth.is_finished());
    releases[0].cancel();
    assert_eq!(ninth.await.unwrap().unwrap(), 9);
    assert_eq!(entered.load(Ordering::SeqCst), 9);
    for r in releases {
        r.cancel();
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn a_call_waiting_for_an_in_run_slot_times_out_at_the_port_deadline() {
    let budget = Arc::new(budget_with(300));
    let run = Arc::new(RunBudget::new());
    let entered = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let (handles, releases) = fill_port_in_run(&budget, &run, &entered);
    settle().await;
    assert_eq!(entered.load(Ordering::SeqCst), 8);
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            entered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Deadline)));
    assert_eq!(start.elapsed(), Duration::from_millis(300));
    assert_eq!(entered.load(Ordering::SeqCst), 8);
    for r in releases {
        r.cancel();
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn a_full_port_does_not_limit_another_port_in_the_run() {
    let budget = Arc::new(budget_with(60_000));
    let run = Arc::new(RunBudget::new());
    let entered = Arc::new(AtomicUsize::new(0));
    let (handles, releases) = fill_port_in_run(&budget, &run, &entered);
    settle().await;
    let result = bounded(
        &budget,
        &run,
        GatePort::Evaluation,
        &CancellationToken::new(),
        || async { Ok(4_u8) },
    )
    .await;
    assert_eq!(result.unwrap(), 4);
    for r in releases {
        r.cancel();
    }
    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn the_201st_subrequest_on_one_port_is_capped() {
    let budget = budget_with(250);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    for _ in 0..200 {
        bounded(&budget, &run, PORT, &cancel, || async { Ok(()) })
            .await
            .unwrap();
    }
    let result: Result<(), CallError> =
        bounded(&budget, &run, PORT, &cancel, || async { Ok(()) }).await;
    assert!(matches!(result, Err(CallError::SubrequestCapExceeded)));
    assert_eq!(run.subrequests(PORT), 200);
    // The cap is per port: another port in the same run is unaffected.
    let other = GatePort::CurrentRevision;
    bounded(&budget, &run, other, &cancel, || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(run.subrequests(other), 1);
}

#[tokio::test(start_paused = true)]
async fn cancellation_aborts_the_call() {
    let budget = budget_with(60_000);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        trigger.cancel();
    });
    let result: Result<(), CallError> = bounded(&budget, &run, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    })
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, CallError::Cancelled));
    assert_eq!(to_unavailable(&error), Unavailable::Outage);
}

#[tokio::test(start_paused = true)]
async fn ten_transient_failures_open_the_breaker_and_the_next_call_is_rejected_without_a_subrequest()
 {
    let budget = Budget::from_config(
        &gate_config_with_breaker(BreakerPolicy::BASELINE),
        Arc::new(NoGateSignals),
    );
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    for _ in 0..10 {
        let r = bounded(&budget, &run, GatePort::Resolve, &cancel, || async {
            Err::<(), _>(unavailable())
        })
        .await;
        assert!(matches!(r, Err(CallError::Transient(_))));
    }
    assert_eq!(
        budget.breaker_state(GatePort::Resolve, Instant::now()),
        Some(BreakerState::Open)
    );
    let before = run.subrequests(GatePort::Resolve);
    let r = bounded(&budget, &run, GatePort::Resolve, &cancel, || async {
        Ok::<_, CanonicalError>(())
    })
    .await;
    assert!(matches!(r, Err(CallError::BreakerOpen)));
    assert_eq!(
        run.subrequests(GatePort::Resolve),
        before,
        "an open breaker spends no subrequest"
    );
    assert_eq!(to_unavailable(&CallError::BreakerOpen), Unavailable::Outage);
}

#[tokio::test(start_paused = true)]
async fn the_breaker_is_per_port_and_gear_wide() {
    let budget = Budget::from_config(
        &gate_config_with_breaker(BreakerPolicy::BASELINE),
        Arc::new(NoGateSignals),
    );
    let cancel = CancellationToken::new();
    let run_a = RunBudget::new();
    for _ in 0..10 {
        drop(
            bounded(&budget, &run_a, GatePort::Resolve, &cancel, || async {
                Err::<(), _>(unavailable())
            })
            .await,
        );
    }
    let run_b = RunBudget::new();
    assert!(
        matches!(
            bounded(&budget, &run_b, GatePort::Resolve, &cancel, || async {
                Ok::<_, CanonicalError>(())
            })
            .await,
            Err(CallError::BreakerOpen)
        ),
        "another run sees the open breaker"
    );
    assert!(
        bounded(
            &budget,
            &run_b,
            GatePort::CurrentRevision,
            &cancel,
            || async { Ok::<_, CanonicalError>(()) }
        )
        .await
        .is_ok(),
        "another port is unaffected"
    );
}

#[tokio::test(start_paused = true)]
async fn after_open_for_one_probe_runs_and_success_closes() {
    let budget = Budget::from_config(
        &gate_config_with_breaker(BreakerPolicy::BASELINE),
        Arc::new(NoGateSignals),
    );
    let cancel = CancellationToken::new();
    let run = RunBudget::new();
    for _ in 0..10 {
        drop(
            bounded(&budget, &run, GatePort::Resolve, &cancel, || async {
                Err::<(), _>(unavailable())
            })
            .await,
        );
    }
    tokio::time::advance(Duration::from_secs(10)).await;
    assert_eq!(
        budget.breaker_state(GatePort::Resolve, Instant::now()),
        Some(BreakerState::HalfOpen)
    );
    let run2 = RunBudget::new();
    assert!(
        bounded(&budget, &run2, GatePort::Resolve, &cancel, || async {
            Ok::<_, CanonicalError>(())
        })
        .await
        .is_ok()
    );
    assert_eq!(
        budget.breaker_state(GatePort::Resolve, Instant::now()),
        Some(BreakerState::Closed)
    );
}

#[tokio::test(start_paused = true)]
async fn a_deadline_is_a_breaker_failure_and_a_permanent_answer_is_not() {
    let budget = Budget::from_config(
        &gate_config_with_breaker(BreakerPolicy {
            min_calls: 2,
            ..BreakerPolicy::BASELINE
        }),
        Arc::new(NoGateSignals),
    );
    let cancel = CancellationToken::new();
    let run = RunBudget::new();
    drop(
        bounded(&budget, &run, GatePort::SkuRead, &cancel, || async {
            Err::<(), _>(invalid_argument())
        })
        .await,
    );
    drop(
        bounded(&budget, &run, GatePort::SkuRead, &cancel, || async {
            Err::<(), _>(invalid_argument())
        })
        .await,
    );
    assert_eq!(
        budget.breaker_state(GatePort::SkuRead, Instant::now()),
        Some(BreakerState::Closed),
        "two permanent answers: 0 failures of 2"
    );
    // The SkuRead run deadline is 250 ms: the sleep is a Deadline, and it spends run2's port
    // deadline, so the transient call runs in a fresh run.
    let run2 = RunBudget::new();
    let r = bounded(&budget, &run2, GatePort::SkuRead, &cancel, || async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, CanonicalError>(())
    })
    .await;
    assert!(matches!(r, Err(CallError::Deadline)));
    let run3 = RunBudget::new();
    drop(
        bounded(&budget, &run3, GatePort::SkuRead, &cancel, || async {
            Err::<(), _>(unavailable())
        })
        .await,
    );
    assert_eq!(
        budget.breaker_state(GatePort::SkuRead, Instant::now()),
        Some(BreakerState::Open),
        "deadline + transient of 4 = 0.5"
    );
}

#[tokio::test(start_paused = true)]
async fn a_disabled_breaker_reports_none_and_never_rejects() {
    let mut cfg = gate_config_with_breaker(BreakerPolicy::BASELINE);
    cfg.breaker.enabled = false;
    let budget = Budget::from_config(&cfg, Arc::new(NoGateSignals));
    let cancel = CancellationToken::new();
    // One run per call: the retry pauses of twenty calls in one run would spend its port deadline.
    for _ in 0..20 {
        drop(
            bounded(
                &budget,
                &RunBudget::new(),
                GatePort::Resolve,
                &cancel,
                || async { Err::<(), _>(unavailable()) },
            )
            .await,
        );
    }
    assert_eq!(
        budget.breaker_state(GatePort::Resolve, Instant::now()),
        None
    );
    assert!(
        bounded(
            &budget,
            &RunBudget::new(),
            GatePort::Resolve,
            &cancel,
            || async { Ok::<_, CanonicalError>(()) }
        )
        .await
        .is_ok()
    );
}

#[test]
fn breaker_config_validation_names_the_field() {
    let mut cfg = gate_config_with_breaker(BreakerPolicy::BASELINE);
    cfg.breaker.failure_rate = 0.0;
    let err = full_config_with_gate(cfg)
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("gate.breaker"), "{err}");
}

/// A budget with `policy` as its enabled breaker over a recording sink.
fn recorded(policy: BreakerPolicy) -> (Budget, Arc<RecordingGateSignals>) {
    let sink = Arc::new(RecordingGateSignals::default());
    let budget = Budget::from_config(&gate_config_with_breaker(policy), sink.clone());
    (budget, sink)
}

/// The `port_call` outcomes recorded for `port`, in order.
fn port_calls(sink: &RecordingGateSignals, port: GatePort) -> Vec<&'static str> {
    sink.observations()
        .into_iter()
        .filter_map(|o| match o {
            GateObservation::PortCall {
                port: p, outcome, ..
            } if p == port => Some(outcome.token()),
            _ => None,
        })
        .collect()
}

/// The `(outcome, timed)` of every `port_call` recorded for `port`, in order: `timed` is whether
/// a duration was reported (the call reached the callee).
fn port_calls_timed(sink: &RecordingGateSignals, port: GatePort) -> Vec<(&'static str, bool)> {
    sink.observations()
        .into_iter()
        .filter_map(|o| match o {
            GateObservation::PortCall {
                port: p,
                outcome,
                timed,
            } if p == port => Some((outcome.token(), timed)),
            _ => None,
        })
        .collect()
}

/// The breaker states reported for `port`, in order: real transitions only.
fn breaker_reports(sink: &RecordingGateSignals, port: GatePort) -> Vec<BreakerState> {
    sink.observations()
        .into_iter()
        .filter_map(|o| match o {
            GateObservation::Breaker { port: p, state } if p == port => Some(state),
            _ => None,
        })
        .collect()
}

fn retries(sink: &RecordingGateSignals, port: GatePort) -> usize {
    sink.observations()
        .into_iter()
        .filter(|o| *o == GateObservation::PortRetry { port })
        .count()
}

#[tokio::test(start_paused = true)]
async fn construction_reports_every_breaker_closed_without_a_transition_and_a_success_reports_ok() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let observed = sink.observations();
    assert_eq!(
        observed.len(),
        GatePort::ALL.len(),
        "12 initial states, nothing else"
    );
    for port in GatePort::ALL {
        assert!(
            observed.contains(&GateObservation::BreakerInitial {
                port,
                state: BreakerState::Closed,
            }),
            "{port:?}"
        );
    }
    assert!(
        !observed
            .iter()
            .any(|o| matches!(o, GateObservation::Breaker { .. })),
        "construction is not a transition"
    );
    let run = RunBudget::new();
    bounded(
        &budget,
        &run,
        GatePort::Resolve,
        &CancellationToken::new(),
        || async { Ok::<_, CanonicalError>(()) },
    )
    .await
    .unwrap();
    assert_eq!(port_calls(&sink, GatePort::Resolve), vec!["ok"]);
    assert_eq!(
        port_calls_timed(&sink, GatePort::Resolve),
        vec![("ok", true)],
        "a call that reached the callee carries a duration"
    );
    assert_eq!(retries(&sink, GatePort::Resolve), 0);
}

#[tokio::test(start_paused = true)]
async fn a_transient_failure_retried_then_failing_reports_one_retry_and_transient() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let run = RunBudget::new();
    let r = bounded(
        &budget,
        &run,
        GatePort::Resolve,
        &CancellationToken::new(),
        || async { Err::<(), _>(unavailable()) },
    )
    .await;
    assert!(matches!(r, Err(CallError::Transient(_))));
    assert_eq!(retries(&sink, GatePort::Resolve), 1);
    assert_eq!(port_calls(&sink, GatePort::Resolve), vec!["transient"]);
}

#[tokio::test(start_paused = true)]
async fn the_breaker_reports_open_once_rejection_then_half_open_and_closed() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let cancel = CancellationToken::new();
    let run = RunBudget::new();
    for _ in 0..10 {
        drop(
            bounded(&budget, &run, GatePort::Resolve, &cancel, || async {
                Err::<(), _>(unavailable())
            })
            .await,
        );
    }
    assert_eq!(
        breaker_reports(&sink, GatePort::Resolve),
        vec![BreakerState::Open],
        "open reported exactly once"
    );
    let r = bounded(&budget, &run, GatePort::Resolve, &cancel, || async {
        Ok::<_, CanonicalError>(())
    })
    .await;
    assert!(matches!(r, Err(CallError::BreakerOpen)));
    assert_eq!(
        port_calls(&sink, GatePort::Resolve).last(),
        Some(&"breaker_open")
    );
    assert_eq!(port_calls(&sink, GatePort::Resolve).len(), 11);
    assert_eq!(
        port_calls_timed(&sink, GatePort::Resolve).last(),
        Some(&("breaker_open", false)),
        "a breaker_open exit never reached the callee: counted, not timed"
    );

    tokio::time::advance(Duration::from_secs(10)).await;
    let run2 = RunBudget::new();
    bounded(&budget, &run2, GatePort::Resolve, &cancel, || async {
        Ok::<_, CanonicalError>(())
    })
    .await
    .unwrap();
    assert_eq!(
        breaker_reports(&sink, GatePort::Resolve),
        vec![
            BreakerState::Open,
            BreakerState::HalfOpen,
            BreakerState::Closed,
        ]
    );
}

/// M3-2: a run's port deadline is cumulative, so concurrent calls that all hit it are one
/// failure for the breaker, not one per call; the metric still reports each.
#[tokio::test(start_paused = true)]
async fn only_the_first_deadline_per_run_per_port_counts_for_the_breaker() {
    let policy = BreakerPolicy {
        min_calls: 2,
        ..BreakerPolicy::BASELINE
    };
    let (budget, sink) = recorded(policy);
    let cancel = CancellationToken::new();
    let stall = || async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, CanonicalError>(())
    };
    let run = RunBudget::new();
    let (a, b) = tokio::join!(
        bounded(&budget, &run, GatePort::SkuRead, &cancel, stall),
        bounded(&budget, &run, GatePort::SkuRead, &cancel, stall),
    );
    assert!(matches!(a, Err(CallError::Deadline)), "{a:?}");
    assert!(matches!(b, Err(CallError::Deadline)), "{b:?}");
    assert_eq!(
        budget.breaker_state(GatePort::SkuRead, Instant::now()),
        Some(BreakerState::Closed),
        "two deadlines in one run are one failure, below min_calls"
    );
    assert_eq!(
        port_calls_timed(&sink, GatePort::SkuRead),
        vec![("deadline", true), ("deadline", true)]
    );

    // A deadline in each of two runs is two failures.
    let (budget, _sink) = recorded(policy);
    for _ in 0..2 {
        let run = RunBudget::new();
        let r = bounded(&budget, &run, GatePort::SkuRead, &cancel, stall).await;
        assert!(matches!(r, Err(CallError::Deadline)), "{r:?}");
    }
    assert_eq!(
        budget.breaker_state(GatePort::SkuRead, Instant::now()),
        Some(BreakerState::Open)
    );
}

#[tokio::test(start_paused = true)]
async fn an_exit_before_the_first_attempt_is_counted_but_not_timed() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = bounded(
        &budget,
        &RunBudget::new(),
        GatePort::Resolve,
        &cancel,
        || async { Ok::<_, CanonicalError>(()) },
    )
    .await;
    assert!(matches!(r, Err(CallError::Cancelled)));
    assert_eq!(
        port_calls_timed(&sink, GatePort::Resolve),
        vec![("cancelled", false)]
    );
}

/// A call that finds the run's port deadline already elapsed exits before any attempt: it is
/// reported `deadline`, untimed (unlike the admitted attempt that timed out first).
#[tokio::test(start_paused = true)]
async fn a_pre_admission_deadline_exit_is_reported_deadline_untimed() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    let first: Result<(), CallError> = bounded(&budget, &run, PORT, &cancel, || async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(())
    })
    .await;
    assert!(matches!(first, Err(CallError::Deadline)), "{first:?}");
    let spent = run.subrequests(PORT);
    let second: Result<(), CallError> =
        bounded(&budget, &run, PORT, &cancel, || async { Ok(()) }).await;
    assert!(matches!(second, Err(CallError::Deadline)), "{second:?}");
    assert_eq!(run.subrequests(PORT), spent, "no subrequest spent");
    assert_eq!(
        port_calls_timed(&sink, PORT),
        vec![("deadline", true), ("deadline", false)]
    );
}

/// The subrequest cap refusing a call's first attempt is an exit before the callee: untimed. A
/// cap hit on the retry is timed, since the first attempt reached the callee.
#[tokio::test(start_paused = true)]
async fn a_subrequest_cap_exit_is_untimed_on_the_first_attempt_and_timed_on_the_retry() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let cancel = CancellationToken::new();

    let run = RunBudget::new();
    for _ in 0..200 {
        bounded(&budget, &run, PORT, &cancel, || async {
            Ok::<_, CanonicalError>(())
        })
        .await
        .unwrap();
    }
    let capped: Result<(), CallError> =
        bounded(&budget, &run, PORT, &cancel, || async { Ok(()) }).await;
    assert!(
        matches!(capped, Err(CallError::SubrequestCapExceeded)),
        "{capped:?}"
    );
    assert_eq!(
        port_calls_timed(&sink, PORT).last(),
        Some(&("subrequest_cap", false))
    );

    // 199 subrequests spent: the transient first attempt takes the 200th, the retry is capped.
    let run = RunBudget::new();
    for _ in 0..199 {
        bounded(&budget, &run, PORT, &cancel, || async {
            Ok::<_, CanonicalError>(())
        })
        .await
        .unwrap();
    }
    let retried: Result<(), CallError> = bounded(&budget, &run, PORT, &cancel, || async {
        Err(unavailable())
    })
    .await;
    assert!(
        matches!(retried, Err(CallError::SubrequestCapExceeded)),
        "{retried:?}"
    );
    assert_eq!(
        port_calls_timed(&sink, PORT).last(),
        Some(&("subrequest_cap", true))
    );
}

/// Trip [`PORT`]'s baseline breaker with ten transient failures and wait out `open_for`, so the
/// next admission is the probe.
async fn trip_and_wait(budget: &Budget) {
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    for _ in 0..10 {
        let r = bounded(budget, &run, PORT, &cancel, || async {
            Err::<(), _>(unavailable())
        })
        .await;
        assert!(matches!(r, Err(CallError::Transient(_))), "{r:?}");
    }
    tokio::time::advance(BreakerPolicy::BASELINE.open_for).await;
}

/// After a released probe the breaker is open again: an immediate call is rejected, and after
/// another `open_for` a new probe is admitted (reported half-open) and its success closes it.
async fn assert_released_probe_reopened(budget: &Budget, sink: &RecordingGateSignals) {
    assert_eq!(
        breaker_reports(sink, PORT),
        vec![
            BreakerState::Open,
            BreakerState::HalfOpen,
            BreakerState::Open
        ]
    );
    let cancel = CancellationToken::new();
    let next: Result<(), CallError> = bounded(budget, &RunBudget::new(), PORT, &cancel, || async {
        Ok(())
    })
    .await;
    assert!(matches!(next, Err(CallError::BreakerOpen)), "{next:?}");
    tokio::time::advance(BreakerPolicy::BASELINE.open_for).await;
    bounded(budget, &RunBudget::new(), PORT, &cancel, || async {
        Ok::<_, CanonicalError>(())
    })
    .await
    .unwrap();
    assert_eq!(
        breaker_reports(sink, PORT),
        vec![
            BreakerState::Open,
            BreakerState::HalfOpen,
            BreakerState::Open,
            BreakerState::HalfOpen,
            BreakerState::Closed,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_probe_releases_the_slot_and_reopens() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    trip_and_wait(&budget).await;
    let parked = Notify::new();
    let cancel = CancellationToken::new();
    let run = RunBudget::new();
    let probe = bounded(&budget, &run, PORT, &cancel, || async {
        parked.notified().await;
        Ok::<_, CanonicalError>(())
    });
    let canceller = async {
        settle().await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(probe, canceller);
    assert!(matches!(result, Err(CallError::Cancelled)), "{result:?}");
    assert_released_probe_reopened(&budget, &sink).await;
}

#[tokio::test(start_paused = true)]
async fn a_dropped_probe_releases_the_slot_and_reopens() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    trip_and_wait(&budget).await;
    let parked = Notify::new();
    let cancel = CancellationToken::new();
    let run = RunBudget::new();
    // The timeout (50 ms) ends before the port's 250 ms run deadline: the `bounded` future is
    // dropped mid-call, not timed out by the discipline.
    let dropped = tokio::time::timeout(
        Duration::from_millis(50),
        bounded(&budget, &run, PORT, &cancel, || async {
            parked.notified().await;
            Ok::<_, CanonicalError>(())
        }),
    )
    .await;
    assert!(dropped.is_err(), "the probe must still be parked");
    assert_released_probe_reopened(&budget, &sink).await;
}

#[tokio::test(start_paused = true)]
async fn the_retry_waits_a_bounded_jittered_pause() {
    let budget = budget_with(250);
    let deadline = Duration::from_millis(250);
    let start = Instant::now();
    // Each attempt's start, measured from the call's start.
    let starts = parking_lot::Mutex::new(Vec::new());
    let result = bounded(
        &budget,
        &RunBudget::new(),
        PORT,
        &CancellationToken::new(),
        || {
            let n = {
                let mut starts = starts.lock();
                starts.push(start.elapsed());
                starts.len()
            };
            async move {
                if n == 1 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Err(unavailable())
                } else {
                    Ok(1_u8)
                }
            }
        },
    )
    .await;
    assert_eq!(result.unwrap(), 1);
    let starts = starts.into_inner();
    assert_eq!(starts.len(), 2, "{starts:?}");
    let pause = starts[1].saturating_sub(Duration::from_millis(5));
    assert!(pause > Duration::ZERO, "the retry waits: {starts:?}");
    assert!(pause <= RETRY_PAUSE, "the pause is bounded: {starts:?}");
    assert!(starts[1] < deadline, "{starts:?}");
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_the_retry_pause_is_cancelled() {
    let (budget, sink) = recorded(BreakerPolicy::BASELINE);
    let run = RunBudget::new();
    let cancel = CancellationToken::new();
    let calls = AtomicU32::new(0);
    let start = Instant::now();
    // The first attempt fails transiently and the caller cancels as it answers: the pause
    // ends at once, the retry is never taken and spends no subrequest.
    let result: Result<(), CallError> = bounded(&budget, &run, PORT, &cancel, || async {
        calls.fetch_add(1, Ordering::SeqCst);
        cancel.cancel();
        Err(unavailable())
    })
    .await;
    assert!(matches!(result, Err(CallError::Cancelled)), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.subrequests(PORT), 1);
    assert_eq!(retries(&sink, PORT), 0);
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert_eq!(port_calls_timed(&sink, PORT), vec![("cancelled", true)]);
}

#[tokio::test(start_paused = true)]
async fn a_pause_never_runs_past_the_port_deadline() {
    let budget = budget_with(10);
    let run = RunBudget::new();
    let calls = AtomicU32::new(0);
    let start = Instant::now();
    let result: Result<u8, CallError> =
        bounded(&budget, &run, PORT, &CancellationToken::new(), || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(9)).await;
                Err(unavailable())
            } else {
                Ok(1)
            }
        })
        .await;
    // 1 ms remains when the first attempt fails: the pause is cut at the deadline, and a retry
    // with no time left is not taken. Tokio timers resolve to whole milliseconds, so even a
    // sub-millisecond jittered pause wakes at the 10 ms deadline here.
    assert!(matches!(result, Err(CallError::Transient(_))), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.subrequests(PORT), 1);
    assert_eq!(start.elapsed(), Duration::from_millis(10));
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn acceptance_transient_failure_retries_once_and_logs() {
    let budget = AcceptanceBudget::with_deadline(ACCEPTANCE_DEADLINE);
    let calls = AtomicU32::new(0);
    let line_id = uuid::Uuid::from_u128(0x11);
    let result = budget
        .bounded(
            line_id,
            "line-key-11",
            &CancellationToken::new(),
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(unavailable())
                } else {
                    Ok(4_u8)
                }
            },
        )
        .await;
    assert_eq!(result.unwrap(), 4);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(logs_contain("retrying a transient acceptance command"));
    assert!(logs_contain("attempt=2"));
    assert!(logs_contain(&line_id.to_string()));
    assert!(logs_contain("line-key-11"));
}

/// A 1-based attempt counter for the acceptance tests: `answer(n)` is the `n`th attempt's result.
fn counted<T>(
    calls: &AtomicU32,
    answer: impl Fn(u32) -> Result<T, CanonicalError>,
) -> impl Fn() -> std::future::Ready<Result<T, CanonicalError>> {
    move || std::future::ready(answer(calls.fetch_add(1, Ordering::SeqCst) + 1))
}

const LINE: uuid::Uuid = uuid::Uuid::from_u128(0x11);

#[tokio::test(start_paused = true)]
async fn acceptance_deadline_fires_at_the_configured_value() {
    let budget = AcceptanceBudget::with_deadline(Duration::from_millis(120));
    let start = Instant::now();
    let result: Result<(), CallError> = budget
        .bounded(LINE, "k", &CancellationToken::new(), || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Deadline)), "{result:?}");
    assert_eq!(start.elapsed(), Duration::from_millis(120));
}

#[tokio::test(start_paused = true)]
async fn acceptance_transient_retries_once_then_succeeds() {
    let budget = AcceptanceBudget::with_deadline(ACCEPTANCE_DEADLINE);
    let start = Instant::now();
    let starts = parking_lot::Mutex::new(Vec::new());
    let result = budget
        .bounded(LINE, "k", &CancellationToken::new(), || {
            let n = {
                let mut starts = starts.lock();
                starts.push(start.elapsed());
                starts.len()
            };
            async move { if n == 1 { Err(unavailable()) } else { Ok(9_u8) } }
        })
        .await;
    assert_eq!(result.unwrap(), 9);
    let starts = starts.into_inner();
    assert_eq!(starts.len(), 2, "{starts:?}");
    // The retry waits the same bounded jittered pause as a gate port (D-244).
    assert!(starts[1] > Duration::ZERO, "{starts:?}");
    assert!(starts[1] <= RETRY_PAUSE, "{starts:?}");
}

#[tokio::test(start_paused = true)]
async fn acceptance_second_transient_is_not_retried() {
    let budget = AcceptanceBudget::with_deadline(ACCEPTANCE_DEADLINE);
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> = budget
        .bounded(
            LINE,
            "k",
            &CancellationToken::new(),
            counted(&calls, |_| Err(unavailable())),
        )
        .await;
    assert!(matches!(result, Err(CallError::Transient(_))), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn acceptance_permanent_and_business_refusal_never_retry() {
    let budget = AcceptanceBudget::with_deadline(ACCEPTANCE_DEADLINE);
    let refusal = || {
        TestError::aborted("not sellable")
            .with_reason("PLAN_NOT_SELLABLE")
            .create()
    };
    for answer in [invalid_argument as fn() -> CanonicalError, refusal] {
        let calls = AtomicU32::new(0);
        let result: Result<(), CallError> = budget
            .bounded(
                LINE,
                "k",
                &CancellationToken::new(),
                counted(&calls, |_| Err(answer())),
            )
            .await;
        assert!(matches!(result, Err(CallError::Permanent(_))), "{result:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn acceptance_past_port_bulkhead_is_backpressure_without_queueing() {
    let budget = AcceptanceBudget::with_deadline(Duration::from_secs(60));
    let release = CancellationToken::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let parked: Vec<_> = (0..PORT_BULKHEAD)
        .map(|_| {
            let (budget, release, entered) =
                (budget.clone(), release.clone(), Arc::clone(&entered));
            tokio::spawn(async move {
                drop(
                    budget
                        .bounded(LINE, "k", &CancellationToken::new(), || async {
                            entered.fetch_add(1, Ordering::SeqCst);
                            release.cancelled().await;
                            Ok::<_, CanonicalError>(())
                        })
                        .await,
                );
            })
        })
        .collect();
    settle().await;
    assert_eq!(entered.load(Ordering::SeqCst), PORT_BULKHEAD);
    let start = Instant::now();
    let calls = AtomicU32::new(0);
    let result: Result<(), CallError> = budget
        .bounded(
            LINE,
            "k",
            &CancellationToken::new(),
            counted(&calls, |_| Ok(())),
        )
        .await;
    assert!(matches!(result, Err(CallError::Backpressure)), "{result:?}");
    assert_eq!(start.elapsed(), Duration::ZERO, "backpressure never queues");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    release.cancel();
    for handle in parked {
        handle.await.unwrap();
    }
    // The permits are released with their calls.
    budget
        .bounded(
            LINE,
            "k",
            &CancellationToken::new(),
            counted(&calls, |_| Ok(())),
        )
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn acceptance_cancel_before_and_during() {
    let budget = AcceptanceBudget::with_deadline(Duration::from_secs(60));
    let calls = AtomicU32::new(0);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result: Result<(), CallError> = budget
        .bounded(LINE, "k", &cancelled, counted(&calls, |_| Ok(())))
        .await;
    assert!(matches!(result, Err(CallError::Cancelled)), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        trigger.cancel();
    });
    let start = Instant::now();
    let result: Result<(), CallError> = budget
        .bounded(LINE, "k", &cancel, || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(CallError::Cancelled)), "{result:?}");
    assert_eq!(start.elapsed(), Duration::from_millis(5));
}

#[test]
fn acceptance_from_config_default_is_500ms_and_override_wins() {
    assert_eq!(ACCEPTANCE_DEADLINE, Duration::from_millis(500));
    assert_eq!(
        AcceptanceBudget::from_config(&GateConfig::default()).deadline(),
        ACCEPTANCE_DEADLINE
    );
    let config = GateConfig {
        acceptance_deadline_ms: Some(120),
        ..GateConfig::default()
    };
    config.validate().unwrap();
    assert_eq!(
        AcceptanceBudget::from_config(&config).deadline(),
        Duration::from_millis(120)
    );
    for ms in [0, 60_001] {
        let config = GateConfig {
            acceptance_deadline_ms: Some(ms),
            ..GateConfig::default()
        };
        assert!(config.validate().is_err(), "acceptance {ms}");
    }
}

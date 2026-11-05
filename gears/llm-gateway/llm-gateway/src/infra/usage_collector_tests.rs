//! Usage Reporter adapter: record mapping, buffering and batching, retry
//! classification, fail-open overflow and drain on shutdown (DESIGN §3.2
//! Usage Reporter, `seq-usage-delivery`, §4.5 `usage.*` / `shutdown.*`).
//!
//! Delivery tests run on a paused tokio clock: waits advance virtual time, so
//! backoff and drain deadlines are asserted exactly and cost nothing.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rust_decimal::Decimal;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use usage_collector_sdk::{UsageCollectorError, UsageTypeGtsId};
use uuid::Uuid;

use super::{RESOURCE_TYPE, UsageCollectorReporter, build_record};
use crate::config::{ShutdownConfig, UsageConfig};
use crate::domain::ports::{UsageProducers, UsageReport, UsageSink};
use crate::domain::test_support::{
    FakeIdentity, FakeUsageCollector, SERVICE_SUBJECT, UcScript, uc_errors,
};

const USAGE_TYPE: &str = "gts.cf.core.uc.usage_record.v1~cf.llmgw.gateway.ai_credits.v1";
const BACKOFF: Duration = Duration::from_secs(1);
const RESERVE: Duration = Duration::from_secs(5);
const TENANT: Uuid = Uuid::from_u128(0xA);
const USER: Uuid = Uuid::from_u128(0xB);

/// Producers that are quiescent from the start (nothing in flight).
struct Idle;

#[async_trait::async_trait]
impl UsageProducers for Idle {
    async fn quiescent(&self) {}
}

/// Producers that become quiescent when `release` is called; until then
/// they are busy (a request in flight).
struct Gate(tokio::sync::watch::Sender<bool>);

impl Gate {
    fn closed() -> Arc<Self> {
        Arc::new(Self(tokio::sync::watch::channel(false).0))
    }

    fn release(&self) {
        self.0.send_replace(true);
    }
}

#[async_trait::async_trait]
impl UsageProducers for Gate {
    async fn quiescent(&self) {
        let mut open = self.0.subscribe();
        drop(open.wait_for(|open| *open).await);
    }
}

fn usage_type() -> UsageTypeGtsId {
    UsageTypeGtsId::new(USAGE_TYPE).expect("valid usage type")
}

/// Report of request `n`: id `resp_<n>`, cost 0.0004 credits = 400 micro.
fn report(n: u64) -> UsageReport {
    UsageReport {
        response_id: format!("resp_{n}"),
        tenant_id: TENANT,
        user_id: USER,
        model: "openai::gpt-4o".to_owned(),
        provider_type: "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~".to_owned(),
        usage: llm_gateway_sdk::Usage::reported(100, 50, Decimal::new(4, 4)),
        quantity_micro_credits: 400,
        terminal_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n),
        provider_request_id: Some(format!("req_{n}")),
    }
}

// ── record mapping ──────────────────────────────────────────────────────────

mod mapping {
    use super::*;

    fn meta(record: &usage_collector_sdk::CreateUsageRecord, key: &str) -> Option<String> {
        record
            .metadata
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn a_report_maps_to_the_usage_record_of_the_design() {
        let record = build_record(&report(7), &usage_type()).expect("valid record");

        assert_eq!(record.gts_id, usage_type());
        assert_eq!(record.tenant_id, TENANT, "tenant is explicit on the record");
        assert_eq!(record.resource_ref.resource_type(), "llm_gateway.response");
        assert_eq!(record.resource_ref.resource_type(), RESOURCE_TYPE);
        assert_eq!(record.resource_ref.resource_id(), "resp_7");
        let subject = record.subject_ref.as_ref().expect("user subject");
        assert_eq!(subject.subject_id(), USER.to_string());
        assert_eq!(record.value, Decimal::from(400));
        assert_eq!(record.idempotency_key.as_str(), "resp_7");
        assert!(record.corrects_id.is_none());
        assert_eq!(meta(&record, "model").as_deref(), Some("openai::gpt-4o"));
        assert_eq!(
            meta(&record, "provider_type").as_deref(),
            Some("gts.cf.genai.model.info.v1~cf.genai._.openai.v1~")
        );
        assert_eq!(
            meta(&record, "provider_request_id").as_deref(),
            Some("req_7")
        );
        assert_eq!(meta(&record, "complete").as_deref(), Some("true"));
        assert_eq!(meta(&record, "estimate").as_deref(), Some("false"));
        assert_eq!(record.metadata.len(), 5, "only the declared metadata keys");
    }

    #[test]
    fn the_value_is_the_micro_credit_quantity_exactly() {
        let mut r = report(1);
        r.quantity_micro_credits = i64::MAX;
        let record = build_record(&r, &usage_type()).expect("valid record");
        assert_eq!(record.value, Decimal::from(i64::MAX));
    }

    #[test]
    fn a_zero_quantity_is_reported_as_zero() {
        let mut r = report(1);
        r.quantity_micro_credits = 0;
        let record = build_record(&r, &usage_type()).expect("valid record");
        assert_eq!(record.value, Decimal::ZERO);
    }

    #[test]
    fn an_incomplete_estimated_usage_sets_the_flags() {
        let mut r = report(1);
        r.usage = llm_gateway_sdk::Usage::estimated(10, 5, Decimal::new(1, 4));
        let record = build_record(&r, &usage_type()).expect("valid record");
        assert_eq!(meta(&record, "complete").as_deref(), Some("false"));
        assert_eq!(meta(&record, "estimate").as_deref(), Some("true"));
    }

    #[test]
    fn a_missing_provider_request_id_omits_the_key() {
        let mut r = report(1);
        r.provider_request_id = None;
        let record = build_record(&r, &usage_type()).expect("valid record");
        assert_eq!(meta(&record, "provider_request_id"), None);
        assert_eq!(record.metadata.len(), 4);
    }

    #[test]
    fn created_at_is_the_terminal_time_and_the_same_on_every_mapping() {
        let r = report(3);
        let first = build_record(&r, &usage_type()).expect("valid record");
        let second = build_record(&r, &usage_type()).expect("valid record");
        assert_eq!(first.created_at, second.created_at);
        assert_eq!(first.created_at, time::OffsetDateTime::from(r.terminal_at));
    }

    #[test]
    fn an_empty_response_id_is_rejected_by_the_record_validation() {
        let mut r = report(1);
        r.response_id = String::new();
        let err = build_record(&r, &usage_type()).expect_err("empty id is invalid");
        assert!(matches!(err, UsageCollectorError::InvalidArgument { .. }));
    }
}

// ── construction ────────────────────────────────────────────────────────────

mod construction {
    use super::*;

    #[test]
    fn the_default_usage_type_is_accepted() {
        let reporter = UsageCollectorReporter::new(
            FakeUsageCollector::new(),
            FakeIdentity::new(),
            &UsageConfig::default(),
            &ShutdownConfig::default(),
        );
        assert!(reporter.is_ok());
    }

    #[test]
    fn a_usage_type_outside_the_usage_record_base_is_rejected() {
        let cfg = UsageConfig {
            usage_type: "gts.cf.core.uc.usage_type.v1~cf.llmgw.gateway.ai_credits.v1".to_owned(),
            ..UsageConfig::default()
        };
        let reporter = UsageCollectorReporter::new(
            FakeUsageCollector::new(),
            FakeIdentity::new(),
            &cfg,
            &ShutdownConfig::default(),
        );
        assert!(reporter.is_err());
    }
}

// ── delivery ────────────────────────────────────────────────────────────────

fn cfg(capacity: usize, batch: usize) -> UsageConfig {
    UsageConfig {
        buffer_capacity: capacity,
        batch_size: batch,
        flush_interval: Duration::from_secs(10),
        retry_backoff: BACKOFF,
        ..UsageConfig::default()
    }
}

struct Rig {
    reporter: Arc<UsageCollectorReporter>,
    uc: Arc<FakeUsageCollector>,
    identity: Arc<FakeIdentity>,
    cancel: CancellationToken,
    t0: Instant,
}

impl Rig {
    fn new(cfg: &UsageConfig) -> Self {
        Self::with_identity(cfg, FakeIdentity::new())
    }

    fn with_identity(cfg: &UsageConfig, identity: Arc<FakeIdentity>) -> Self {
        let uc = FakeUsageCollector::new();
        let reporter = UsageCollectorReporter::new(
            uc.clone(),
            identity.clone(),
            cfg,
            &ShutdownConfig {
                usage_drain_reserve: RESERVE,
            },
        )
        .expect("reporter")
        // Deterministic backoff; jitter has its own test.
        .with_backoff_jitter(0.0);
        Self {
            reporter: Arc::new(reporter),
            uc,
            identity,
            cancel: CancellationToken::new(),
            t0: Instant::now(),
        }
    }

    /// Start the worker with nothing in flight.
    fn start(&self) -> JoinHandle<()> {
        self.start_with(Arc::new(Idle))
    }

    /// Start the worker over `producers`.
    fn start_with(&self, producers: Arc<dyn UsageProducers>) -> JoinHandle<()> {
        let reporter = Arc::clone(&self.reporter);
        let cancel = self.cancel.clone();
        tokio::spawn(async move { reporter.run(cancel, producers).await })
    }

    async fn push(&self, ids: impl IntoIterator<Item = u64>) {
        for n in ids {
            self.reporter.record(report(n)).await;
        }
    }

    fn after_t0(&self, at: Instant) -> Duration {
        at - self.t0
    }

    fn keys(&self) -> Vec<String> {
        self.uc.submitted_keys()
    }
}

/// Let the worker run (advances the paused clock a little).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn approx(actual: Duration, expected: Duration) -> bool {
    actual.abs_diff(expected) <= Duration::from_millis(5)
}

mod batching {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn record_only_buffers_and_nothing_is_sent_before_the_worker_runs() {
        let rig = Rig::new(&cfg(10, 3));
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_mins(1)).await;
        assert!(rig.uc.calls().is_empty());
        assert_eq!(rig.reporter.pending(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_batch_is_sent_at_once_without_waiting_for_the_flush_interval() {
        let rig = Rig::new(&cfg(10, 3));
        let _worker = rig.start();
        rig.push(0..3).await;
        settle().await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].records.len(), 3);
        assert!(rig.after_t0(calls[0].at) < Duration::from_secs(1));
        assert_eq!(rig.keys(), ["resp_0", "resp_1", "resp_2"]);
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_partial_batch_waits_for_the_flush_interval_and_no_longer() {
        let rig = Rig::new(&cfg(10, 3));
        let _worker = rig.start();
        rig.push(0..2).await;

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(rig.uc.calls().is_empty(), "not flushed before the interval");

        tokio::time::sleep(Duration::from_secs(10)).await;
        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].records.len(), 2);
        assert!(
            rig.after_t0(calls[0].at) <= Duration::from_millis(10_050),
            "flushed within flush_interval: {:?}",
            rig.after_t0(calls[0].at)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn more_records_than_the_batch_size_go_out_in_order_in_several_calls() {
        let rig = Rig::new(&cfg(10, 3));
        let _worker = rig.start();
        rig.push(0..7).await;
        tokio::time::sleep(Duration::from_secs(11)).await;

        let sizes: Vec<usize> = rig.uc.calls().iter().map(|c| c.records.len()).collect();
        assert!(sizes.iter().all(|n| (1..=3).contains(n)), "{sizes:?}");
        assert_eq!(
            rig.keys(),
            (0..7).map(|n| format!("resp_{n}")).collect::<Vec<_>>()
        );
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_runs_under_the_service_identity_with_explicit_tenants() {
        let rig = Rig::new(&cfg(10, 3));
        let _worker = rig.start();
        let other_tenant = Uuid::from_u128(0xC);
        let mut second = report(1);
        second.tenant_id = other_tenant;
        rig.reporter.record(report(0)).await;
        rig.reporter.record(second).await;
        rig.reporter.record(report(2)).await;
        settle().await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].subject, Uuid::from_u128(SERVICE_SUBJECT));
        assert!(rig.identity.calls() >= 1);
        let tenants: Vec<Uuid> = calls[0].records.iter().map(|r| r.tenant_id).collect();
        assert_eq!(tenants, [TENANT, other_tenant, TENANT]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idempotent_replay_counts_as_success_and_is_not_resent() {
        // The collector answers Ok (identical replay) for a record it has
        // seen: nothing is retried.
        let rig = Rig::new(&cfg(10, 2));
        let _worker = rig.start();
        rig.push(0..1).await;
        rig.push(0..1).await; // same response id twice: a replay
        settle().await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(rig.keys(), ["resp_0", "resp_0"]);
        assert_eq!(rig.uc.calls().len(), 1);
    }
}

mod retries {
    use super::*;

    fn unavailable_times(n: usize) -> Vec<UcScript> {
        (0..n).map(|_| Err(uc_errors::unavailable())).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_level_unavailable_retries_the_whole_batch_after_the_backoff() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(unavailable_times(2));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(10)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 3, "two failures, then the delivery");
        for call in &calls {
            let keys: Vec<&str> = call
                .records
                .iter()
                .map(|r| r.idempotency_key.as_str())
                .collect();
            assert_eq!(keys, ["resp_0", "resp_1"], "the whole batch is retried");
        }
        assert!(
            approx(calls[1].at - calls[0].at, BACKOFF),
            "{:?}",
            calls[1].at - calls[0].at
        );
        assert!(approx(calls[2].at - calls[1].at, BACKOFF));
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn created_at_and_the_idempotency_key_are_identical_on_every_retry() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(unavailable_times(3));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(10)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 4);
        let first = &calls[0].records;
        for call in &calls[1..] {
            for (a, b) in first.iter().zip(&call.records) {
                assert_eq!(a.created_at, b.created_at, "created_at is stamped once");
                assert_eq!(a.idempotency_key, b.idempotency_key);
                assert_eq!(a, b, "the retried record is identical");
            }
        }
        // created_at is the terminal time of the report, not the send time.
        assert_eq!(
            first[0].created_at,
            time::OffsetDateTime::from(report(0).terminal_at)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_backoff_is_jittered_within_twenty_percent_by_default() {
        let uc = FakeUsageCollector::new();
        uc.script(unavailable_times(6));
        let reporter = Arc::new(
            UsageCollectorReporter::new(
                uc.clone(),
                FakeIdentity::new(),
                &cfg(10, 1),
                &ShutdownConfig::default(),
            )
            .expect("reporter"),
        );
        let cancel = CancellationToken::new();
        let worker = {
            let reporter = Arc::clone(&reporter);
            let cancel = cancel.clone();
            tokio::spawn(async move { reporter.run(cancel, Arc::new(Idle)).await })
        };
        reporter.record(report(0)).await;
        tokio::time::sleep(Duration::from_secs(20)).await;

        let calls = uc.calls();
        assert_eq!(calls.len(), 7);
        let gaps: Vec<Duration> = calls.windows(2).map(|w| w[1].at - w[0].at).collect();
        assert!(
            gaps.iter()
                .all(|g| *g >= Duration::from_millis(800) && *g <= Duration::from_millis(1200)),
            "{gaps:?}"
        );
        assert!(gaps.windows(2).any(|w| w[0] != w[1]), "{gaps:?}");
        cancel.cancel();
        worker.await.expect("worker");
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_are_retried_until_delivery_with_no_attempt_limit() {
        let rig = Rig::new(&cfg(10, 1));
        rig.uc.script(unavailable_times(40));
        let _worker = rig.start();
        rig.push(0..1).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        assert_eq!(rig.uc.calls().len(), 41);
        assert_eq!(rig.reporter.pending(), 0, "delivered at last");
    }

    fn internal_times(n: usize) -> Vec<UcScript> {
        (0..n).map(|_| Err(uc_errors::internal())).collect()
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_batch_level_internal_is_retried_with_backoff_up_to_the_limit_then_dropped() {
        // Default limit: 3 retries, i.e. 4 attempts in total.
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(internal_times(10));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 4, "first attempt plus max_internal_retries");
        for pair in calls.windows(2) {
            assert!(
                pair[1].at - pair[0].at >= BACKOFF,
                "backoff between attempts"
            );
        }
        assert_eq!(rig.reporter.pending(), 0, "dropped after the last retry");
        assert!(logs_contain("resp_0") && logs_contain("resp_1"));
    }

    #[tokio::test(start_paused = true)]
    async fn internal_retries_resend_identical_records() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(internal_times(10));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 4);
        for call in &calls[1..] {
            assert_eq!(call.records, calls[0].records, "same created_at and key");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_internal_that_clears_within_the_limit_is_delivered() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(internal_times(3));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        assert_eq!(
            rig.uc.calls().len(),
            4,
            "three Internal answers, then success"
        );
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_per_record_internal_requeues_only_that_record_up_to_the_limit_then_drops_it() {
        let rig = Rig::new(&cfg(10, 3));
        let per_record = || Ok(vec![Ok(()), Err(uc_errors::internal()), Ok(())]);
        rig.uc.script([
            per_record(),
            Ok(vec![Err(uc_errors::internal())]),
            Ok(vec![Err(uc_errors::internal())]),
            Ok(vec![Err(uc_errors::internal())]),
        ]);
        let _worker = rig.start();
        rig.push(0..3).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 4, "1 attempt + 3 retries of resp_1");
        for call in &calls[1..] {
            let keys: Vec<&str> = call
                .records
                .iter()
                .map(|r| r.idempotency_key.as_str())
                .collect();
            assert_eq!(keys, ["resp_1"]);
        }
        assert_eq!(rig.reporter.pending(), 0);
        assert!(logs_contain("resp_1"), "the dropped record is logged by id");
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn zero_max_internal_retries_drops_on_the_first_internal() {
        let usage = UsageConfig {
            max_internal_retries: 0,
            ..cfg(10, 2)
        };
        let rig = Rig::new(&usage);
        rig.uc.script(internal_times(10));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(30)).await;

        assert_eq!(rig.uc.calls().len(), 1);
        assert_eq!(rig.reporter.pending(), 0);
        assert!(logs_contain("resp_0") && logs_contain("resp_1"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_internal_drop_does_not_hold_back_the_next_batch() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script(internal_times(4));
        let _worker = rig.start();
        rig.push(0..4).await;
        tokio::time::sleep(Duration::from_secs(60)).await;

        let keys = rig.keys();
        assert_eq!(&keys[keys.len() - 2..], ["resp_2", "resp_3"]);
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn no_failure_class_refreshes_the_service_identity() {
        let rig = Rig::new(&cfg(10, 1));
        rig.uc.script([
            Err(uc_errors::unavailable()),
            Err(uc_errors::internal()),
            Err(uc_errors::permission_denied()),
        ]);
        let _worker = rig.start();
        rig.push(0..3).await;
        tokio::time::sleep(Duration::from_secs(30)).await;

        assert_eq!(rig.identity.invalidations(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_per_record_transient_error_requeues_only_that_record() {
        let rig = Rig::new(&cfg(10, 3));
        rig.uc
            .script([Ok(vec![Ok(()), Err(uc_errors::unavailable()), Ok(())])]);
        let _worker = rig.start();
        rig.push(0..3).await;
        tokio::time::sleep(Duration::from_secs(5)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].records.len(), 3);
        let second: Vec<&str> = calls[1]
            .records
            .iter()
            .map(|r| r.idempotency_key.as_str())
            .collect();
        assert_eq!(second, ["resp_1"], "accepted records are not resent");
        assert_eq!(
            calls[1].records[0].created_at,
            calls[0].records[1].created_at
        );
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn per_record_permanent_errors_are_logged_and_dropped_without_retry() {
        let rig = Rig::new(&cfg(10, 4));
        rig.uc.script([Ok(vec![
            Err(uc_errors::invalid_argument()),
            Err(uc_errors::conflict()),
            Err(uc_errors::permission_denied()),
            Ok(()),
        ])]);
        let _worker = rig.start();
        rig.push(0..4).await;
        tokio::time::sleep(Duration::from_secs(30)).await;

        assert_eq!(
            rig.uc.calls().len(),
            1,
            "permanent errors are never retried"
        );
        assert_eq!(rig.reporter.pending(), 0);
        for id in ["resp_0", "resp_1", "resp_2"] {
            assert!(logs_contain(id), "{id} is logged with its response id");
        }
        assert_eq!(
            rig.identity.invalidations(),
            0,
            "a permission denial does not refresh the identity"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_permanent_record_error_does_not_hold_back_the_next_batch() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc
            .script([Ok(vec![Err(uc_errors::invalid_argument()), Ok(())])]);
        let _worker = rig.start();
        rig.push(0..4).await;
        tokio::time::sleep(Duration::from_secs(15)).await;

        assert_eq!(rig.keys(), ["resp_0", "resp_1", "resp_2", "resp_3"]);
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_batch_level_permanent_error_drops_that_batch_and_keeps_going() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.script([Err(uc_errors::permission_denied())]);
        let _worker = rig.start();
        rig.push(0..4).await;
        tokio::time::sleep(Duration::from_secs(15)).await;

        // First batch (resp_0, resp_1) refused for good; the next one goes.
        assert_eq!(rig.keys(), ["resp_0", "resp_1", "resp_2", "resp_3"]);
        assert_eq!(rig.uc.calls().len(), 2);
        assert!(logs_contain("resp_0") && logs_contain("resp_1"));
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_service_identity_keeps_the_records_and_retries_with_backoff() {
        let rig = Rig::with_identity(&cfg(10, 2), FakeIdentity::failing_first(2));
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(15)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 1, "no collector call without a context");
        assert_eq!(rig.identity.calls(), 3, "two refused exchanges, then one");
        assert!(
            rig.after_t0(calls[0].at) >= BACKOFF * 2,
            "backoff between exchange attempts: {:?}",
            rig.after_t0(calls[0].at)
        );
        assert_eq!(rig.keys(), ["resp_0", "resp_1"]);
    }
}

mod overflow {
    use super::*;

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_full_buffer_drops_the_new_record_counts_it_and_logs_its_id() {
        let rig = Rig::new(&cfg(3, 3));
        rig.push(0..5).await;

        assert_eq!(rig.reporter.pending(), 3);
        assert_eq!(rig.reporter.dropped_total(), 2);
        assert!(logs_contain("resp_3") && logs_contain("resp_4"));

        let _worker = rig.start();
        settle().await;
        assert_eq!(
            rig.keys(),
            ["resp_0", "resp_1", "resp_2"],
            "oldest are kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn record_returns_immediately_on_a_full_buffer() {
        let rig = Rig::new(&cfg(1, 1));
        rig.push(0..1).await;
        let outcome =
            tokio::time::timeout(Duration::from_millis(1), rig.reporter.record(report(1))).await;
        assert!(outcome.is_ok(), "reporting never blocks the request");
        assert_eq!(rig.reporter.dropped_total(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_collector_fills_the_buffer_and_then_new_records_are_dropped() {
        // Capacity bounds queued plus in-flight records.
        let rig = Rig::new(&cfg(2, 2));
        rig.uc.park();
        let _worker = rig.start();
        rig.push(0..2).await;
        settle().await;
        assert_eq!(rig.uc.calls().len(), 1, "the batch is in flight");

        rig.push(2..4).await;
        assert_eq!(rig.reporter.dropped_total(), 2);
        assert_eq!(rig.reporter.pending(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_buffer_accepts_records_again_once_it_has_drained() {
        let rig = Rig::new(&cfg(2, 2));
        let _worker = rig.start();
        rig.push(0..3).await;
        settle().await;
        rig.push(10..12).await;
        settle().await;

        assert_eq!(rig.reporter.dropped_total(), 1);
        assert_eq!(rig.keys(), ["resp_0", "resp_1", "resp_10", "resp_11"]);
    }
}

mod shutdown {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_worker_keeps_running_until_cancelled() {
        let rig = Rig::new(&cfg(10, 3));
        let worker = rig.start();
        tokio::time::sleep(Duration::from_mins(5)).await;
        assert!(!worker.is_finished());
        rig.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("stops promptly with an empty buffer")
            .expect("worker");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_flushes_queued_records_at_once_without_the_flush_interval() {
        let rig = Rig::new(&cfg(10, 3));
        let worker = rig.start();
        rig.push(0..2).await;
        settle().await;
        assert!(rig.uc.calls().is_empty(), "partial batch still waiting");

        let cancelled = Instant::now();
        rig.cancel.cancel();
        worker.await.expect("worker");

        assert_eq!(rig.keys(), ["resp_0", "resp_1"]);
        assert!(cancelled.elapsed() < Duration::from_secs(1));
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn an_unavailable_collector_is_retried_until_the_drain_reserve_then_the_rest_is_logged() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc
            .script((0..100).map(|_| Err(uc_errors::unavailable())));
        let worker = rig.start();
        rig.push(0..2).await;
        settle().await;

        let cancelled = Instant::now();
        let calls_before = rig.uc.calls().len();
        rig.cancel.cancel();
        worker.await.expect("worker");

        let drained_for = cancelled.elapsed();
        assert!(
            drained_for >= RESERVE && drained_for <= RESERVE + BACKOFF,
            "drain lasts the reserve: {drained_for:?}"
        );
        assert!(
            rig.uc.calls().len() >= calls_before + 3,
            "delivery keeps being retried during the drain"
        );
        assert!(logs_contain("undelivered"));
        assert!(logs_contain("resp_0") && logs_contain("resp_1"));
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_hanging_collector_cannot_hold_the_shutdown_past_the_reserve() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.park();
        let worker = rig.start();
        rig.push(0..2).await;
        settle().await;

        let cancelled = Instant::now();
        rig.cancel.cancel();
        tokio::time::timeout(RESERVE + Duration::from_secs(2), worker)
            .await
            .expect("run returns by the drain deadline")
            .expect("worker");

        let drained_for = cancelled.elapsed();
        assert!(drained_for >= RESERVE, "{drained_for:?}");
        assert!(logs_contain("undelivered"));
        assert!(logs_contain("resp_0"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_collector_that_recovers_within_the_reserve_gets_everything() {
        let rig = Rig::new(&cfg(10, 5));
        rig.uc.script((0..2).map(|_| Err(uc_errors::unavailable())));
        let worker = rig.start();
        rig.push(0..5).await;
        settle().await;

        rig.cancel.cancel();
        worker.await.expect("worker");

        assert_eq!(rig.reporter.pending(), 0);
        assert_eq!(rig.uc.calls().len(), 3);
        assert_eq!(rig.keys().len(), 15, "three attempts of five records");
    }

    // ── quiescence before the drain (S5B-001) ───────────────────────────

    use crate::domain::test_support::{
        FakePlugin, Harness, body, ctx, qe_errors, sample_response, set_cost,
    };

    const MODEL: &str = "openai::gpt-4o";

    /// Priced harness whose plugin parks every call, and a service whose
    /// quota guard hands usage to the rig's reporter.
    fn gated_service(rig: &Rig) -> (Harness, Arc<FakePlugin>, Arc<crate::domain::Service>) {
        let mut h = Harness::new();
        h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
        let plugin = FakePlugin::with(
            |c| Ok(sample_response(&c.request_id, "gpt-4o")),
            h.log.clone(),
            true,
        );
        h.set_plugin(plugin.clone());
        let service = h.service_with_quota(h.quota_guard_with(rig.reporter.clone()));
        (h, plugin, service)
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_in_flight_at_cancel_completes_settles_and_its_usage_is_delivered() {
        let rig = Rig::new(&cfg(10, 5));
        let (h, plugin, service) = gated_service(&rig);
        let worker = rig.start_with(service.clone());
        let request = {
            let service = service.clone();
            tokio::spawn(async move { service.create_response(&ctx(), body(MODEL)).await })
        };
        tokio::time::timeout(Duration::from_secs(5), plugin.entered.notified())
            .await
            .expect("the request reached the plugin");

        rig.cancel.cancel();
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !worker.is_finished(),
            "the reporter waits for the request in flight"
        );
        assert!(rig.keys().is_empty());

        plugin.release.notify_one();
        let response = request.await.expect("request task").expect("response");
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("the reporter finishes once the request is delivered")
            .expect("worker");

        assert_eq!(h.quota.commits().len(), 1, "settled after the cancel");
        assert_eq!(rig.keys(), std::slice::from_ref(&response.id));
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_settle_still_running_at_cancel_is_awaited_and_its_usage_delivered() {
        let rig = Rig::new(&cfg(10, 5));
        let (h, plugin, service) = gated_service(&rig);
        // Two transient commit failures: the settle job outlives the request.
        h.quota
            .script_commit((0..2).map(|_| Err(qe_errors::unavailable())));
        plugin.release.notify_one();
        let worker = rig.start_with(service.clone());
        let response = service
            .create_response(&ctx(), body(MODEL))
            .await
            .expect("response");
        assert!(rig.keys().is_empty(), "the settle has not reported yet");

        rig.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .expect("the reporter finishes after the settle")
            .expect("worker");

        assert_eq!(h.quota.commits().len(), 3, "two retries, then the commit");
        assert_eq!(rig.keys(), [response.id]);
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn producers_that_never_quiesce_cannot_hold_the_stop_past_budget_plus_reserve() {
        let rig = Rig::new(&cfg(10, 2));
        let budget = ShutdownConfig {
            usage_drain_reserve: RESERVE,
        }
        .quiescence_budget();
        let worker = rig.start_with(Gate::closed());
        rig.push(0..1).await;

        let cancelled = Instant::now();
        rig.cancel.cancel();
        tokio::time::sleep(budget.saturating_sub(Duration::from_secs(1))).await;
        assert!(!worker.is_finished(), "still within the quiescence budget");
        assert_eq!(rig.keys(), ["resp_0"], "delivery goes on while waiting");

        tokio::time::timeout(RESERVE + Duration::from_secs(10), worker)
            .await
            .expect("run returns by budget plus reserve")
            .expect("worker");

        let stopped_after = cancelled.elapsed();
        assert!(stopped_after >= budget, "{stopped_after:?}");
        assert!(stopped_after <= budget + RESERVE, "{stopped_after:?}");
        assert!(logs_contain("quiescence budget spent"));
    }

    #[tokio::test(start_paused = true)]
    async fn records_enqueued_while_waiting_for_producers_are_drained_with_the_reserve() {
        let rig = Rig::new(&cfg(10, 5));
        let gate = Gate::closed();
        let worker = rig.start_with(gate.clone());
        rig.cancel.cancel();
        settle().await;
        rig.push(0..2).await;
        gate.release();
        worker.await.expect("worker");
        assert_eq!(rig.keys(), ["resp_0", "resp_1"]);
        assert_eq!(rig.reporter.pending(), 0);
    }

    // ── closing (S5B-002) and the undelivered log (S5B-004) ─────────────

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_record_after_the_worker_stopped_is_logged_undelivered_not_queued() {
        let rig = Rig::new(&cfg(10, 3));
        let worker = rig.start();
        rig.cancel.cancel();
        worker.await.expect("worker");

        rig.reporter.record(report(9)).await;

        assert_eq!(rig.reporter.pending(), 0);
        assert!(logs_contain("usage record undelivered"));
        assert!(logs_contain("resp_9"));
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn records_racing_with_the_close_are_each_delivered_or_logged() {
        const TOTAL: u64 = 40;
        let rig = Rig::new(&cfg(100, 7));
        let worker = rig.start();
        let producer = {
            let reporter = Arc::clone(&rig.reporter);
            tokio::spawn(async move {
                for n in 0..TOTAL {
                    reporter.record(report(n)).await;
                    tokio::task::yield_now().await;
                }
            })
        };
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        rig.cancel.cancel();
        producer.await.expect("producer");
        worker.await.expect("worker");

        let delivered = rig.keys();
        for n in 0..TOTAL {
            let id = format!("resp_{n}");
            assert!(
                delivered.contains(&id) || logs_contain(&id),
                "{id} neither delivered nor logged"
            );
        }
        assert_eq!(rig.reporter.pending(), 0, "nothing stays queued");
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_worker_panic_closes_the_reporter_so_later_records_are_logged() {
        let rig = Rig::new(&cfg(10, 1));
        rig.uc.panic_on_call();
        let worker = rig.start();
        rig.push(0..1).await;
        assert!(worker.await.expect_err("worker panicked").is_panic());

        rig.reporter.record(report(5)).await;

        assert_eq!(rig.reporter.pending(), 0, "the dead worker's batch is gone");
        assert!(logs_contain("usage record undelivered"));
        assert!(logs_contain("resp_5"));
        assert!(logs_contain("usage records lost"));
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn undelivered_ids_are_logged_in_chunks_of_at_most_one_hundred_with_the_total() {
        let rig = Rig::new(&cfg(1000, 100));
        rig.uc.park();
        let worker = rig.start();
        rig.push(0..100).await;
        settle().await;
        // Queued behind the parked batch of 100 (not in flight).
        rig.push(100..350).await;

        rig.cancel.cancel();
        worker.await.expect("worker");

        assert!(logs_contain("total=100"), "the parked in-flight batch");
        assert!(logs_contain("total=250"), "the queued rest");
        assert!(logs_contain("count=50"), "last chunk of the rest");
        assert!(logs_contain("resp_349"));
        assert_eq!(rig.reporter.pending(), 0);
    }
}

// ── timeouts.dependency.usage_collector ─────────────────────────────────────

mod call_timeout {
    use super::*;

    fn rig_with_timeout(timeout: Duration) -> Rig {
        let mut rig = Rig::new(&cfg(10, 2));
        let reporter = UsageCollectorReporter::new(
            rig.uc.clone(),
            rig.identity.clone(),
            &cfg(10, 2),
            &ShutdownConfig {
                usage_drain_reserve: RESERVE,
            },
        )
        .expect("reporter")
        .with_backoff_jitter(0.0)
        .with_call_timeout(timeout);
        rig.reporter = Arc::new(reporter);
        rig
    }

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_call_over_the_bound_is_a_transient_failure_retried_after_the_backoff() {
        let rig = rig_with_timeout(Duration::from_secs(3));
        rig.uc.hang_first(1);
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(20)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 2, "the hung call, then the retry");
        assert!(
            approx(calls[1].at - calls[0].at, Duration::from_secs(3) + BACKOFF),
            "cut at the bound, then the backoff: {:?}",
            calls[1].at - calls[0].at
        );
        let keys: Vec<&str> = calls[1]
            .records
            .iter()
            .map(|r| r.idempotency_key.as_str())
            .collect();
        assert_eq!(keys, ["resp_0", "resp_1"], "the whole batch is retried");
        assert_eq!(rig.reporter.pending(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_default_bound_is_five_seconds() {
        let rig = Rig::new(&cfg(10, 2));
        rig.uc.hang_first(1);
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_secs(20)).await;

        let calls = rig.uc.calls();
        assert_eq!(calls.len(), 2);
        assert!(approx(
            calls[1].at - calls[0].at,
            Duration::from_secs(5) + BACKOFF
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_collector_is_retried_repeatedly_not_waited_for_forever() {
        let rig = rig_with_timeout(Duration::from_secs(3));
        rig.uc.hang_first(2);
        let _worker = rig.start();
        rig.push(0..2).await;
        tokio::time::sleep(Duration::from_mins(1)).await;

        assert_eq!(rig.uc.calls().len(), 3, "two hung calls, then delivery");
        assert_eq!(rig.reporter.pending(), 0);
    }
}

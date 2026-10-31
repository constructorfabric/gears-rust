//! The Preview service end to end over the Task 8b stubs, the counted orchestrator harness, a
//! scripted PDP and recording doubles of the date preparer and the diagnostics writer (S3-11;
//! F03 §2.2, §4.6; D-215, D-125). The Postgres write itself is the writer tests'
//! (`tests/writer.rs`). Compiled only with `gate-dev-stubs`.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use super::orchestrate::{
    Harness, ITEM, PAYER, PLAN, RESOURCE, REVISION, SELLER, day, fixture, harness, harness_with,
};
use crate::authz::test_pdp::{ScriptedPdp, allow, deny, eq, path, pep, user};
use crate::authz::{Caller, Pep};
use crate::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use crate::domain::gate::ports::GatePort;
use crate::domain::gate::predicates::{PREDICATES, Scope};
use crate::gts::permissions::properties;
use crate::infra::dates::PrepareDatesError;
use crate::infra::gate::budget::Budget;
use crate::infra::gate::preview::{PreviewDates, PreviewError, PreviewRuns, PreviewService};
use crate::infra::gate::signals::{Flow, GateObservation, RecordingGateSignals};
use crate::infra::gate::stubs::StubFixture;
use crate::infra::gate::writer::{RunRecord, RunTarget};
use crate::infra::throttle::{CallerLimitSettings, CallerLimiter, PerCallerLimiter, Rejection};
use async_trait::async_trait;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::preview::{PinOutcomeView, PreviewBasket, PreviewResult};
use parking_lot::Mutex;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_security::pep_properties;
use uuid::Uuid;

const SUBJECT: u128 = 0x5b;
const SUBJECT_TENANT: u128 = 0x5c;

/// The run's frozen basis over the platform default (no date required), on the harness day.
#[derive(Default)]
struct FixedDates {
    prepared: AtomicUsize,
    overlap: Mutex<Option<Arc<PreviewOverlap>>>,
}

/// The test coordinator observes both arrivals before allowing either request to continue.
struct PreviewOverlap {
    entered: tokio::sync::Barrier,
    release: tokio::sync::Barrier,
}
#[async_trait]
impl PreviewDates for FixedDates {
    async fn prepare(
        &self,
        resource_tenant_id: Uuid,
        lines: Vec<(Uuid, SuppliedDates)>,
    ) -> Result<DateBasis, PrepareDatesError> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        let overlap = self.overlap.lock().clone();
        if let Some(overlap) = overlap {
            overlap.entered.wait().await;
            overlap.release.wait().await;
        }
        let row = PolicyRow {
            policy_id: PLATFORM_DEFAULT_POLICY_ID,
            resource_tenant_id: None,
            service_activation_required: false,
            acceptance_due_required: false,
            revision: 1,
        };
        let policy = DatePolicySnapshot::effective(resource_tenant_id, &[row])?;
        Ok(DateBasis::resolve(resource_tenant_id, policy, day(), lines))
    }
}

/// One committed run: `(run_id, target, row count, write scope)`.
type WrittenRow = (Uuid, RunTarget, usize, AccessScope);

/// Records every committed run (`run_id`, target, row count, write scope), or fails.
#[derive(Default)]
struct RecordingRuns {
    fail: bool,
    written: Mutex<Vec<WrittenRow>>,
}
#[async_trait]
impl PreviewRuns for RecordingRuns {
    async fn write(&self, scope: &AccessScope, run: &RunRecord<'_>) -> Result<Uuid, ScopeError> {
        if self.fail {
            return Err(ScopeError::Invalid("injected commit failure"));
        }
        self.written.lock().push((
            run.run_id,
            run.target,
            run.vector.outcomes().len(),
            scope.clone(),
        ));
        Ok(run.run_id)
    }
}

struct Fixture {
    service: PreviewService,
    h: Harness,
    dates: Arc<FixedDates>,
    runs: Arc<RecordingRuns>,
    pdp: Arc<ScriptedPdp>,
}

/// The complete authorized arrangement of the basket.
fn allowing() -> Arc<ScriptedPdp> {
    ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![
            eq(properties::RESOURCE_TENANT_ID, RESOURCE),
            eq(properties::SELLER_TENANT_ID, SELLER),
            eq(properties::PAYER_TENANT_ID, PAYER),
        ])]))
    })
}

fn service(f: StubFixture, pdp: Arc<ScriptedPdp>, fail_write: bool, per_minute: u32) -> Fixture {
    service_over(harness(f, false), pdp, fail_write, per_minute)
}

/// [`service`] over a prepared harness (a recording signal sink, say).
fn service_over(h: Harness, pdp: Arc<ScriptedPdp>, fail_write: bool, per_minute: u32) -> Fixture {
    let dates = Arc::new(FixedDates::default());
    let runs = Arc::new(RecordingRuns {
        fail: fail_write,
        ..RecordingRuns::default()
    });
    let pep: Pep = pep(pdp.clone());
    let limiter = PerCallerLimiter::new(
        CallerLimiter::Preview,
        CallerLimitSettings {
            per_minute: NonZeroU32::new(per_minute).unwrap(),
            max_keys: 100,
        },
    );
    let service = PreviewService::new(
        pep,
        h.orchestrator.clone(),
        dates.clone(),
        runs.clone(),
        limiter,
    );
    Fixture {
        service,
        h,
        dates,
        runs,
        pdp,
    }
}

fn basket(lines: usize) -> PreviewBasket {
    let line = serde_json::json!({
        "plan_id": PLAN,
        "plan_revision_id": REVISION,
        "selected_items": [{"item_id": ITEM, "quantity": "1"}],
        "currency": "USD",
        "term_duration": {"kind": "rolling"},
        "billing_cycle": "month",
    });
    serde_json::from_value(serde_json::json!({
        "resource_tenant_id": RESOURCE,
        "payer_tenant_id": PAYER,
        "seller_tenant_id": SELLER,
        "lines": vec![line; lines],
    }))
    .unwrap()
}

fn caller() -> Caller {
    user(SUBJECT, SUBJECT_TENANT)
}

async fn preview(f: &Fixture, b: PreviewBasket) -> Result<PreviewResult, OrdersError> {
    f.service
        .preview(&caller(), b, None, &CancellationToken::new())
        .await
}

/// No gate port, no date preparation and no write happened.
fn nothing_called(f: &Fixture) {
    for port in GatePort::ALL {
        assert_eq!(f.h.calls.of(port), 0, "{port:?}");
    }
    assert_eq!(*f.h.calls.read_lines.lock(), 0);
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), 0);
    assert!(f.runs.written.lock().is_empty());
}

#[tokio::test]
async fn a_stub_run_persists_its_vector_and_returns_the_shaped_result() {
    let f = service(fixture(), allowing(), false, 60);
    let result = preview(&f, basket(2)).await.unwrap();
    assert_eq!(f.pdp.calls().len(), 1, "one PDP decision");
    assert_eq!(*f.h.calls.read_lines.lock(), 1, "one catalog call (M2-6)");
    assert_eq!(f.h.calls.of(GatePort::IdentityProfile), 1);
    assert_eq!(f.h.calls.of(GatePort::Assessment), 1);
    // Run-local line identities, in basket order, distinct from the run.
    assert_eq!(result.lines.len(), 2);
    assert_ne!(result.lines[0].line_id, result.lines[1].line_id);
    // The committed run is the reported assessment; every row of the vector was written once,
    // under the caller's home-tenant scope.
    let written = f.runs.written.lock();
    assert_eq!(written.len(), 1);
    let (run_id, target, rows, scope) = &written[0];
    assert_eq!(*run_id, result.assessment_id);
    assert_eq!(*target, RunTarget::Preview);
    assert_eq!(*rows, result.assessment.outcomes.len());
    // One constraint with one filter: the owner tenant is the subject's tenant and nothing else.
    assert_eq!(scope.constraints().len(), 1, "{scope:?}");
    assert_eq!(scope.constraints()[0].filters().len(), 1, "{scope:?}");
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        [Uuid::from_u128(SUBJECT_TENANT)],
        "{scope:?}"
    );
    // Every line carries one row per line-scoped predicate (pin included).
    let per_line = PREDICATES.iter().filter(|p| p.scope == Scope::Line).count();
    for line in &result.lines {
        let rows = result
            .assessment
            .outcomes
            .iter()
            .filter(|o| o.line_id == Some(line.line_id) && o.item_id.is_none())
            .count();
        assert_eq!(rows, per_line, "line {}", line.line_id);
        assert_eq!(line.pin, PinOutcomeView::Composed);
        // The proposed date is the default activation date: nothing is deferred.
        assert!(line.deferral.is_none());
    }
    assert!(result.expected_fulfillment_time.is_none());
    // Both lines authored term and cycle and their terms resolved: the fixture's TCV is copied
    // verbatim, never withheld.
    assert!(result.tcv_withheld.is_none());
    let tcv = result.tcv.expect("tcv");
    assert_eq!(
        (tcv.exact.numerator.as_str(), tcv.exact.denominator.as_str()),
        ("126", "1")
    );
    assert_eq!(tcv.display, "126.00");
}

#[tokio::test]
async fn a_failed_diagnostics_write_returns_unavailable_and_no_result() {
    let f = service(fixture(), allowing(), true, 60);
    let out = preview(&f, basket(1)).await;
    assert!(matches!(out, Err(OrdersError::Unavailable)), "{out:?}");
    // The run happened; its result is never returned without the committed diagnostics.
    assert_eq!(*f.h.calls.read_lines.lock(), 1);
}

#[tokio::test]
async fn a_denied_or_seller_only_caller_is_refused_before_any_port_call() {
    let f = service(fixture(), ScriptedPdp::new(|_| Some(deny(None))), false, 60);
    let out = preview(&f, basket(1)).await;
    assert!(matches!(out, Err(OrdersError::Refused(_))), "{out:?}");
    nothing_called(&f);

    // F03 §4.6: seller-only authority grants read, not preview. A scripted
    // allow on preview with only a seller constraint would instead grant buyer
    // authority on every arrangement for that seller (other axes unrestricted).
    let seller_only = ScriptedPdp::new(|request| {
        Some(if request.action.name == "read" {
            allow(vec![path(vec![eq(properties::SELLER_TENANT_ID, SELLER)])])
        } else {
            deny(None)
        })
    });
    let f = service(fixture(), seller_only, false, 60);
    let out = preview(&f, basket(1)).await;
    assert!(
        matches!(
            out,
            Err(OrdersError::Refused(Reason::OperationNotPermittedForActor))
        ),
        "{out:?}"
    );
    nothing_called(&f);
    assert_eq!(f.pdp.calls(), vec![("preview".to_owned(), None)]);

    // A PDP outage is the sanitized unavailable answer, also before any port.
    let f = service(fixture(), ScriptedPdp::new(|_| None), false, 60);
    assert!(matches!(
        preview(&f, basket(1)).await,
        Err(OrdersError::Unavailable)
    ));
    nothing_called(&f);
}

#[tokio::test]
async fn an_unconfirmed_payer_withholds_every_payer_port() {
    let mut fx = fixture();
    fx.payer_profiles
        .get_mut(&PAYER)
        .unwrap()
        .in_relationship_with_seller = false;
    let f = service(fx, allowing(), false, 60);
    let result = preview(&f, basket(1)).await.unwrap();
    for port in [
        GatePort::ContractResolution,
        GatePort::OverlapKey,
        GatePort::Occupancy,
        GatePort::BillingTerms,
        GatePort::Evaluation,
        GatePort::IndicativeTax,
    ] {
        assert_eq!(f.h.calls.of(port), 0, "{port:?}");
    }
    assert_eq!(f.h.inflight.reads.load(Ordering::SeqCst), 0);
    assert!(
        result
            .assessment
            .failures
            .iter()
            .any(|x| x.reason == Reason::PayerRelationshipUnconfirmed),
        "{:?}",
        result.assessment.failures
    );
    // No evaluation was asked: no total, no tax, no TCV figure.
    assert!(result.resolved_total.is_none());
    assert!(result.indicative_tax.is_none());
    assert!(result.tcv.is_none());
}

#[tokio::test]
async fn the_sixty_first_preview_in_a_minute_is_throttled_before_the_run() {
    let f = service(fixture(), allowing(), false, 60);
    for _ in 0..60 {
        preview(&f, basket(1)).await.unwrap();
    }
    let runs_before = *f.h.calls.read_lines.lock();
    let out = f
        .service
        .run(&caller(), basket(1), None, &CancellationToken::new())
        .await;
    assert!(
        matches!(
            out,
            Err(PreviewError::Throttled(Rejection::PerCallerLimit { .. }))
        ),
        "{out:?}"
    );
    assert_eq!(*f.h.calls.read_lines.lock(), runs_before, "no run");
    assert_eq!(f.runs.written.lock().len(), 60);
    // The SDK answer is the canonical resource-exhausted error.
    let sdk = preview(&f, basket(1)).await;
    assert!(matches!(sdk, Err(OrdersError::Other { .. })), "{sdk:?}");
}

#[tokio::test]
async fn an_invalid_basket_is_refused_before_authorization() {
    let f = service(fixture(), allowing(), false, 60);
    let out = preview(&f, basket(0)).await;
    assert!(
        matches!(out, Err(OrdersError::Refused(Reason::RequestInvalid))),
        "{out:?}"
    );
    assert!(f.pdp.calls().is_empty());
    nothing_called(&f);
}

#[tokio::test]
async fn a_throttled_preview_still_counts_as_a_request() {
    let sink = Arc::new(RecordingGateSignals::default());
    let budget = Arc::new(Budget::from_config(
        &crate::config::GateConfig::default(),
        sink.clone(),
    ));
    let f = service_over(
        harness_with(fixture(), false, budget, |_| {}),
        allowing(),
        false,
        1,
    );
    let requests = || {
        sink.observations()
            .into_iter()
            .filter(|o| {
                *o == GateObservation::Request {
                    flow: Flow::Preview,
                }
            })
            .count()
    };
    let runs = || {
        sink.observations()
            .into_iter()
            .filter(|o| matches!(o, GateObservation::Run { .. }))
            .count()
    };
    preview(&f, basket(1)).await.unwrap();
    assert_eq!((requests(), runs()), (1, 1));
    let out = f
        .service
        .run(&caller(), basket(1), None, &CancellationToken::new())
        .await;
    assert!(matches!(out, Err(PreviewError::Throttled(_))), "{out:?}");
    assert_eq!(
        (requests(), runs()),
        (2, 1),
        "the throttled Preview is one request and no run"
    );
}

/// Identical concurrent baskets are independent executions, never idempotency replays.
#[tokio::test]
async fn concurrent_identical_previews_write_two_independent_runs() {
    let f = service(fixture(), allowing(), false, 60);
    let overlap = Arc::new(PreviewOverlap {
        entered: tokio::sync::Barrier::new(3),
        release: tokio::sync::Barrier::new(3),
    });
    *f.dates.overlap.lock() = Some(overlap.clone());
    let (a, b, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(preview(&f, basket(1)), preview(&f, basket(1)), async {
            overlap.entered.wait().await;
            assert_eq!(
                f.dates.prepared.load(Ordering::SeqCst),
                2,
                "both identical calls are in flight before either may finish preparation"
            );
            assert!(
                f.runs.written.lock().is_empty(),
                "neither call has persisted"
            );
            overlap.release.wait().await;
        })
    })
    .await
    .expect("both Preview calls must overlap and complete within five seconds");
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.assessment_id, b.assessment_id);
    assert_ne!(a.lines[0].line_id, b.lines[0].line_id);
    let written = f.runs.written.lock();
    assert_eq!(written.len(), 2);
    for result in [a, b] {
        assert_eq!(
            written
                .iter()
                .filter(|(id, target, rows, _)| *id == result.assessment_id
                    && *target == RunTarget::Preview
                    && *rows == result.assessment.outcomes.len())
                .count(),
            1
        );
    }
}

#[tokio::test]
#[tracing_test::traced_test]
async fn failed_local_inflight_reads_return_sanitized_unavailable_without_diagnostics_write() {
    use super::orchestrate::{
        FailedInflightRead, FailingInflight, PRIVATE_DB_CAUSE, harness_with_inflight,
    };
    for fail in [FailedInflightRead::Count, FailedInflightRead::Tuple] {
        let reads = Arc::new(FailingInflight::new(fail, None));
        let h = harness_with_inflight(fixture(), Arc::new(Budget::baseline()), reads);
        let f = service_over(h, allowing(), false, 60);
        let error = preview(&f, basket(1)).await.unwrap_err();
        assert!(matches!(error, OrdersError::Unavailable));
        let problem = crate::api::rest::error::problem(error);
        assert_eq!(problem.status, Some(503));
        assert!(
            !serde_json::to_string(&problem)
                .unwrap()
                .contains(PRIVATE_DB_CAUSE)
        );
        assert!(f.runs.written.lock().is_empty());
        for port in [
            GatePort::Assessment,
            GatePort::Evaluation,
            GatePort::IndicativeTax,
        ] {
            assert_eq!(f.h.calls.of(port), 0);
        }
    }
    assert!(logs_contain("in-flight line count"));
    assert!(logs_contain("in-flight tuple lookup"));
    assert!(!logs_contain(PRIVATE_DB_CAUSE));
}

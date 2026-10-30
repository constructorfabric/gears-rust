//! The fault-injection matrix (D-237 item 6; 03 §3.8): one test per injected condition, each
//! proving that the signal behind its alert moves. Rows 1–7 drive `Orchestrator::run` over the
//! orchestrate harness (the counted stubs over the example fixture or a row fixture under
//! `fixtures/`, and, where Pricing is the fault, the production catalog adapter over
//! [`FakePricing`]); row 8 drives `bounded` directly. Compiled only with `gate-dev-stubs`.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use super::doubles::{FakePricing, Fault, pricing_catalog};
use super::orchestrate::{
    CONTRACT, Harness, ITEM, LINE, PAYER, PLAN, RESOURCE, REVISION, SELLER, day, fixture,
    harness_with, target,
};
use super::signals::{Point, exported, install_meter, meter_lock, point};
use crate::config::GateConfig;
use crate::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use crate::domain::gate::answers::{EvaluationCycle, TermIntentInput};
use crate::domain::gate::assess::BasketLine;
use crate::domain::gate::breaker::BreakerState;
use crate::domain::gate::orchestrate::{Basket, LineTerms, TenantAxes};
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::domain::gate::predicates::PredicateId;
use crate::domain::gate::pricing_map::{LineCatalogRequest, SelectedItem};
use crate::domain::gate::signals::RunSignals;
use crate::domain::gate::vector::Operation;
use crate::infra::gate::budget::{
    Budget, CallError, CallOutcome, PORT_BULKHEAD, RunBudget, bounded, to_unavailable,
};
use crate::infra::gate::orchestrate::Resolved;
use crate::infra::gate::signals::{
    Flow, GateObservation, GateSignals, OtelGateSignals, RecordingGateSignals,
};
use crate::infra::gate::stubs::StubFixture;

/// The fixture order a Submit or Amendment run belongs to.
use bss_orders_sdk::catalog::Reason;
use bss_pricing_sdk::read::{BindingSelection, ResolvedCell, RevisionRef};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A row fixture under `fixtures/`: the example fixture with one entry removed.
fn row_fixture(name: &str) -> StubFixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/infra/gate/tests/fixtures")
        .join(name);
    StubFixture::load(&path).unwrap()
}

/// [`RecordingGateSignals`] that also keeps each run's elapsed time (the recorder drops it) and,
/// when `otel` is set, forwards every call to the production sink as well.
#[derive(Debug, Default)]
struct Sink {
    recorded: RecordingGateSignals,
    runs: parking_lot::Mutex<Vec<(Flow, Duration)>>,
    otel: Option<OtelGateSignals>,
}

impl Sink {
    fn observations(&self) -> Vec<GateObservation> {
        self.recorded.observations()
    }
    fn runs(&self) -> Vec<(Flow, Duration)> {
        self.runs.lock().clone()
    }
}

impl GateSignals for Sink {
    fn port_call(&self, port: GatePort, outcome: CallOutcome, duration: Option<Duration>) {
        self.recorded.port_call(port, outcome, duration);
        if let Some(otel) = &self.otel {
            otel.port_call(port, outcome, duration);
        }
    }
    fn port_retry(&self, port: GatePort) {
        self.recorded.port_retry(port);
        if let Some(otel) = &self.otel {
            otel.port_retry(port);
        }
    }
    fn breaker(&self, port: GatePort, state: BreakerState) {
        self.recorded.breaker(port, state);
        if let Some(otel) = &self.otel {
            otel.breaker(port, state);
        }
    }
    fn breaker_initial(&self, port: GatePort, state: BreakerState) {
        self.recorded.breaker_initial(port, state);
        if let Some(otel) = &self.otel {
            otel.breaker_initial(port, state);
        }
    }
    fn port_unavailable(&self, port: GatePort, kind: Unavailable) {
        self.recorded.port_unavailable(port, kind);
        if let Some(otel) = &self.otel {
            otel.port_unavailable(port, kind);
        }
    }
    fn catalog_denial(&self, catalog_tenant: Uuid) {
        self.recorded.catalog_denial(catalog_tenant);
        if let Some(otel) = &self.otel {
            otel.catalog_denial(catalog_tenant);
        }
    }
    fn run(&self, flow: Flow, signals: &RunSignals, elapsed: Duration) {
        self.runs.lock().push((flow, elapsed));
        self.recorded.run(flow, signals, elapsed);
        if let Some(otel) = &self.otel {
            otel.run(flow, signals, elapsed);
        }
    }
    fn request(&self, flow: Flow) {
        self.recorded.request(flow);
        if let Some(otel) = &self.otel {
            otel.request(flow);
        }
    }
}

/// A budget over validated `config` whose signals land in the returned sink.
fn budget(config: &GateConfig) -> (Arc<Budget>, Arc<Sink>) {
    budget_over(config, Sink::default())
}

/// [`budget`] whose signals land in `sink`.
fn budget_over(config: &GateConfig, sink: Sink) -> (Arc<Budget>, Arc<Sink>) {
    config.validate().unwrap();
    let sink = Arc::new(sink);
    (Arc::new(Budget::from_config(config, sink.clone())), sink)
}

/// The orchestrate harness over `fixture` and `budget`, its catalog the production adapter over
/// `pricing` when given (the harness's counted catalog double otherwise).
fn harness(
    fixture: StubFixture,
    budget: &Arc<Budget>,
    pricing: Option<&Arc<FakePricing>>,
) -> Harness {
    let mut h = harness_with(fixture, false, Arc::clone(budget), |_| {});
    if let Some(pricing) = pricing {
        let catalog = pricing_catalog(Arc::clone(pricing), SELLER, Arc::clone(budget));
        h.orchestrator = h.orchestrator.clone().with_catalog(Arc::new(catalog));
    }
    h
}

/// Pricing answering the example's plan and revision with `cells`.
fn pricing(cells: Vec<ResolvedCell>) -> Arc<FakePricing> {
    Arc::new(FakePricing::new(
        RevisionRef {
            plan_id: PLAN,
            revision_id: REVISION,
            revision_no: 1,
        },
        cells,
    ))
}

/// The `i`-th basket line: line 0 is the fixture's `LINE` on `REVISION`; every other line has
/// its own id and its own revision, so each line is one distinct `resolve`.
fn line_ids(i: u128) -> (Uuid, Uuid) {
    if i == 0 {
        (LINE, REVISION)
    } else {
        (Uuid::from_u128(0xf100 + i), Uuid::from_u128(0xe100 + i))
    }
}

/// A basket of `lines` lines of `ITEM` on one plan (the orchestrate harness's basket, widened).
fn basket(operation: Operation, lines: u128) -> Basket {
    let ids: Vec<(Uuid, Uuid)> = (0..lines).map(line_ids).collect();
    let basket_lines = ids
        .iter()
        .map(|&(line_id, revision_id)| BasketLine {
            line_id,
            catalog: LineCatalogRequest {
                line_id,
                plan_id: PLAN,
                revision_id,
                items: vec![SelectedItem {
                    item_id: ITEM,
                    quantity: "1".into(),
                    selected_dim_value: None,
                }],
            },
            currency: Some("USD".into()),
            contract_ref: Some(CONTRACT),
        })
        .collect();
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    let policy = DatePolicySnapshot::effective(RESOURCE, &[row]).unwrap();
    let dates = DateBasis::resolve(
        RESOURCE,
        policy,
        day(),
        ids.iter().map(|&(l, _)| (l, SuppliedDates::default())),
    );
    Basket {
        assessed_at: time::OffsetDateTime::UNIX_EPOCH,
        target: target(operation),
        assessment_id: Uuid::from_u128(0xa55),
        axes: TenantAxes {
            resource: RESOURCE,
            payer: PAYER,
            seller: SELLER,
        },
        contract_id: None,
        lines: basket_lines,
        terms: ids
            .iter()
            .map(|&(l, _)| {
                (
                    l,
                    LineTerms {
                        intent: crate::domain::gate::orchestrate::LineTermIntent::Stated(
                            TermIntentInput::Rolling,
                        ),
                        cycle: Some(EvaluationCycle::Month),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        resolve_date: day(),
        dates,
    }
}

async fn run(h: &Harness, b: &Basket) -> Resolved {
    h.orchestrator
        .run(&SecurityContext::anonymous(), b, &CancellationToken::new())
        .await
        .unwrap()
}

/// The `Run` observations' signals, in order.
fn run_signals(seen: &[GateObservation]) -> Vec<(Flow, RunSignals)> {
    seen.iter()
        .filter_map(|o| match o {
            GateObservation::Run { flow, signals } => Some((*flow, signals.clone())),
            _ => None,
        })
        .collect()
}

/// How many times `wanted` was observed.
fn count(seen: &[GateObservation], wanted: &GateObservation) -> usize {
    seen.iter().filter(|o| *o == wanted).count()
}

/// Whether `signals` carries an unevaluable `predicate` entry with `reason`.
fn unevaluable(signals: &RunSignals, predicate: PredicateId, reason: Reason) -> bool {
    signals
        .unevaluable
        .iter()
        .any(|s| s.predicate == predicate && s.reason == reason)
}

/// Row 1 (alert: breaker open). Ten transient `resolve` failures over two runs of a 5-line
/// basket open the resolve breaker (baseline: 10 outcomes, failure rate 0.5); the third run is
/// rejected before any subrequest.
#[tokio::test(start_paused = true)]
async fn alert_1_transient_failures_open_the_breaker() {
    let (budget, sink) = budget(&GateConfig::default());
    let pricing = pricing(Vec::new());
    pricing.fail_with(Some(Fault::Unavailable));
    let h = harness(fixture(), &budget, Some(&pricing));
    let b = basket(Operation::Submit, 5);

    run(&h, &b).await;
    run(&h, &b).await;
    let seen = sink.observations();
    let transient = GateObservation::PortCall {
        port: GatePort::Resolve,
        outcome: CallOutcome::Transient,
        timed: true,
    };
    assert_eq!(count(&seen, &transient), 10, "{seen:?}");
    // Each of the ten calls took its retry: twenty attempts reached Pricing's resolve.
    assert_eq!(pricing.resolves(), 20);
    let resolve_breaker: Vec<BreakerState> = seen
        .iter()
        .filter_map(|o| match o {
            GateObservation::Breaker {
                port: GatePort::Resolve,
                state,
            } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(resolve_breaker, vec![BreakerState::Open], "{seen:?}");
    // One plan: three `current_revision` failures stay under the minimum volume.
    assert!(
        !seen.iter().any(|o| matches!(
            o,
            GateObservation::Breaker {
                port: GatePort::CurrentRevision,
                ..
            }
        )),
        "{seen:?}"
    );

    let mark = seen.len();
    run(&h, &b).await;
    let next = &sink.observations()[mark..];
    let open = GateObservation::PortCall {
        port: GatePort::Resolve,
        outcome: CallOutcome::BreakerOpen,
        timed: false,
    };
    assert_eq!(count(next, &open), 5, "{next:?}");
    assert!(
        !next.iter().any(|o| matches!(
            o,
            GateObservation::PortCall {
                port: GatePort::Resolve,
                outcome: CallOutcome::Transient,
                ..
            }
        )),
        "{next:?}"
    );
    assert_eq!(
        count(
            next,
            &GateObservation::PortUnavailable {
                port: GatePort::Resolve,
                kind: Unavailable::Outage,
            }
        ),
        1,
        "{next:?}"
    );
    assert_eq!(
        pricing.resolves(),
        20,
        "an open breaker spends no subrequest"
    );
}

/// Row 2 (alert: unevaluable rate). Pricing never answers: the current-revision port hits its
/// run deadline, the run is blocked and carries unevaluable entries.
#[tokio::test(start_paused = true)]
async fn alert_2_hung_port_hits_its_deadline() {
    let (budget, sink) = budget(&GateConfig::default());
    let pricing = pricing(Vec::new());
    pricing.fail_with(Some(Fault::Hang));
    let h = harness(fixture(), &budget, Some(&pricing));

    run(&h, &basket(Operation::Submit, 1)).await;
    let seen = sink.observations();
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortCall {
                port: GatePort::CurrentRevision,
                outcome: CallOutcome::Deadline,
                timed: true,
            }
        ),
        1,
        "{seen:?}"
    );
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortUnavailable {
                port: GatePort::CurrentRevision,
                kind: Unavailable::Deadline,
            }
        ),
        1,
        "{seen:?}"
    );
    let runs = run_signals(&seen);
    assert_eq!(runs.len(), 1, "{seen:?}");
    let (flow, signals) = &runs[0];
    assert_eq!(*flow, Flow::Submit);
    assert!(signals.blocked, "{signals:?}");
    assert!(!signals.unevaluable.is_empty(), "{signals:?}");
    assert!(
        unevaluable(
            signals,
            PredicateId::CatalogRevisionCurrent,
            Reason::PricingRevisionUnavailable
        ),
        "{signals:?}"
    );
}

/// Row 3 (diagnostic, no alert). Pricing denies the Orders principal: the seller's catalog is
/// named on the denial counter, while the buyer sees `pricing-revision-unavailable`, never a
/// 403 (F03 §4.6, D-122).
#[tokio::test(start_paused = true)]
async fn alert_3_denied_catalog_read_counts_per_seller() {
    let (budget, sink) = budget(&GateConfig::default());
    let pricing = pricing(Vec::new());
    pricing.fail_with(Some(Fault::Denied));
    let h = harness(fixture(), &budget, Some(&pricing));

    let resolved = run(&h, &basket(Operation::Submit, 1)).await;
    let seen = sink.observations();
    assert!(
        seen.contains(&GateObservation::PortUnavailable {
            port: GatePort::Resolve,
            kind: Unavailable::Denied,
        }),
        "{seen:?}"
    );
    // `current_revision` and `resolve` are both denied, yet the seller's catalog denial counts
    // once per run.
    assert_eq!(
        count(
            &seen,
            &GateObservation::CatalogDenial {
                catalog_tenant: SELLER
            }
        ),
        1,
        "{seen:?}"
    );
    assert!(
        seen.contains(&GateObservation::PortUnavailable {
            port: GatePort::CurrentRevision,
            kind: Unavailable::Denied,
        }),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|o| matches!(
            o,
            GateObservation::CatalogDenial { catalog_tenant } if *catalog_tenant != SELLER
        )),
        "{seen:?}"
    );
    assert_eq!(
        resolved.assessment.primary.and_then(|o| o.reason),
        Some(Reason::PricingRevisionUnavailable)
    );
}

/// Row 4 (alert: unevaluable rate). No D-189 assessment answer: the assessment port is
/// missing and the catalog market predicate is unevaluable. The pin does not read the
/// assessment, so every pin still composes.
#[tokio::test(start_paused = true)]
async fn alert_4_missing_assessment_makes_predicates_unevaluable() {
    let (budget, sink) = budget(&GateConfig::default());
    let h = harness(
        row_fixture("alert-4-no-assessment-profile.json"),
        &budget,
        None,
    );

    run(&h, &basket(Operation::Submit, 1)).await;
    let seen = sink.observations();
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortUnavailable {
                port: GatePort::Assessment,
                kind: Unavailable::Missing,
            }
        ),
        1,
        "{seen:?}"
    );
    let runs = run_signals(&seen);
    assert_eq!(runs.len(), 1, "{seen:?}");
    let signals = &runs[0].1;
    assert!(signals.blocked, "{signals:?}");
    assert!(
        unevaluable(
            signals,
            PredicateId::CatalogDimensionMarket,
            Reason::CatalogPredicatesUnavailable
        ),
        "{signals:?}"
    );
    assert_eq!(signals.pins_unevaluable, 0, "{signals:?}");
    assert_eq!(signals.pins_unresolvable, 0, "{signals:?}");
}

/// Row 5 (alert: unevaluable rate, D-126). No occupancy entry for the line's overlap key: the
/// occupancy port answers missing and the run reports occupancy incomplete, with overlap
/// uniqueness unevaluable `overlap-presence-unevaluable`. (An entry that omits
/// `max_concurrent_active` is a decode error; only an explicit `null` is unlimited: D-237 item 9.)
#[tokio::test(start_paused = true)]
async fn alert_5_missing_occupancy_entry_sets_the_d126_flag() {
    let (budget, sink) = budget(&GateConfig::default());
    let h = harness(
        row_fixture("alert-5-no-occupancy-entry.json"),
        &budget,
        None,
    );

    run(&h, &basket(Operation::Submit, 1)).await;
    let seen = sink.observations();
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortUnavailable {
                port: GatePort::Occupancy,
                kind: Unavailable::Missing,
            }
        ),
        1,
        "{seen:?}"
    );
    let runs = run_signals(&seen);
    assert_eq!(runs.len(), 1, "{seen:?}");
    let signals = &runs[0].1;
    assert!(signals.occupancy_incomplete, "{signals:?}");
    assert!(
        unevaluable(
            signals,
            PredicateId::OverlapUniqueness,
            Reason::OverlapPresenceUnevaluable
        ),
        "{signals:?}"
    );
    // The engine's in-transaction claim decides the in-flight row: it passes here.
    assert!(
        !signals
            .unevaluable
            .iter()
            .any(|s| s.predicate == PredicateId::InFlightPerKey),
        "{signals:?}"
    );
}

/// Row 6 (alert: pin unresolvable). Pricing resolves the selected slot without a binding: the
/// line's pin is unresolvable.
#[tokio::test(start_paused = true)]
async fn alert_6_uncovered_slot_makes_a_pin_unresolvable() {
    let (budget, sink) = budget(&GateConfig::default());
    let uncovered = ResolvedCell {
        selection: BindingSelection {
            item_id: ITEM,
            dimension_value: None,
        },
        binding: None,
    };
    let pricing = pricing(vec![uncovered]);
    let h = harness(fixture(), &budget, Some(&pricing));

    run(&h, &basket(Operation::Submit, 1)).await;
    let seen = sink.observations();
    let runs = run_signals(&seen);
    assert_eq!(runs.len(), 1, "{seen:?}");
    let signals = &runs[0].1;
    assert_eq!(signals.pins_unresolvable, 1, "{signals:?}");
    assert_eq!(signals.pins_unevaluable, 0, "{signals:?}");
    // The catalog answered: the pin is unresolvable, not a port outage.
    assert!(
        !seen.iter().any(|o| matches!(
            o,
            GateObservation::PortUnavailable {
                port: GatePort::CurrentRevision | GatePort::Resolve,
                ..
            }
        )),
        "{seen:?}"
    );
}

/// Row 7 (alert: resolution ceiling). `resolve` answers after 2.6 s, inside its 3000 ms
/// override but past the 2500 ms Preview ceiling: the production sink counts the run in
/// `orders_gate_resolution_over_ceiling_total{flow=preview}`. A second run answered after 2.4 s
/// stays under the ceiling and is not counted.
#[tokio::test(start_paused = true)]
async fn alert_7_slow_resolve_crosses_the_preview_ceiling() {
    let guard = meter_lock().lock().await;
    let (provider, exporter) = install_meter(&guard);
    let mut config = GateConfig::default();
    config.deadline_ms.insert(GatePort::Resolve, 3000);
    let (budget, sink) = budget_over(
        &config,
        Sink {
            otel: Some(OtelGateSignals::new()),
            ..Sink::default()
        },
    );
    let pricing = pricing(vec![super::doubles::bound_cell(
        ITEM,
        None,
        super::orchestrate::SKU,
    )]);
    pricing.fail_with(Some(Fault::Delay(Duration::from_millis(2600))));
    let h = harness(fixture(), &budget, Some(&pricing));

    run(&h, &basket(Operation::Preview, 1)).await;
    let seen = sink.observations();
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortCall {
                port: GatePort::Resolve,
                outcome: CallOutcome::Ok,
                timed: true,
            }
        ),
        1,
        "{seen:?}"
    );
    let over_ceiling = |exported: &BTreeSet<Point>| -> Vec<Point> {
        exported
            .iter()
            .filter(|(name, ..)| name == "orders_gate_resolution_over_ceiling_total")
            .cloned()
            .collect()
    };
    let preview_over = vec![point(
        "orders_gate_resolution_over_ceiling_total",
        &[("flow", "preview")],
        1,
    )];
    assert_eq!(over_ceiling(&exported(&provider, &exporter)), preview_over);

    // Under the ceiling: the run is timed but not counted over it.
    pricing.fail_with(Some(Fault::Delay(Duration::from_millis(2400))));
    run(&h, &basket(Operation::Preview, 1)).await;
    let runs = sink.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[1].1 < Flow::Preview.ceiling(), "{runs:?}");
    let after = exported(&provider, &exporter);
    assert_eq!(over_ceiling(&after), preview_over);
    assert!(
        after.contains(&point(
            "orders_gate_resolution_duration_ms",
            &[("flow", "preview")],
            2
        )),
        "{after:?}"
    );
}

/// Row 8 (no alert; D-237 item 2). One call past the gear-wide bulkhead is backpressure at once,
/// an outage to its run, and not a breaker failure: the breaker stays closed.
#[tokio::test]
async fn alert_8_backpressure_is_not_a_breaker_failure() {
    const PORT: GatePort = GatePort::Resolve;
    let mut config = GateConfig::default();
    // The parked calls must never reach their run deadline.
    config.deadline_ms.insert(PORT, 60_000);
    let (budget, sink) = budget(&config);
    let notify = Notify::new();
    let parked = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let cancel = CancellationToken::new();
    // One run per call: the in-run limit (8) never queues them, only the bulkhead (32) decides.
    let runs: Vec<RunBudget> = (0..=PORT_BULKHEAD).map(|_| RunBudget::new()).collect();
    let (notify, parked, finished, cancel, budget_ref) =
        (&notify, &parked, &finished, &cancel, budget.as_ref());
    let calls = runs.iter().map(|run| async move {
        let result = bounded(budget_ref, run, PORT, cancel, move || async move {
            let released = notify.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            parked.fetch_add(1, Ordering::SeqCst);
            released.await;
            Ok::<(), CanonicalError>(())
        })
        .await;
        finished.fetch_add(1, Ordering::SeqCst);
        result
    });
    // Release the parked calls only once all 32 hold a permit and the 33rd has ended; a
    // regression that never gets there fails the test instead of hanging it.
    let release = async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while parked.load(Ordering::SeqCst) < PORT_BULKHEAD
                || finished.load(Ordering::SeqCst) < 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("32 calls parked on a permit and the 33rd rejected within 5 s");
        notify.notify_waiters();
    };
    let (results, ()) = tokio::join!(futures::future::join_all(calls), release);

    let rejected: Vec<&CallError> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(rejected.len(), 1, "{results:?}");
    assert!(
        matches!(rejected[0], CallError::Backpressure),
        "{rejected:?}"
    );
    assert_eq!(to_unavailable(rejected[0]), Unavailable::Outage);
    let seen = sink.observations();
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortCall {
                port: PORT,
                outcome: CallOutcome::Backpressure,
                timed: false,
            }
        ),
        1,
        "{seen:?}"
    );
    assert_eq!(
        count(
            &seen,
            &GateObservation::PortCall {
                port: PORT,
                outcome: CallOutcome::Ok,
                timed: true,
            }
        ),
        PORT_BULKHEAD,
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|o| matches!(o, GateObservation::Breaker { .. })),
        "{seen:?}"
    );
    assert_eq!(
        budget.breaker_state(PORT, tokio::time::Instant::now()),
        Some(BreakerState::Closed)
    );
}

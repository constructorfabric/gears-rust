//! API-level (router) tests for the ASC 606 recognition REST surface
//! (`crate::api::rest::recognition`): the run trigger (POST), the
//! revenue-disaggregation report (GET), the by-id schedule read (GET), the
//! schedule list/discovery (GET), and the schedule change/cancel (POST).
//!
//! Drives the router via `tower::ServiceExt::oneshot` against a stub
//! `LedgerClientV1` (no DB) + an in-test `PolicyEnforcer` fake (no PDP), mirroring
//! `rest_journal_entries.rs` / `rest_payments.rs`. Covers: trigger → 200 (Ran) and
//! 202 (`recognition-period-queued`); disaggregation → 200; by-id read → 200 and
//! 404 (scoped-out/absent); list → 200; change/cancel → 200; the unauthenticated
//! path (401); and a PDP deny (403). `approval` is `None` (no governance DB), so
//! the change handler's dual-control gate is skipped — the threshold routing is
//! covered by `postgres_dual_control.rs` + the e2e.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bss_ledger::api::rest::recognition::{ApiState, RecognitionApprovalGate, router};
use bss_ledger_sdk::api::LedgerClientV1;
use bss_ledger_sdk::posting::{
    RecognitionRunOutcome, RecognitionRunQueued, RecognitionRunRef, RecognitionScheduleList,
    RecognitionScheduleSegmentView, RecognitionScheduleSummaryView, RecognitionScheduleView,
    RevenueDisaggregation, RevenueDisaggregationEntry, ScheduleChangeRef,
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::pep_properties;
use tower::ServiceExt;
use uuid::Uuid;

/// The authenticated caller's tenant — also the only tenant the allow fake
/// authorizes (its `In` constraint echoes it), so every body/query `tenant_id`
/// uses it (a cross-tenant target is covered by `postgres_dual_control` + e2e).
const SUBJECT_TENANT: Uuid = uuid::uuid!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
const SUBJECT_ID: Uuid = uuid::uuid!("11111111-2222-3333-4444-555555555555");

// ── Stub client ───────────────────────────────────────────────────────────────

/// In-test data-access stub. The recognition methods return canned values; the
/// `run_queued` / `schedule_absent` flags drive the 202 / 404 branches. The other
/// (non-recognition) methods are not reached by the recognition router.
#[derive(Default)]
struct StubClient {
    run_queued: bool,
    schedule_absent: bool,
    /// Override the returned schedule's status (e.g. `REPLACED`); `None` ⇒ `ACTIVE`
    /// (the `canned_schedule` default). Drives the gate ACTIVE-filter test.
    schedule_status: Option<String>,
}

fn canned_schedule() -> RecognitionScheduleView {
    RecognitionScheduleView {
        schedule_id: "SCH-1".to_owned(),
        status: "ACTIVE".to_owned(),
        version: 0,
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
        total_deferred_minor: 1200,
        recognized_minor: 0,
        source_invoice_id: "INV-1".to_owned(),
        source_invoice_item_ref: "INV-1#1".to_owned(),
        po_allocation_group: None,
        subscription_ref: None,
        policy_ref: "straight-line.v1".to_owned(),
        segments: vec![RecognitionScheduleSegmentView {
            segment_no: 1,
            period_id: "202606".to_owned(),
            amount_minor: 1200,
            status: "PENDING".to_owned(),
        }],
    }
}

fn canned_summary() -> RecognitionScheduleSummaryView {
    RecognitionScheduleSummaryView {
        schedule_id: "SCH-1".to_owned(),
        status: "ACTIVE".to_owned(),
        version: 0,
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
        total_deferred_minor: 1200,
        recognized_minor: 0,
        source_invoice_id: "INV-1".to_owned(),
        source_invoice_item_ref: "INV-1#1".to_owned(),
        po_allocation_group: None,
        subscription_ref: None,
        policy_ref: "straight-line.v1".to_owned(),
    }
}

#[async_trait::async_trait]
impl LedgerClientV1 for StubClient {
    async fn trigger_recognition_run(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        req: bss_ledger_sdk::TriggerRecognitionRun,
    ) -> Result<RecognitionRunOutcome, CanonicalError> {
        if self.run_queued {
            Ok(RecognitionRunOutcome::Queued(RecognitionRunQueued {
                run_id: Uuid::now_v7(),
                period_id: req.period_id,
                released: 1,
                queued: 2,
            }))
        } else {
            Ok(RecognitionRunOutcome::Ran(RecognitionRunRef {
                run_id: Uuid::now_v7(),
                period_id: req.period_id,
                replayed: false,
                released: 3,
                already_recognized: 0,
            }))
        }
    }

    async fn list_revenue_disaggregation(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        query: bss_ledger_sdk::RevenueDisaggregationQuery,
    ) -> Result<RevenueDisaggregation, CanonicalError> {
        Ok(RevenueDisaggregation {
            entries: vec![RevenueDisaggregationEntry {
                period_id: query.period_id.unwrap_or_else(|| "202606".to_owned()),
                revenue_stream: "subscription".to_owned(),
                recognized_minor: 1200,
                currency: "USD".to_owned(),
            }],
        })
    }

    async fn get_recognition_schedule(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _schedule_id: String,
    ) -> Result<Option<RecognitionScheduleView>, CanonicalError> {
        if self.schedule_absent {
            return Ok(None);
        }
        let mut view = canned_schedule();
        if let Some(status) = &self.schedule_status {
            view.status.clone_from(status);
        }
        Ok(Some(view))
    }

    async fn list_recognition_schedules(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _invoice_id: Option<String>,
        _revenue_stream: Option<String>,
    ) -> Result<RecognitionScheduleList, CanonicalError> {
        Ok(RecognitionScheduleList {
            schedules: vec![canned_summary()],
            truncated: false,
        })
    }

    async fn change_recognition_schedule(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _cmd: bss_ledger_sdk::ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, CanonicalError> {
        Ok(ScheduleChangeRef {
            schedule_id: "SCH-1".to_owned(),
            new_schedule_id: None,
            status: "CANCELLED".to_owned(),
        })
    }

    // ── not reached by the recognition router ──
    async fn return_payment(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::ReturnPayment,
    ) -> Result<bss_ledger_sdk::PostingRef, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn record_dispute_phase(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::RecordDisputePhase,
    ) -> Result<bss_ledger_sdk::DisputeOutcome, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn post_credit_application(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::CreditApplication,
    ) -> Result<bss_ledger_sdk::CreditApplicationApplied, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn post_balanced_entry(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _entry: bss_ledger_sdk::PostEntry,
    ) -> Result<bss_ledger_sdk::PostingRef, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn read_account_balance(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _account_id: Uuid,
    ) -> Result<Option<i64>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn list_accounts(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<bss_ledger_sdk::Page<bss_ledger_sdk::AccountInfo>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn get_entry(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _entry_id: Uuid,
    ) -> Result<Option<bss_ledger_sdk::EntryView>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn list_lines(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<bss_ledger_sdk::Page<bss_ledger_sdk::LineView>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn list_balances(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<bss_ledger_sdk::Page<bss_ledger_sdk::BalanceView>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn list_ar_invoice_balances(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _payer_tenant_id: Option<Uuid>,
    ) -> Result<Vec<bss_ledger_sdk::ArInvoiceBalanceView>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn provision(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::ProvisionRequest,
    ) -> Result<bss_ledger_sdk::ProvisionOutcome, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn close_period(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _period_id: String,
    ) -> Result<bss_ledger_sdk::CloseOutcome, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn settle_payment(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::SettlePayment,
    ) -> Result<bss_ledger_sdk::PostingRef, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn allocate_payment(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: bss_ledger_sdk::AllocatePayment,
    ) -> Result<bss_ledger_sdk::AllocateOutcome, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn list_payment_allocations(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _payment_id: String,
    ) -> Result<Vec<bss_ledger_sdk::AllocationView>, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
    async fn read_unallocated(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant_id: Uuid,
        _payer_tenant_id: Uuid,
        _currency: String,
    ) -> Result<bss_ledger_sdk::UnallocatedView, CanonicalError> {
        unimplemented!("not exercised by the recognition router tests")
    }
}

// ── PDP fakes + router harness (mirror rest_journal_entries.rs) ────────────────

/// Always-allow fake emitting a flat `In([SUBJECT_TENANT])` over `owner_tenant_id`
/// — the degraded-mode shape this gear's PEP compiles. A target/`tenant_id` equal
/// to `SUBJECT_TENANT` passes the gate's write-membership assert.
struct AllowInResolver;

#[async_trait]
impl AuthZResolverClient for AllowInResolver {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![SUBJECT_TENANT],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// Always-deny fake — the PDP refuses, so the gate maps it to 403.
struct DenyResolver;

#[async_trait]
impl AuthZResolverClient for DenyResolver {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: vec![],
                deny_reason: None,
            },
        })
    }
}

fn allow_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(AllowInResolver))
}

fn deny_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyResolver))
}

/// Build the recognition router over `client` + `enforcer` (`approval = None`, so
/// the change handler's dual-control gate is skipped).
fn router_with(client: StubClient, enforcer: PolicyEnforcer) -> Router {
    let state = Arc::new(ApiState {
        client: Arc::new(client) as Arc<dyn LedgerClientV1>,
        approval: None,
        recognition_repo: None,
    });
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    router(state, &openapi).layer(axum::Extension(enforcer))
}

fn authed_ctx() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(SUBJECT_ID)
        .subject_tenant_id(SUBJECT_TENANT)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

/// An authenticated router (allow enforcer + default stub).
fn authed_router(client: StubClient) -> Router {
    router_with(client, allow_enforcer()).layer(axum::Extension(authed_ctx()))
}

async fn get(router: Router, uri: &str) -> StatusCode {
    router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("router call")
        .status()
}

async fn post_json(router: Router, uri: &str, body: serde_json::Value) -> StatusCode {
    router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("build req"),
        )
        .await
        .expect("router call")
        .status()
}

// ── Dual-control gate stub ──────────────────────────────────────────────────

/// A recording stub for the recognition dual-control gate. `decision` is what
/// `gate` returns — `Some(id)` (over threshold ⇒ the handler 409s) or `None`
/// (below ⇒ inline) — and `calls` counts invocations, so a test can assert the
/// gate was (or was NOT) reached.
struct StubGate {
    decision: Option<Uuid>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl RecognitionApprovalGate for StubGate {
    async fn gate(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _scope: &toolkit_db::secure::AccessScope,
        _intent: bss_ledger::domain::approval::intent::ApprovalIntent,
        _facts: bss_ledger::domain::approval::policy::OperationFacts,
        _reason_code: String,
    ) -> Result<Option<Uuid>, bss_ledger::domain::error::DomainError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.decision)
    }
}

/// An authenticated router wired WITH a dual-control gate (vs `authed_router`'s
/// `approval = None`). Returns the router + the gate's call counter so a test can
/// assert whether the gate was reached.
fn authed_router_with_gate(
    client: StubClient,
    decision: Option<Uuid>,
) -> (Router, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate: Arc<dyn RecognitionApprovalGate> = Arc::new(StubGate {
        decision,
        calls: Arc::clone(&calls),
    });
    let state = Arc::new(ApiState {
        client: Arc::new(client) as Arc<dyn LedgerClientV1>,
        approval: Some(gate),
        recognition_repo: None,
    });
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = router(state, &openapi)
        .layer(axum::Extension(allow_enforcer()))
        .layer(axum::Extension(authed_ctx()));
    (router, calls)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn trigger_recognition_run_in_order_returns_200() {
    let status = post_json(
        authed_router(StubClient::default()),
        "/bss-ledger/v1/recognition-runs",
        serde_json::json!({ "tenant_id": SUBJECT_TENANT, "period_id": "202606", "run_id": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn trigger_recognition_run_out_of_order_returns_202() {
    let status = post_json(
        authed_router(StubClient {
            run_queued: true,
            schedule_absent: false,
            ..Default::default()
        }),
        "/bss-ledger/v1/recognition-runs",
        serde_json::json!({ "tenant_id": SUBJECT_TENANT, "period_id": "202606", "run_id": null }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn revenue_disaggregation_returns_200() {
    let status = get(
        authed_router(StubClient::default()),
        &format!("/bss-ledger/v1/revenue/disaggregation?tenant_id={SUBJECT_TENANT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn get_recognition_schedule_present_returns_200() {
    let status = get(
        authed_router(StubClient::default()),
        &format!("/bss-ledger/v1/recognition-schedules/SCH-1?tenant_id={SUBJECT_TENANT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn get_recognition_schedule_absent_returns_404() {
    let status = get(
        authed_router(StubClient {
            run_queued: false,
            schedule_absent: true,
            ..Default::default()
        }),
        &format!("/bss-ledger/v1/recognition-schedules/NOPE?tenant_id={SUBJECT_TENANT}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_recognition_schedules_returns_200() {
    let status = get(
        authed_router(StubClient::default()),
        &format!("/bss-ledger/v1/recognition-schedules?tenant_id={SUBJECT_TENANT}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn change_recognition_schedule_cancel_returns_200() {
    let status = post_json(
        authed_router(StubClient::default()),
        "/bss-ledger/v1/recognition-schedules/SCH-1/changes",
        serde_json::json!({
            "tenant_id": SUBJECT_TENANT,
            "change_id": "CHG-1",
            "action": "cancel",
            "treatment": "prospective",
            "new_segments": null
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A `catch_up` treatment is refused with `MODIFICATION_TREATMENT_REVIEW`
/// (400) BEFORE the dual-control gate runs — so an over-threshold catch-up never
/// durably parks a PENDING approval. The gate stub is never called.
#[tokio::test]
async fn change_with_catch_up_treatment_refuses_before_the_gate() {
    let (router, calls) = authed_router_with_gate(StubClient::default(), Some(Uuid::now_v7()));
    let status = post_json(
        router,
        "/bss-ledger/v1/recognition-schedules/SCH-1/changes",
        serde_json::json!({
            "tenant_id": SUBJECT_TENANT,
            "change_id": "CHG-1",
            "action": "cancel",
            "treatment": "catch_up",
            "new_segments": null
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "catch_up ⇒ treatment review (invalid_argument), not a gate 409"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the treatment gate MUST refuse before the dual-control gate (no PENDING parked)"
    );
}

/// #3 (VHP-1855): a change against a non-ACTIVE (REPLACED) schedule — the shape of an
/// idempotent replay of an already-applied change — skips the gate and falls through
/// to the change-service (200), instead of recomputing the stale remainder off the
/// REPLACED row and raising a spurious 409.
#[tokio::test]
async fn change_against_replaced_schedule_skips_the_gate() {
    let client = StubClient {
        schedule_status: Some("REPLACED".to_owned()),
        ..Default::default()
    };
    let (router, calls) = authed_router_with_gate(client, Some(Uuid::now_v7()));
    let status = post_json(
        router,
        "/bss-ledger/v1/recognition-schedules/SCH-1/changes",
        serde_json::json!({
            "tenant_id": SUBJECT_TENANT,
            "change_id": "CHG-1",
            "action": "cancel",
            "treatment": "prospective",
            "new_segments": null
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a REPLACED schedule must not gate; the change-service replays 200"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the gate must be skipped for a non-ACTIVE schedule"
    );
}

/// The complement of the two skip paths: an ACTIVE schedule whose gate returns an
/// approval id routes to the dual-control queue — the handler 409s and the gate IS
/// reached (proves the wiring is live, not dead like `approval = None`).
#[tokio::test]
async fn change_active_schedule_gate_pending_returns_409() {
    let (router, calls) = authed_router_with_gate(StubClient::default(), Some(Uuid::now_v7()));
    let status = post_json(
        router,
        "/bss-ledger/v1/recognition-schedules/SCH-1/changes",
        serde_json::json!({
            "tenant_id": SUBJECT_TENANT,
            "change_id": "CHG-1",
            "action": "cancel",
            "treatment": "prospective",
            "new_segments": null
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an ACTIVE change the gate parks ⇒ 409 DUAL_CONTROL_REQUIRED"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the gate is reached on the ACTIVE path"
    );
}

#[tokio::test]
async fn trigger_recognition_run_unauthenticated_returns_401() {
    // No `Extension(SecurityContext)` layer ⇒ require_authenticated fails (401).
    let router = router_with(StubClient::default(), allow_enforcer());
    let status = post_json(
        router,
        "/bss-ledger/v1/recognition-runs",
        serde_json::json!({ "tenant_id": SUBJECT_TENANT, "period_id": "202606", "run_id": null }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn trigger_recognition_run_pdp_deny_returns_403() {
    let router =
        router_with(StubClient::default(), deny_enforcer()).layer(axum::Extension(authed_ctx()));
    let status = post_json(
        router,
        "/bss-ledger/v1/recognition-runs",
        serde_json::json!({ "tenant_id": SUBJECT_TENANT, "period_id": "202606", "run_id": null }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

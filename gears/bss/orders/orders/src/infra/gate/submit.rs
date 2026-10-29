//! The submit service (S3-12; 03 §3.6; D-188, D-192, D-194, M2-3): recover a reserved attempt or
//! assess the draft, refuse a business failure through the ordinary engine path without
//! reserving anything, reserve the durable commercial attempt with its frozen per-line Pricing
//! requests (phase A), command Pricing for every line under the frozen keys outside any lock
//! (phase B) and commit the reserved candidate through the engine (phase C, [`super::commit`]).
//!
//! Every request is first reported to the budget's gate signals as a `submit` request
//! (`orders_gate_requests_total`, 03 §3.8), before anything below, the throttle included.
//!
//! Flow, in order:
//! 1. the per-caller limiter (`gate.submit_per_caller_per_minute`), keyed by the authenticated
//!    subject, before any read or port call;
//! 2. the [`TransitionRequest`] (`submit`, the caller's expected version and draft revision, the
//!    empty fingerprint document `{}`: the draft revision is a fingerprint input of its own); it
//!    reads nothing from the order;
//! 3. recovery before any draft read or assessment (D-188 step 1, E2,
//!    [`Engine::recover_commercial_attempt`]): the engine authorizes the target for `submit`
//!    first (denial evidence, the non-disclosing `order-not-found` for a hidden target), then
//!    probes the caller's own key. A settled key is replayed exactly as stored after the
//!    disclosure recheck, whatever state the order is in now (a committed order is no longer a
//!    draft); a key under another fingerprint is the engine's `idempotency-mismatch`; a reserved
//!    attempt (a lapsed lease reclaimed) goes straight to phase B with its frozen requests, date
//!    basis and commercial subject, never rebuilt; only a miss continues;
//! 4. the authorized draft read ([`ReadService::get_draft`]: `order × read` over the current
//!    facts, every working line); a denial is the read's own answer and no port is called. It
//!    runs only for a key with no record: the read composes drafts only, so before the probe it
//!    could not answer a committed same-key retry. An order that is no longer a draft (a new key
//!    on a committed order) skips the gate: the engine settles the state table's
//!    `not-admissible` (`{"state", "trigger"}`) through [`RefusalPreparation`] before any guard,
//!    no port is called, and a same-key retry replays it. A narrow race: a held draft makes
//!    `get_draft` return `None`; a concurrent resume before the engine lock makes the `NotRun`
//!    admission abort as an integration failure (unsettled, a retry recovers), the same exposure
//!    as the stale-revision `NotRun` path of step 5;
//! 5. a read whose draft revision is not the expected one skips the gate: the engine settles the
//!    precedence refusal (`draft-revision-conflict` / `version-conflict`) through
//!    [`RefusalPreparation`] before any guard, and no port is called;
//! 6. the frozen date basis for the order's resource tenant, then one orchestrated gate run with
//!    [`GateTarget::Submit`] (one batched call per port) on the basis' proposed date;
//! 7. a blocking assessment (`primary` set), a draft without lines, or a passing run whose
//!    evaluation cannot be stored as submit totals (D-216, `evaluation-unavailable`) refuses
//!    through [`Engine::transition`] with [`RefusalPreparation`]: the composite gate fails with
//!    the primary's reason (or its evaluation input is unresolvable), `gate.has-lines` with
//!    `no-lines`, the shared structural guards evaluate over the engine's locked snapshot, and
//!    the engine writes the run's diagnostics (M2-2) and settles the refusal. No durable
//!    execution exists and no candidate is allocated (M2-3);
//! 8. otherwise the attempt inputs are frozen ([`AttemptInputs`]: the prepared draft revision,
//!    the arrangement, the authorization-fact fingerprint, the original principal and proof
//!    reference, the frozen date basis and the seller's service principal, D-194) and
//!    [`Engine::prepare_commercial_attempt`] claims the key and reserves the candidate; the
//!    line requests, the overlap keys and the run's assessment, evaluation, gate report and
//!    outcome vector (D-231) are frozen from that candidate inside the reservation transaction
//!    ([`FrozenLineInputs`]), after being proven buildable before it. A settled same-key replay is returned as settled;
//! 9. phase B: for every line without a recorded result, Pricing's `SellabilityV1::check` as the
//!    attempt's frozen commercial subject (resolved again through the seller's authority and
//!    required to be the same principal, D-194) under the line's frozen key, bounded by the
//!    [`AcceptanceBudget`]; the issued receipt, or a Pricing business refusal, is recorded in
//!    its own short transaction under the aggregate lock and the attempt's fence
//!    ([`Engine::receipt_recorder`]), which also renews the owner's lease. An infrastructure
//!    failure records nothing and answers unavailable (a same-key retry continues); a key or
//!    acceptance conflict from Pricing is a broken command contract: nothing is recorded and
//!    the answer is an integration failure; a moved fence stops this owner;
//! 10. phase C: [`Engine::transition`] presenting the attempt's owner with the
//!     [`SubmitPreparation`]: the engine commits the candidate, or settles the attempt refused.
//!
//! A refusal never settles over a recovered attempt: step 3 routes every existing attempt to
//! phases B and C, and the engine fails closed on a non-commercial refusal that meets one
//! (E3). Pricing's acceptance command is issued only here; the development double
//! (`stubs::SellabilityStub`) implements it.
use super::authority::{AuthorityError, CommercialAuthority};
use super::budget::{ACCEPTANCE_DEADLINE, AcceptanceBudget, CallError};
use super::commercial::FrozenLineInputs;
use super::commit::{
    BINDING_ADMISSION, DATE_BASIS, HAS_LINES, SubmitPreparation, row_inputs_resolved,
};
use super::failure::{Ids, submit_integration};
use super::orchestrate::{OrchestrateError, Orchestrator, Resolved};
use super::preview::{PreviewDates, gate_line};
use super::writer::GateRun;
use crate::authz::Caller;
use crate::domain::capture::{self, category_verdict, currency_verdict, line_cap_verdict};
use crate::domain::contributions::AggregateContribution;
use crate::domain::dates::{DateBasis, SuppliedDates};
use crate::domain::gate::assess::BasketLine;
use crate::domain::gate::commercial::{
    CommercialError, FrozenLine, FrozenOrder, FrozenRequests, ReceiptResult,
    consumed_item_capacity, contract_defect, decode_line_requests, decode_receipt_result,
    receipt_result_value, recorded_refusal,
};
use crate::domain::gate::orchestrate::{Basket, GateTarget, TenantAxes, line_terms};
use crate::domain::gate::totals::total_rows;
use crate::domain::guards::{GATE_COMPOSITE, InputPort};
use crate::domain::idempotency::{FingerprintAxes, authorization_fact_fingerprint};
use crate::domain::transition::{GuardBindings, GuardSubject, GuardVerdict, InputState, Inputs};
use crate::infra::capture::{Settled, fingerprint_document};
use crate::infra::dates::proposed_date;
use crate::infra::engine::{
    Engine, Preparation, PreparationView, PrepareError, Prepared, PreparedExecution,
    ReceiptOutcome, Recovery, TransitionRequest,
};
use crate::infra::read::ReadService;
use crate::infra::storage::entity::commercial_attempt;
use crate::infra::storage::repo::idempotency::{AttemptInputs, DurableExecution, Frozen};
use crate::infra::throttle::{PerCallerLimiter, Rejection, rejection_error};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{DraftLine, OrderView};
use bss_orders_sdk::catalog::{Reason, Trigger};
use bss_orders_sdk::gate_report::GateReport;
use bss_orders_sdk::models::{CallMeta, DraftRevision};
use bss_pricing_sdk::acceptance::{CommandMeta, SellabilityV1};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// The public catalog operation submit fingerprints under.
const OPERATION: &str = "submit";

/// The submit service (module doc).
pub struct SubmitService {
    engine: Arc<ArcSwapOption<Engine>>,
    reads: Arc<ReadService>,
    orchestrator: Orchestrator,
    dates: Arc<dyn PreviewDates>,
    authority: Arc<dyn CommercialAuthority>,
    limiter: Arc<PerCallerLimiter>,
    line_cap: usize,
    /// Pricing's sellability port, bound directly (tests, the development stub).
    acceptance: Option<Arc<dyn SellabilityV1>>,
    /// The hub Pricing publishes `SellabilityV1` in, read at each phase B when no port is bound
    /// directly (Pricing is optional and binds late, like the catalog reads).
    hub: Option<Arc<toolkit::ClientHub>>,
    /// The phase-B command budget (`gate.acceptance_deadline_ms`).
    budget: AcceptanceBudget,
    /// Whether the missing-port warning was logged (once per process).
    warned: AtomicBool,
}

impl std::fmt::Debug for SubmitService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitService")
            .field("limiter", &self.limiter)
            .field("line_cap", &self.line_cap)
            .field("acceptance", &self.acceptance.is_some())
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// Why a submit produced no settled answer: an Orders error, or the per-caller limiter's refusal
/// (kept apart so REST answers the limiter's own 429 with `Retry-After`, as Preview does).
#[derive(Debug)]
pub enum SubmitError {
    /// Refused, unavailable or failed.
    Orders(OrdersError),
    /// The per-caller limiter refused before any read or port call.
    Throttled(Rejection),
}

impl From<OrdersError> for SubmitError {
    fn from(error: OrdersError) -> Self {
        Self::Orders(error)
    }
}

/// The SDK view: a limiter refusal is the canonical `resource_exhausted` error REST answers.
impl From<SubmitError> for OrdersError {
    fn from(error: SubmitError) -> Self {
        match error {
            SubmitError::Orders(error) => error,
            SubmitError::Throttled(rejection) => Self::Other {
                canonical: rejection_error(rejection),
            },
        }
    }
}

impl SubmitService {
    /// A service over the engine slot, the authorized reads, the gate orchestrator, the date
    /// preparer, the seller's commercial authority (the one the catalog reads present), the
    /// submit limiter and the configured line cap; no sellability port yet and the baseline
    /// acceptance budget.
    #[must_use]
    pub fn new(
        engine: Arc<ArcSwapOption<Engine>>,
        reads: Arc<ReadService>,
        orchestrator: Orchestrator,
        dates: Arc<dyn PreviewDates>,
        authority: Arc<dyn CommercialAuthority>,
        limiter: Arc<PerCallerLimiter>,
        line_cap: usize,
    ) -> Self {
        Self {
            engine,
            reads,
            orchestrator,
            dates,
            authority,
            limiter,
            line_cap,
            acceptance: None,
            hub: None,
            budget: AcceptanceBudget::with_deadline(ACCEPTANCE_DEADLINE),
            warned: AtomicBool::new(false),
        }
    }

    /// Bind Pricing's sellability port directly (phase B).
    #[must_use]
    pub fn with_acceptance(mut self, acceptance: Option<Arc<dyn SellabilityV1>>) -> Self {
        self.acceptance = acceptance;
        self
    }

    /// Read Pricing's sellability port from `hub` at each phase B when none is bound directly.
    #[must_use]
    pub fn with_acceptance_hub(mut self, hub: Arc<toolkit::ClientHub>) -> Self {
        self.hub = Some(hub);
        self
    }

    /// The phase-B command budget.
    #[must_use]
    pub fn with_acceptance_budget(mut self, budget: AcceptanceBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Submit `order_id` at `expected_draft_revision` as `caller` (module doc).
    ///
    /// # Errors
    /// `resource_exhausted` from the limiter; the read's denial and not-found answers; unsettled
    /// engine refusals; `Unavailable` when the engine is not bound, the date basis, the gate run,
    /// the seller's principal or Pricing's sellability port cannot be had, a Pricing command
    /// fails without a business answer or this owner's fence moved (a same-key retry continues);
    /// `Integration` when a passing run's answers cannot be frozen into Pricing requests or a
    /// frozen attempt, receipt or pin does not verify. A business refusal is settled and
    /// returned as `Ok`.
    pub async fn submit(
        &self,
        caller: &Caller,
        order_id: Uuid,
        expected_draft_revision: DraftRevision,
        meta: &CallMeta,
        cancel: &CancellationToken,
    ) -> Result<Settled, SubmitError> {
        // Every request counts, before the throttle (03 §3.8).
        self.orchestrator
            .budget()
            .signals()
            .request(super::signals::Flow::Submit);
        // 1. The limiter, before any read or port call.
        self.limiter
            .admit(caller.ctx().subject_id())
            .map_err(SubmitError::Throttled)?;
        Ok(
            Box::pin(self.submit_admitted(caller, order_id, expected_draft_revision, meta, cancel))
                .await?,
        )
    }

    /// Everything after the limiter (steps 2 onward).
    async fn submit_admitted(
        &self,
        caller: &Caller,
        order_id: Uuid,
        expected_draft_revision: DraftRevision,
        meta: &CallMeta,
        cancel: &CancellationToken,
    ) -> Result<Settled, OrdersError> {
        let engine = self.engine.load_full().ok_or(OrdersError::Unavailable)?;
        // 2. The request: built from the call alone, so the probe needs no order read.
        let request = TransitionRequest {
            order_id,
            trigger: Trigger::Submit,
            operation: OPERATION,
            expected_version: meta.expected_version,
            expected_draft_revision: Some(expected_draft_revision),
            // E4: the draft revision is already a fingerprint input (`DraftRevisionInput`), so the
            // contribution is empty; capture's draft writes never repeat it in theirs either.
            document: fingerprint_document(&json!({}))?,
            proposed: None,
            caller_reason: None,
            idempotency_key: meta.idempotency_key.clone(),
            correlation_id: meta.correlation_id,
            execution: None,
        };
        // 3. Recovery before any draft read or assessment (D-188 step 1, E2): the engine
        // authorizes the target, then replays a settled key, refuses a mismatched one or resumes
        // a reserved attempt.
        match engine.recover_commercial_attempt(caller, &request).await? {
            Recovery::Replay(outcome) => return Ok(Settled::from_outcome(*outcome)),
            Recovery::Existing(existing) => {
                let (execution, frozen) = *existing;
                return self
                    .continue_attempt(caller, &engine, request, &execution, frozen, cancel)
                    .await;
            }
            Recovery::Miss => {}
        }
        // 4. The authorized draft read: every working line, under `order × read`. An order that is
        // no longer a draft (a new key on a committed order) is the engine's settled
        // `not-admissible`, decided from the state table before any guard or port call.
        let Some(view) = self.reads.get_draft(caller, order_id).await? else {
            let preparation = RefusalPreparation {
                gate: RefusalGate::NotRun,
                line_cap: self.line_cap,
            };
            let outcome = engine.transition(caller, request, &preparation).await?;
            return Ok(Settled::from_outcome(outcome));
        };
        // 5. A stale draft revision: the engine's precedence refusal, no gate run. An empty
        // draft likewise makes no port call: the engine's `gate.has-lines` guard settles
        // `no-lines` (the other guards still evaluate over the locked snapshot).
        if view.draft_revision != Some(expected_draft_revision) || view.lines.is_empty() {
            let preparation = RefusalPreparation {
                gate: RefusalGate::NotRun,
                line_cap: self.line_cap,
            };
            let outcome = engine.transition(caller, request, &preparation).await?;
            return Ok(Settled::from_outcome(outcome));
        }

        // The read detects an over-cap draft before any date or catalog port or reservation.
        // The refusal preparation rechecks the count under the engine's aggregate lock.
        if consumed_item_capacity(view.lines.iter().map(|line| line.selected_items.len()))
            != GuardVerdict::Pass
        {
            let preparation = RefusalPreparation {
                gate: RefusalGate::Capacity,
                line_cap: self.line_cap,
            };
            let outcome = engine.transition(caller, request, &preparation).await?;
            return Ok(Settled::from_outcome(outcome));
        }

        // 6. The frozen date basis, then one gate run.
        let axes = TenantAxes {
            resource: view.order.resource_tenant_id,
            payer: view.order.payer_tenant_id,
            seller: view.order.seller_tenant_id,
        };
        let supplied = view
            .lines
            .iter()
            .map(|l| (l.line_id, supplied(l)))
            .collect();
        let basis = self
            .dates
            .prepare(axes.resource, supplied)
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "orders.gate.submit",
                    order_id = %order_id,
                    correlation_id = ?meta.correlation_id,
                    error = %error,
                    "bss-orders: submit date basis could not be prepared"
                );
                OrdersError::Unavailable
            })?;
        let run_id = Uuid::now_v7();
        let basket = basket(run_id, order_id, axes, &view, &basis);
        let resolved = self
            .orchestrator
            .run(caller.ctx(), &basket, cancel)
            .await
            .map_err(|error| run_failure(order_id, &error))?;
        let evaluated_at = basket.assessed_at;
        let assessment = &resolved.assessment;
        let report = assessment.report(run_id);
        let diagnostics = Arc::new(GateRun {
            overlap_keys: resolved
                .keys
                .iter()
                .map(|(id, key)| {
                    (
                        *id,
                        key.iter().map(|k| k.overlap_scope_key.clone()).collect(),
                    )
                })
                .collect(),
            run_id,
            resource_tenant_id: axes.resource,
            seller_tenant_id: axes.seller,
            payer_tenant_id: axes.payer,
            correlation_id: meta.correlation_id,
            evaluated_at,
            vector: assessment.vector.clone(),
        });
        let basis = Arc::new(basis);

        // 7. A business refusal settles through the engine without reserving anything.
        let line_ids: Vec<Uuid> = view.lines.iter().map(|l| l.line_id).collect();
        if let Some(gate) = refusal_gate(
            order_id,
            &resolved,
            &line_ids,
            &report,
            &diagnostics,
            &basis,
        )? {
            let preparation = RefusalPreparation {
                gate,
                line_cap: self.line_cap,
            };
            let outcome = engine.transition(caller, request, &preparation).await?;
            return Ok(Settled::from_outcome(outcome));
        }

        // 8. Freeze the attempt inputs and reserve the candidate.
        let policy = resolved.policy.as_ref().ok_or_else(|| {
            integration(
                Ids::order(order_id),
                "a passing run without an acceptance policy",
            )
        })?;
        let evaluation = resolved.evaluation.clone().ok_or_else(|| {
            integration(Ids::order(order_id), "a passing run without an evaluation")
        })?;
        let evaluation_request = resolved.evaluation_request.clone().ok_or_else(|| {
            integration(
                Ids::order(order_id),
                "a passing run without the actual evaluation request",
            )
        })?;
        let order = FrozenOrder {
            assessment_id: run_id,
            assessed_at: evaluated_at,
            resolve_date: basket.resolve_date,
            evaluation_request: Some(evaluation_request),
            evaluation,
            report,
            correlation_id: meta.correlation_id,
            vector: assessment.vector.clone(),
        };
        let frozen = FrozenLineInputs::capture(
            order_id,
            axes,
            &basket.lines,
            &resolved,
            &basis,
            policy,
            order,
        );
        // Proven buildable before the reservation; only the order version, which every positive
        // candidate encodes alike, differs from the requests the reservation freezes.
        let probe = i32::from(view.version.version).saturating_add(1);
        frozen
            .line_requests(probe)
            .map_err(|error| commercial_failure(&error))?;
        let inputs = self
            .attempt_inputs(caller, order_id, axes, &view, &basis)
            .await?;
        let lines = move |candidate| {
            frozen.line_requests(candidate).map_err(|error| {
                commercial_failure(&error);
                PrepareError::Integration
            })
        };
        match engine
            .prepare_commercial_attempt(caller, &request, inputs, lines)
            .await?
        {
            PreparedExecution::Settled(outcome) => Ok(Settled::from_outcome(*outcome)),
            PreparedExecution::Execution(execution) => {
                let (execution, frozen) = *execution;
                self.continue_attempt(caller, &engine, request, &execution, frozen, cancel)
                    .await
            }
        }
    }

    /// Phases B and C of a reserved or recovered attempt (module doc, steps 9–10): both read
    /// only the attempt's frozen inputs, its gate report and outcome vector included (D-231).
    async fn continue_attempt(
        &self,
        caller: &Caller,
        engine: &Engine,
        request: TransitionRequest,
        execution: &DurableExecution,
        frozen: Frozen,
        cancel: &CancellationToken,
    ) -> Result<Settled, OrdersError> {
        let Frozen::Commercial(attempt) = frozen else {
            return Err(integration(
                Ids::order(request.order_id),
                "a submit key linked to a receiver control",
            ));
        };
        let ids = Ids::attempt(attempt.order_id, attempt.attempt_id);
        let requests = decode_line_requests(&attempt.line_requests).map_err(|error| {
            integration_with(
                ids,
                "a reserved attempt's frozen requests do not decode",
                &error,
            )
        })?;
        self.acceptance_phase(
            caller, engine, &request, execution, &attempt, &requests, cancel,
        )
        .await?;
        let preparation = SubmitPreparation {
            attempt_id: attempt.attempt_id,
            candidate_version: attempt.candidate_version,
            line_requests: attempt.line_requests.clone(),
            requests,
            line_cap: self.line_cap,
        };
        let outcome = engine
            .transition(
                caller,
                request.with_execution(execution.owner()),
                &preparation,
            )
            .await?;
        Ok(Settled::from_outcome(outcome))
    }

    /// Phase B (module doc, step 9). Returns once every line has a recorded result or one line
    /// recorded Pricing's refusal (the commit then refuses; no later line is commanded).
    #[expect(
        clippy::too_many_arguments,
        reason = "phase B reads the caller, the engine, the request, the owner, the frozen \
                  attempt, its decoded requests and the cancellation"
    )]
    async fn acceptance_phase(
        &self,
        caller: &Caller,
        engine: &Engine,
        request: &TransitionRequest,
        execution: &DurableExecution,
        attempt: &commercial_attempt::Model,
        requests: &FrozenRequests,
        cancel: &CancellationToken,
    ) -> Result<(), OrdersError> {
        let ids = Ids::attempt(request.order_id, attempt.attempt_id);
        let mut pending: Vec<&FrozenLine> = Vec::new();
        for line in &requests.lines {
            match attempt.receipt_results.get(line.line_id.to_string()) {
                Some(value) => {
                    let result = decode_receipt_result(value).map_err(|e| {
                        integration_with(
                            ids.line(line.line_id),
                            "a recorded receipt result does not decode",
                            &e,
                        )
                    })?;
                    if matches!(result, ReceiptResult::Refused(_)) {
                        return Ok(());
                    }
                }
                None => pending.push(line),
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let port = self.acceptance_port()?;
        let service = self.commercial_subject(attempt, requests, cancel).await?;
        let recorder = engine.receipt_recorder(caller, request).await?;
        for line in pending {
            let answer = self
                .budget
                .bounded(line.line_id, &line.key, cancel, || {
                    port.check(
                        &service,
                        line.query.clone(),
                        CommandMeta {
                            idempotency_key: line.key.clone(),
                        },
                    )
                })
                .await;
            let result = line_result(line.line_id, answer)?;
            let refused = matches!(result, ReceiptResult::Refused(_));
            let value = receipt_result_value(line.line_id, &result)
                .map_err(|error| commercial_failure(&error))?;
            match recorder
                .record(execution, &line.line_id.to_string(), value)
                .await?
            {
                ReceiptOutcome::Recorded(_) => {}
                ReceiptOutcome::StaleOwner => {
                    tracing::warn!(
                        target: "orders.gate.submit",
                        order_id = %request.order_id,
                        attempt_id = %attempt.attempt_id,
                        candidate = attempt.candidate_version,
                        "bss-orders: the attempt's fence moved during phase B; this owner stops"
                    );
                    return Err(OrdersError::Unavailable);
                }
            }
            if refused {
                break;
            }
        }
        Ok(())
    }

    /// Pricing's sellability port: bound directly, else published in the hub.
    fn acceptance_port(&self) -> Result<Arc<dyn SellabilityV1>, OrdersError> {
        if let Some(port) = &self.acceptance {
            return Ok(Arc::clone(port));
        }
        if let Some(port) = self
            .hub
            .as_ref()
            .and_then(|hub| hub.get::<dyn SellabilityV1>().ok())
        {
            return Ok(port);
        }
        if !self.warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                target: "orders.gate.submit",
                "bss-orders: Pricing's SellabilityV1 is not published; submit phase B answers \
                 unavailable"
            );
        }
        Err(OrdersError::Unavailable)
    }

    /// The attempt's frozen commercial subject, resolved again through the seller's authority
    /// (D-194). A rotated credential that now yields another principal never rebinds an
    /// uncertain command: phase B stops unavailable.
    async fn commercial_subject(
        &self,
        attempt: &commercial_attempt::Model,
        requests: &FrozenRequests,
        cancel: &CancellationToken,
    ) -> Result<toolkit_security::SecurityContext, OrdersError> {
        let seller = requests
            .lines
            .first()
            .map(|l| l.query.tenant_axes.seller_tenant_id)
            .ok_or_else(|| integration(Ids::order(attempt.order_id), "an attempt without lines"))?;
        // Bounded by `DeadlineAuthority`; a cancelled run stops here too (as the port calls do).
        let service = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(OrdersError::Unavailable),
            r = self.authority.service_context(seller) => {
                r.map_err(|error| authority_failure(seller, &error))?
            }
        };
        if service.subject_id() != attempt.commercial_subject_id
            || service.subject_tenant_id() != attempt.commercial_subject_tenant_id
            || service.subject_type() != Some(attempt.commercial_subject_type.as_str())
        {
            tracing::warn!(
                target: "orders.gate.submit",
                seller_tenant_id = %seller,
                "bss-orders: the seller's commercial principal is not the attempt's frozen \
                 subject (D-194); phase B stops"
            );
            return Err(OrdersError::Unavailable);
        }
        Ok(service)
    }

    /// The immutable D-188 inputs of a passing run (module doc, step 8).
    async fn attempt_inputs(
        &self,
        caller: &Caller,
        order_id: Uuid,
        axes: TenantAxes,
        view: &OrderView,
        basis: &DateBasis,
    ) -> Result<AttemptInputs, OrdersError> {
        let ctx = caller.ctx();
        let service = self
            .authority
            .service_context(axes.seller)
            .await
            .map_err(|error| authority_failure(axes.seller, &error))?;
        let commercial_subject_type = service
            .subject_type()
            .ok_or_else(|| {
                integration(
                    Ids::order(order_id),
                    "the seller's service principal has no subject type",
                )
            })?
            .to_owned();
        let facts = FingerprintAxes {
            seller_tenant_id: axes.seller,
            resource_tenant_id: axes.resource,
            payer_tenant_id: axes.payer,
        };
        let authorization_fact_fingerprint = authorization_fact_fingerprint(order_id, &facts)
            .map_err(|_| integration(Ids::order(order_id), "the authorization-fact fingerprint"))?;
        Ok(AttemptInputs {
            prepared_draft_revision: view.draft_revision.map(i64::from),
            proposed_arrangement: json!({
                "resource_tenant_id": axes.resource,
                "seller_tenant_id": axes.seller,
                "payer_tenant_id": axes.payer,
            }),
            authorization_fact_fingerprint,
            original_principal: json!({
                "subject_id": ctx.subject_id(),
                "subject_tenant_id": ctx.subject_tenant_id(),
                "subject_type": ctx.subject_type(),
            }),
            proof_reference: caller.supplied_proof().map(|p| p.as_str().to_owned()),
            date_policy_basis: basis.frozen(),
            commercial_subject_id: service.subject_id(),
            commercial_subject_type,
            commercial_subject_tenant_id: service.subject_tenant_id(),
        })
    }
}

/// Step 7 (module doc): the refusal a run decides before any reservation. `None` when the run
/// passed, the draft has lines and the evaluation can be stored as submit totals.
fn refusal_gate(
    order_id: Uuid,
    resolved: &Resolved,
    lines: &[Uuid],
    report: &GateReport,
    diagnostics: &Arc<GateRun>,
    basis: &Arc<DateBasis>,
) -> Result<Option<RefusalGate>, OrdersError> {
    let assessment = &resolved.assessment;
    let storable = match (&assessment.primary, &resolved.evaluation) {
        (Some(_), _) => None,
        _ if lines.is_empty() => None,
        (None, Some(evaluation)) => {
            let market = resolved.market.as_ref().ok_or_else(|| {
                integration(Ids::order(order_id), "a passing run without a market")
            })?;
            match total_rows(evaluation, lines, &market.currency) {
                Ok(_) => Some(true),
                Err(error) if error.is_contract_defect() => {
                    return Err(integration_with(
                        Ids::order(order_id),
                        "the evaluation is not in the market currency (D-216)",
                        &error,
                    ));
                }
                Err(error) => {
                    tracing::warn!(
                        target: "orders.gate.submit",
                        order_id = %order_id,
                        error = %error,
                        "bss-orders: the evaluation cannot be stored as submit totals"
                    );
                    Some(false)
                }
            }
        }
        (None, None) => Some(false),
    };
    if storable == Some(true) {
        return Ok(None);
    }
    let primary = match &assessment.primary {
        Some(outcome) => Some(outcome.reason.ok_or_else(|| {
            integration(Ids::order(order_id), "a blocking outcome without a reason")
        })?),
        None => None,
    };
    let mut inputs = assessment.inputs.clone();
    if storable == Some(false) {
        // D-216: the passing run's evaluation has no storable form (a non-terminating
        // fraction, a display finer than the currency scale, a missing declaration), so the
        // evaluation input is unusable for submit.
        tracing::warn!(
            target: "orders.gate.submit",
            "bss-orders: the evaluation cannot be stored as submit totals; refused \
             evaluation-unavailable before reserving"
        );
        inputs = inputs.with(
            InputPort::Evaluation,
            InputState::Unresolvable(Reason::EvaluationUnavailable),
        );
    }
    Ok(Some(RefusalGate::Assessed {
        inputs,
        primary,
        report: report.clone(),
        diagnostics: Arc::clone(diagnostics),
        basis: Arc::clone(basis),
    }))
}

/// The gate basket of a submit: every working line, in read order, on the basis' proposed date.
fn basket(
    run_id: Uuid,
    order_id: Uuid,
    axes: TenantAxes,
    view: &OrderView,
    basis: &DateBasis,
) -> Basket {
    Basket {
        assessed_at: OffsetDateTime::now_utc(),
        target: GateTarget::Submit { order_id },
        assessment_id: run_id,
        axes,
        contract_id: view.order.contract_id,
        lines: view.lines.iter().map(draft_line).collect(),
        terms: view
            .lines
            .iter()
            .map(|l| {
                (
                    l.line_id,
                    line_terms(l.term_duration.as_ref(), l.billing_cycle),
                )
            })
            .collect::<BTreeMap<_, _>>(),
        resolve_date: proposed_date(basis),
        dates: basis.clone(),
    }
}

fn draft_line(l: &DraftLine) -> BasketLine {
    gate_line(
        l.line_id,
        l.plan_id,
        l.plan_revision_id,
        &l.selected_items,
        l.currency.as_str(),
    )
}

/// The line's authored dates.
fn supplied(line: &DraftLine) -> SuppliedDates {
    SuppliedDates {
        contract_effective: line.contract_effective_date.map(|d| d.0),
        service_activation: line.service_activation_date.map(|d| d.0),
        acceptance_due: line.acceptance_due_date.map(|d| d.0),
    }
}

fn integration(ids: Ids, what: &'static str) -> OrdersError {
    submit_integration(what, ids, None);
    OrdersError::Integration
}

/// [`integration`] keeping the cause's `Display` (never stored content, see [`super::failure`]).
fn integration_with(ids: Ids, what: &'static str, cause: &dyn std::fmt::Display) -> OrdersError {
    submit_integration(what, ids, Some(cause));
    OrdersError::Integration
}

/// A run that produced no assessment: cancelled, or a local integration failure.
fn run_failure(order_id: Uuid, error: &OrchestrateError) -> OrdersError {
    tracing::warn!(
        target: "orders.gate.submit",
        order_id = %order_id,
        error = %error,
        "bss-orders: submit gate run produced no assessment"
    );
    OrdersError::Unavailable
}

/// A passing run whose answers cannot be frozen: a producer contract gap, never defaulted.
fn commercial_failure(error: &CommercialError) -> OrdersError {
    tracing::error!(
        target: "orders.gate.submit",
        error = %error,
        "bss-orders: a passing run's answers cannot be frozen into Pricing requests"
    );
    OrdersError::Integration
}

/// The seller's service principal is not available: nothing more is commanded; the caller
/// retries.
fn authority_failure(seller: Uuid, error: &AuthorityError) -> OrdersError {
    tracing::warn!(
        target: "orders.gate.submit",
        seller_tenant_id = %seller,
        error = %error,
        "bss-orders: no commercial service principal for the seller"
    );
    OrdersError::Unavailable
}

/// What phase B records for one line's Pricing answer (module doc, step 9): the receipt, or a
/// recorded business refusal. A key or acceptance conflict is a broken command contract and an
/// answerless command is retried later; neither is recorded.
fn line_result(
    line_id: Uuid,
    answer: Result<bss_pricing_sdk::acceptance::AcceptanceReceipt, CallError>,
) -> Result<ReceiptResult, OrdersError> {
    match answer {
        Ok(receipt) => Ok(ReceiptResult::Accepted(Box::new(receipt))),
        Err(CallError::Permanent(error)) => {
            if let Some(reason) = contract_defect(&error) {
                tracing::error!(
                    target: "orders.gate.submit",
                    line_id = %line_id,
                    reason = reason.code(),
                    "bss-orders: Pricing refused a frozen command as a key or acceptance \
                     conflict; nothing recorded"
                );
                return Err(OrdersError::Integration);
            }
            match recorded_refusal(&error) {
                Some(reason) => Ok(ReceiptResult::Refused(reason)),
                None => Err(command_failure(line_id, &CallError::Permanent(error))),
            }
        }
        Err(error) => Err(command_failure(line_id, &error)),
    }
}

/// A Pricing command without a recordable answer (module doc, step 9): nothing is recorded. An
/// invalid argument remaining after terminal term-refusal classification is Orders' own
/// request defect; everything else is retried by a same-key
/// retry under the same frozen key.
fn command_failure(line_id: Uuid, error: &CallError) -> OrdersError {
    let kind = match error {
        CallError::Deadline => "deadline",
        CallError::Transient(_) => "transient",
        CallError::Permanent(CanonicalError::InvalidArgument { .. }) => {
            tracing::error!(
                target: "orders.gate.submit",
                line_id = %line_id,
                "bss-orders: Pricing refused a frozen request as invalid; nothing recorded"
            );
            return OrdersError::Integration;
        }
        CallError::Permanent(CanonicalError::PermissionDenied { .. }) => "permission-denied",
        CallError::Permanent(_) => "permanent",
        CallError::Backpressure => "backpressure",
        CallError::Cancelled => "cancelled",
        CallError::SubrequestCapExceeded => "subrequest-cap",
        CallError::BreakerOpen => "breaker-open",
    };
    tracing::warn!(
        target: "orders.gate.submit",
        line_id = %line_id,
        kind,
        "bss-orders: a Pricing acceptance command did not answer; nothing recorded"
    );
    OrdersError::Unavailable
}

/// What the refusal path presents for the composite gate.
enum RefusalGate {
    /// The draft revision was stale or the order is no longer a draft, so no gate ran: the
    /// engine's precedence refusal (`not-admissible`, `version-conflict`, `precondition-failed`)
    /// settles before any guard is evaluated, and every input and guard is presented resolved and
    /// passing only to satisfy the binding contract (an admission aborts, module doc).
    NotRun,
    /// An over-cap draft detected before any gate or reservation; rechecked under lock.
    Capacity,
    /// A blocking (or line-less, or unstorable-evaluation) assessment.
    Assessed {
        /// The assessment's engine input resolutions.
        inputs: Inputs,
        /// The primary outcome's reason; `None` when only the draft is line-less or the
        /// evaluation is unstorable.
        primary: Option<Reason>,
        /// The authorized gate report the refusal carries.
        report: GateReport,
        /// The run the engine writes on the composite gate's refusal (M2-2).
        diagnostics: Arc<GateRun>,
        /// The run's frozen date basis.
        basis: Arc<DateBasis>,
    },
}

/// Submit's refusal preparation (module doc, steps 4, 5 and 7): row 4's guards, never an admission.
///
/// The aggregate contribution is [`AggregateContribution::None`]: a refusal settles before
/// `admit`, where the contribution is first read (`check_subflow`, then `contributions::plan`),
/// and `check_subflow` aborts a submit admission without a durable execution before it reads the
/// contribution, so no `Version` candidate is ever named here. It never meets a reserved attempt
/// (step 3 routes those to the commit); if a race made it one, the engine fails closed (E3).
struct RefusalPreparation {
    gate: RefusalGate,
    line_cap: usize,
}

#[async_trait]
impl Preparation for RefusalPreparation {
    async fn prepare(&self, view: &PreparationView) -> Result<Prepared, PrepareError> {
        let mut prepared = Prepared::new(AggregateContribution::None);
        let capacity = locked_consumed_item_capacity(view)?;
        let mut guards = structural_guards(view, self.line_cap)
            // The accepted-binding deadline and byte capacity remain the commit's.
            .bind(BINDING_ADMISSION, move |_: &GuardSubject<'_>| capacity);
        match &self.gate {
            RefusalGate::NotRun | RefusalGate::Capacity => {
                prepared.inputs = row_inputs_resolved();
                guards = guards
                    .bind(DATE_BASIS, |_: &GuardSubject<'_>| GuardVerdict::Pass)
                    .bind(GATE_COMPOSITE, |_: &GuardSubject<'_>| GuardVerdict::Pass);
            }
            RefusalGate::Assessed {
                inputs,
                primary,
                report,
                diagnostics,
                basis,
            } => {
                prepared.inputs = inputs.clone();
                let primary = *primary;
                guards = guards.bind(DATE_BASIS, Arc::clone(basis).guard()).bind(
                    GATE_COMPOSITE,
                    move |_: &GuardSubject<'_>| {
                        primary.map_or(GuardVerdict::Pass, GuardVerdict::Fail)
                    },
                );
                // The engine settles the report as its wire value (`Prepared::assessment`).
                prepared.assessment = Some(serde_json::to_value(report).map_err(|_| {
                    submit_integration("gate report", Ids::order(view.facts.order_id), None);
                    PrepareError::Integration
                })?);
                let run: Arc<GateRun> = Arc::clone(diagnostics);
                prepared.diagnostics = Some(run);
            }
        }
        prepared.guards = guards;
        Ok(prepared)
    }
}

/// Row 4's structural guards over the engine's locked draft (`HAS_LINES`, `CATEGORY`, `CURRENCY`,
/// `LINE_CAP`), shared by the refusal and the commit preparation; each adds its own
/// `BINDING_ADMISSION`, `DATE_BASIS` and `GATE_COMPOSITE`.
pub(super) fn structural_guards(view: &PreparationView, line_cap: usize) -> GuardBindings {
    let lines = view.draft_lines.len();
    let currencies: Vec<String> = view
        .draft_lines
        .iter()
        .map(|l| l.currency.clone())
        .collect();
    GuardBindings::new()
        .bind(HAS_LINES, move |_: &GuardSubject<'_>| {
            if lines == 0 {
                GuardVerdict::Fail(Reason::NoLines)
            } else {
                GuardVerdict::Pass
            }
        })
        .bind(capture::CATEGORY, |subject: &GuardSubject<'_>| {
            category_verdict(None, &subject.locked.category)
        })
        .bind(capture::CURRENCY, move |_: &GuardSubject<'_>| {
            currency_verdict(currencies.iter().map(String::as_str))
        })
        .bind(capture::LINE_CAP, move |_: &GuardSubject<'_>| {
            line_cap_verdict(lines, line_cap)
        })
}

/// Count the authored selections in the engine's locked working draft. A malformed stored
/// shape is an integration defect, never an empty selection or an admission.
pub(super) fn locked_consumed_item_capacity(
    view: &PreparationView,
) -> Result<GuardVerdict, PrepareError> {
    let counts = view
        .draft_lines
        .iter()
        .map(|line| line.selected_items.as_array().map(Vec::len))
        .collect::<Option<Vec<_>>>()
        .ok_or(PrepareError::Integration)?;
    Ok(consumed_item_capacity(counts))
}

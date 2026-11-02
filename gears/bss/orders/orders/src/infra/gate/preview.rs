//! The Preview service (S3-11; F03 §2.2, §4.6; D-52, D-125, D-189, D-215): one nonbinding gate
//! run over a basket that creates and mutates no order, persists its outcome vector as standalone
//! diagnostics, and answers the shaped [`PreviewResult`]. Shared by the (development-only) REST
//! route and the local SDK (M2-1).
//!
//! Preview runs no admission guard, so it adds no annotation for the 200 consumed-item
//! per-order submit cap (D-158).
//!
//! Every request is first reported to the budget's gate signals as a `preview` request
//! (`orders_gate_requests_total`, 03 §3.8), before anything below, the throttle included.
//!
//! Flow, in order:
//! 1. boundary validation of the basket (`request-invalid`);
//! 2. the per-caller limiter (`gate.preview_per_caller_per_minute`), keyed by the authenticated
//!    subject; it runs inside the service, so the local SDK is bounded too, and a denied caller cannot reach the PDP unthrottled (submit's order);
//! 3. the same axis-specific authorization as purchase:
//!    [`Pep::authorize_new_arrangement`] with [`Action::OrderPreview`] over the basket's three
//!    axes. A denial maps exactly as the create path maps it (registered reason, sanitized
//!    unavailable, integration), but writes **no** audit row: Preview has no order and is not a
//!    transition, so there is nothing to attach an unresolved-refusal audit entry to. No port is
//!    called before this step;
//! 4. `run_id = Uuid::now_v7()` and a run-local `line_id = Uuid::now_v7()` per line (nothing is
//!    stored under them except the diagnostics); the frozen date basis is prepared once for the
//!    resource tenant before any date-dependent call, and the catalog resolves on its proposed
//!    date;
//! 5. one orchestrated gate run with [`GateTarget::Preview`] (M2-6: one batched call per port);
//! 6. the outcome vector is written on its own short transaction ([`write_preview_run`]); a
//!    failed write answers `Unavailable` and no result (F03 §2.2 step 5);
//! 7. the result is shaped by the pure [`report::preview_result`].
//!
//! Preview never reaches submit's phase B: nothing here names the sellability port or its
//! reservation call (a grep test in `orders/tests/gate_preview_shape.rs` pins it).
use super::orchestrate::{OrchestrateError, Orchestrator};
use super::writer::{RunRecord, RunTarget, write_preview_run};
use crate::authz::{Action, Arrangement, AuthzFailure, Caller, Pep};
use crate::domain::dates::{DateBasis, LineResolution, SuppliedDates};
use crate::domain::gate::assess::BasketLine;
use crate::domain::gate::orchestrate::{Basket, GateTarget, TenantAxes, line_terms};
use crate::domain::gate::pricing_map::{LineCatalogRequest, SelectedItem};
use crate::domain::gate::report::{self, PreviewAnswers, PreviewLineFacts};
use crate::infra::dates::{DatePreparer, PrepareDatesError, proposed_date};
use crate::infra::throttle::{PerCallerLimiter, Rejection, rejection_error};
use async_trait::async_trait;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::AuthoringInvalid;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::preview::{PreviewBasket, PreviewLine, PreviewResult};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_db::Db;
use toolkit_db::secure::{AccessScope, ScopeError};
use uuid::Uuid;

/// Why a Preview produced no result: an Orders error, or the per-caller limiter's refusal (kept
/// apart so REST answers the limiter's own 429 with `Retry-After`).
#[derive(Debug)]
pub enum PreviewError {
    /// Refused, unavailable or failed.
    Orders(OrdersError),
    /// The per-caller limiter refused before the run.
    Throttled(Rejection),
}

impl From<OrdersError> for PreviewError {
    fn from(error: OrdersError) -> Self {
        Self::Orders(error)
    }
}

/// The SDK view: a limiter refusal is the canonical `resource_exhausted` error REST answers.
impl From<PreviewError> for OrdersError {
    fn from(error: PreviewError) -> Self {
        match error {
            PreviewError::Orders(error) => error,
            PreviewError::Throttled(rejection) => Self::Other {
                canonical: rejection_error(rejection),
            },
        }
    }
}

/// The run's frozen date basis (02 §3.2 steps 1–3); [`DatePreparer`] in production.
#[async_trait]
pub trait PreviewDates: Send + Sync {
    /// Snapshot the policy once, choose the proposed UTC date and resolve every line.
    ///
    /// # Errors
    /// The preparation failed (policy or store).
    async fn prepare(
        &self,
        resource_tenant_id: Uuid,
        lines: Vec<(Uuid, SuppliedDates)>,
    ) -> Result<DateBasis, PrepareDatesError>;
}

#[async_trait]
impl PreviewDates for DatePreparer {
    async fn prepare(
        &self,
        resource_tenant_id: Uuid,
        lines: Vec<(Uuid, SuppliedDates)>,
    ) -> Result<DateBasis, PrepareDatesError> {
        DatePreparer::prepare(self, resource_tenant_id, lines).await
    }
}

/// The standalone diagnostics write of one Preview run; [`StoragePreviewRuns`] in production.
#[async_trait]
pub trait PreviewRuns: Send + Sync {
    /// Commit every outcome of `run` on one short transaction and return its `run_id`.
    ///
    /// # Errors
    /// Nothing was persisted.
    async fn write(&self, scope: &AccessScope, run: &RunRecord<'_>) -> Result<Uuid, ScopeError>;
}

/// [`write_preview_run`] over the gear's database.
#[derive(Clone)]
pub struct StoragePreviewRuns {
    db: Db,
}

impl StoragePreviewRuns {
    /// A writer over `db`.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PreviewRuns for StoragePreviewRuns {
    async fn write(&self, scope: &AccessScope, run: &RunRecord<'_>) -> Result<Uuid, ScopeError> {
        write_preview_run(&self.db, scope, run).await
    }
}

/// The Preview service (module doc).
pub struct PreviewService {
    pep: Pep,
    orchestrator: Orchestrator,
    dates: Arc<dyn PreviewDates>,
    runs: Arc<dyn PreviewRuns>,
    limiter: Arc<PerCallerLimiter>,
}

impl std::fmt::Debug for PreviewService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewService")
            .field("limiter", &self.limiter)
            .finish_non_exhaustive()
    }
}

/// One prepared basket line: its run-local identity and the authored inputs the run reads.
struct PreparedLine {
    line_id: Uuid,
    line: PreviewLine,
}

impl PreviewService {
    /// A service over the shared PEP, the gate orchestrator, the date preparer, the diagnostics
    /// writer and the Preview limiter.
    #[must_use]
    pub fn new(
        pep: Pep,
        orchestrator: Orchestrator,
        dates: Arc<dyn PreviewDates>,
        runs: Arc<dyn PreviewRuns>,
        limiter: Arc<PerCallerLimiter>,
    ) -> Self {
        Self {
            pep,
            orchestrator,
            dates,
            runs,
            limiter,
        }
    }

    /// Preview `basket` as `caller` (module doc), for the SDK.
    ///
    /// # Errors
    /// `request-invalid`; the untargeted denial reasons; `resource_exhausted` from the limiter;
    /// `Unavailable` when the authorization, the date basis, the run or the diagnostics write
    /// cannot complete; `Integration` for an authorization integration failure.
    pub async fn preview(
        &self,
        caller: &Caller,
        basket: PreviewBasket,
        correlation_id: Option<Uuid>,
        cancel: &CancellationToken,
    ) -> Result<PreviewResult, OrdersError> {
        self.run(caller, basket, correlation_id, cancel)
            .await
            .map_err(OrdersError::from)
    }

    /// [`Self::preview`], keeping the limiter's refusal typed (REST answers it with its own
    /// `Retry-After`).
    ///
    /// # Errors
    /// As [`Self::preview`].
    pub async fn run(
        &self,
        caller: &Caller,
        basket: PreviewBasket,
        correlation_id: Option<Uuid>,
        cancel: &CancellationToken,
    ) -> Result<PreviewResult, PreviewError> {
        // Every request counts, before validation and the throttle (03 §3.8).
        self.orchestrator
            .budget()
            .signals()
            .request(super::signals::Flow::Preview);
        basket.validate().map_err(|AuthoringInvalid(member)| {
            tracing::debug!(
                target: "orders.gate.preview",
                member,
                "Preview basket refused request-invalid"
            );
            OrdersError::Refused(Reason::RequestInvalid)
        })?;
        let arrangement = Arrangement {
            resource_tenant_id: basket.resource_tenant_id,
            seller_tenant_id: basket.seller_tenant_id,
            payer_tenant_id: basket.payer_tenant_id,
        };
        // Throttle first (submit's order): a denied caller cannot reach the PDP unthrottled.
        self.limiter
            .admit(caller.ctx().subject_id())
            .map_err(PreviewError::Throttled)?;
        // Step 2: no audit row on denial (module doc); the mapping is the create path's.
        self.pep
            .authorize_new_arrangement(caller, Action::OrderPreview, arrangement)
            .await
            .map_err(|failure| denial(&failure))?;

        let run_id = Uuid::now_v7();
        let lines: Vec<PreparedLine> = basket
            .lines
            .into_iter()
            .map(|line| PreparedLine {
                line_id: Uuid::now_v7(),
                line,
            })
            .collect();
        let supplied = lines
            .iter()
            .map(|l| (l.line_id, supplied(&l.line)))
            .collect();
        let basis = self
            .dates
            .prepare(arrangement.resource_tenant_id, supplied)
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "orders.gate.preview",
                    error = %error,
                    "bss-orders: Preview date basis could not be prepared"
                );
                OrdersError::Unavailable
            })?;
        let proposed = proposed_date(&basis);
        let facts: Vec<PreviewLineFacts> = lines.iter().map(|l| facts(l, &basis)).collect();
        let assessed_at = OffsetDateTime::now_utc();
        let gate_basket = Basket {
            assessed_at,
            target: GateTarget::Preview,
            assessment_id: run_id,
            axes: TenantAxes {
                resource: arrangement.resource_tenant_id,
                payer: arrangement.payer_tenant_id,
                seller: arrangement.seller_tenant_id,
            },
            contract_id: basket.contract_id,
            lines: lines.iter().map(basket_line).collect(),
            terms: lines
                .iter()
                .map(|l| {
                    (
                        l.line_id,
                        line_terms(l.line.term_duration.as_ref(), l.line.billing_cycle),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            resolve_date: proposed,
            dates: basis,
        };

        let resolved = self
            .orchestrator
            .run(caller.ctx(), &gate_basket, cancel)
            .await
            .map_err(|error| run_failure(run_id, correlation_id, &error))?;

        let subject_id = caller.ctx().subject_id().to_string();
        let subject_tenant_id = caller.ctx().subject_tenant_id();
        let record = RunRecord {
            run_id,
            subject_tenant_id,
            subject_id: &subject_id,
            resource_tenant_id: arrangement.resource_tenant_id,
            seller_tenant_id: arrangement.seller_tenant_id,
            payer_tenant_id: arrangement.payer_tenant_id,
            correlation_id,
            target: RunTarget::Preview,
            evaluated_at: assessed_at,
            vector: &resolved.assessment.vector,
        };
        // F03 §2.2 step 5: no committed diagnostics, no result.
        self.runs
            .write(&AccessScope::for_tenant(subject_tenant_id), &record)
            .await
            .map_err(|error| {
                tracing::warn!(
                    target: "orders.gate.preview",
                    error = %error,
                    "bss-orders: Preview diagnostics were not persisted; no result returned"
                );
                OrdersError::Unavailable
            })?;

        Ok(report::preview_result(
            run_id,
            &resolved.assessment,
            PreviewAnswers {
                evaluation: resolved.evaluation.as_ref(),
                tax: resolved.tax.as_ref(),
            },
            &facts,
            proposed,
        ))
    }
}

/// The create path's denial mapping (`Engine::create` → `record_denial`), without its audit row.
fn denial(failure: &AuthzFailure) -> PreviewError {
    PreviewError::Orders(match failure {
        AuthzFailure::Refused { reason, .. } => OrdersError::Refused(*reason),
        AuthzFailure::Unavailable => OrdersError::Unavailable,
        AuthzFailure::Integration => OrdersError::Integration,
    })
}

/// A run that produced no assessment: cancelled, or a local integration failure (the D-180
/// count). Neither is a business answer, so the caller may retry.
pub(super) fn run_failure(
    run_id: Uuid,
    correlation_id: Option<Uuid>,
    error: &OrchestrateError,
) -> PreviewError {
    tracing::warn!(
        target: "orders.gate.preview",
        run_id = %run_id,
        correlation_id = ?correlation_id,
        error = %error,
        "bss-orders: Preview gate run produced no assessment"
    );
    PreviewError::Orders(OrdersError::Unavailable)
}

/// The line's authored dates (Preview's `supplied_from_draft`).
fn supplied(line: &PreviewLine) -> SuppliedDates {
    SuppliedDates {
        contract_effective: line.contract_effective_date.map(|d| d.0),
        service_activation: line.service_activation_date.map(|d| d.0),
        acceptance_due: line.acceptance_due_date.map(|d| d.0),
    }
}

/// What the shaping reads of a line: the D-125 inputs and the resolved activation date.
fn facts(l: &PreparedLine, basis: &DateBasis) -> PreviewLineFacts {
    let service_activation = match basis.lines.get(&l.line_id) {
        Some(LineResolution::Resolved { dates }) => Some(dates.service_activation.date()),
        Some(LineResolution::Invalid { .. }) | None => None,
    };
    PreviewLineFacts {
        line_id: l.line_id,
        term_and_cycle_authored: l.line.term_duration.is_some()
            && l.line.billing_cycle.is_some()
            && matches!(
                line_terms(l.line.term_duration.as_ref(), l.line.billing_cycle).intent,
                crate::domain::gate::orchestrate::LineTermIntent::Stated(_)
            ),
        service_activation,
    }
}

/// The gate's view of a Preview line.
fn basket_line(l: &PreparedLine) -> BasketLine {
    let line = &l.line;
    gate_line(
        l.line_id,
        line.plan_id,
        line.plan_revision_id,
        &line.selected_items,
        line.currency.as_str(),
    )
}

/// The gate's view of an authored line (Preview's basket line, submit's draft line). An absent
/// quantity is sent as empty text, which the quantity floor fails closed; it is never defaulted.
pub(in crate::infra) fn gate_line(
    line_id: Uuid,
    plan_id: Uuid,
    plan_revision_id: Uuid,
    items: &[bss_orders_sdk::authoring::SelectedItem],
    currency: &str,
) -> BasketLine {
    BasketLine {
        line_id,
        catalog: LineCatalogRequest {
            line_id,
            plan_id,
            revision_id: plan_revision_id,
            items: items
                .iter()
                .map(|i| SelectedItem {
                    item_id: i.item_id,
                    quantity: i
                        .quantity
                        .as_ref()
                        .map(|q| bss_money::canonical_decimal(q.value()))
                        .unwrap_or_default(),
                    selected_dim_value: i.selected_dim_value.clone(),
                })
                .collect(),
        },
        currency: Some(currency.to_owned()),
        contract_ref: None,
    }
}

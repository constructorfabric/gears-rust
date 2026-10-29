//! The in-process SDK adapter (08 §3.5): [`LocalOrdersClient`] implements [`OrdersV1`] over the
//! same capture and read services and the same caller rules as the REST handlers, and is
//! published in `ClientHub` by the gear. It is an API-layer adapter beside [`super::rest`], as
//! the Ledger gear's `api/local_client.rs` is: it decodes a settled refusal through the REST error
//! gear's contract-error decode ([`RefusalEnvelope`]), the one owner of the stored Problem shape
//! (D-219), and builds its caller through [`Caller::from_meta`], shared with REST.
use std::sync::Arc;

use async_trait::async_trait;
use bss_orders_sdk::authoring::{
    AddLine, CreateMeta, CreateOrder, HeaderPatch, LinePatch, OrderView,
};
use bss_orders_sdk::models::{CallMeta, DraftRevision, TransitionResult};
use bss_orders_sdk::preview::{PreviewBasket, PreviewResult};
use bss_orders_sdk::reads::ReadMeta;
use bss_orders_sdk::{OrdersError, OrdersV1};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::rest::error::RefusalEnvelope;
use crate::authz::Caller;
use crate::infra::capture::{CaptureService, Settled, SettledOutcome, WriteMeta};
use crate::infra::diagnostics::serde_category;
use crate::infra::engine::errors::not_delivered;
use crate::infra::gate::preview::PreviewService;
use crate::infra::gate::submit::SubmitService;
use crate::infra::read::ReadService;
use tokio_util::sync::CancellationToken;

/// The local SDK client: the same services and caller adapter as REST (08 §3.5), for draft
/// authoring (S2-09) and the authorized draft reads (early S6-01/S6-04).
pub struct LocalOrdersClient {
    service: Arc<CaptureService>,
    reads: Arc<ReadService>,
    /// The Preview service, present only in the development delivery (M2-1: the
    /// `gate-dev-stubs` feature and `gate.dev_stubs = true`); otherwise Preview is undelivered.
    preview: Option<Arc<PreviewService>>,
    /// The submit service, present only in the same development delivery (M2-1); otherwise
    /// submit is undelivered.
    submit: Option<Arc<SubmitService>>,
}
impl LocalOrdersClient {
    /// A client over the shared capture and read services; Preview and submit undelivered.
    #[must_use]
    pub fn new(service: Arc<CaptureService>, reads: Arc<ReadService>) -> Self {
        Self {
            service,
            reads,
            preview: None,
            submit: None,
        }
    }

    /// Deliver Preview through `preview` (the gear passes `Some` only in the development
    /// delivery, M2-1).
    #[must_use]
    pub fn with_preview(mut self, preview: Option<Arc<PreviewService>>) -> Self {
        self.preview = preview;
        self
    }

    /// Deliver submit through `submit` (the gear passes `Some` only in the development
    /// delivery, M2-1).
    #[must_use]
    pub fn with_submit(mut self, submit: Option<Arc<SubmitService>>) -> Self {
        self.submit = submit;
        self
    }
}

/// Decode a settled outcome for the SDK (D-219). The engine's recorded outcome decides success
/// or refusal, never the stored HTTP status. A success decodes the typed body; a refusal decodes
/// the stored Problem through the REST error gear's contract-error decode
/// ([`RefusalEnvelope`], keyed on
/// `(error_domain, error_code)` plus `context.data`) into [`OrdersError::Settled`] with its
/// registered reason and typed diagnostics, the same ones REST returns and replays.
///
/// # Errors
/// The settled refusal; an internal failure for a committed success body that does not decode
/// (logged: the write settled, so a same-key retry replays the same body); an integration
/// failure, logged with the recorded reason, for a stored refusal that does not decode or whose
/// code disagrees with the recorded reason: the refusal stays settled and replayable, it is
/// never reported as a success or as an unknown outcome.
pub fn sdk_result<T: serde::de::DeserializeOwned>(settled: Settled) -> Result<T, OrdersError> {
    let Settled {
        response,
        outcome,
        order_id,
        ..
    } = settled;
    let recorded = match outcome {
        SettledOutcome::Committed => {
            return serde_json::from_value(response.into_body()).map_err(|error| {
                // The serde text quotes the stored body: log its category only (D-44, D-218).
                tracing::error!(
                    target: "orders.capture",
                    order_id = order_id.map(tracing::field::display),
                    serde_category = serde_category(&error),
                    "settled success response does not decode into the SDK type"
                );
                OrdersError::Internal
            });
        }
        SettledOutcome::Refused(recorded) => recorded,
    };
    let decoded = serde_json::from_value::<toolkit_canonical_errors::Problem>(response.into_body())
        .ok()
        .and_then(|problem| RefusalEnvelope::try_from(problem).ok());
    match decoded {
        Some(envelope) if Some(envelope.reason) == recorded => Err(envelope.into()),
        _ => {
            tracing::error!(
                target: "orders.capture",
                recorded_reason = ?recorded,
                decoded_reason = ?decoded.map(|e| e.reason),
                "settled refusal does not decode into its recorded Orders reason"
            );
            Err(OrdersError::Integration)
        }
    }
}

fn sdk_meta(meta: &CallMeta, expected_draft_revision: Option<DraftRevision>) -> WriteMeta {
    WriteMeta {
        call: meta.clone(),
        expected_draft_revision,
    }
}

#[async_trait]
impl OrdersV1 for LocalOrdersClient {
    async fn create(
        &self,
        ctx: &SecurityContext,
        request: CreateOrder,
        meta: CreateMeta,
    ) -> Result<OrderView, OrdersError> {
        // D-202 before any PDP or registry work, as on the REST header path.
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        sdk_result(self.service.create(&caller, request, &meta).await?)
    }
    async fn patch_order(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        patch: HeaderPatch,
        expected_draft_revision: Option<DraftRevision>,
        meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        let meta = sdk_meta(&meta, expected_draft_revision);
        sdk_result(
            self.service
                .patch_order(&caller, order_id, patch, &meta)
                .await?,
        )
    }
    async fn add_line(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        line: AddLine,
        expected_draft_revision: Option<DraftRevision>,
        meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        let meta = sdk_meta(&meta, expected_draft_revision);
        sdk_result(
            self.service
                .add_line(&caller, order_id, line, &meta)
                .await?,
        )
    }
    async fn patch_line(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        line_id: Uuid,
        patch: LinePatch,
        expected_draft_revision: Option<DraftRevision>,
        meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        let meta = sdk_meta(&meta, expected_draft_revision);
        sdk_result(
            self.service
                .patch_line(&caller, order_id, line_id, patch, &meta)
                .await?,
        )
    }
    async fn remove_line(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        line_id: Uuid,
        expected_draft_revision: Option<DraftRevision>,
        meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        let meta = sdk_meta(&meta, expected_draft_revision);
        sdk_result(
            self.service
                .remove_line(&caller, order_id, line_id, &meta)
                .await?,
        )
    }
    async fn get(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        meta: ReadMeta,
    ) -> Result<OrderView, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.get(&caller, order_id).await
    }
    async fn list(
        &self,
        ctx: &SecurityContext,
        request: bss_orders_sdk::reads::ListOrders,
        meta: ReadMeta,
    ) -> Result<bss_orders_sdk::reads::OrderPage, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.list(&caller, &request).await
    }
    async fn list_lines(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        request: bss_orders_sdk::reads::LineList,
        meta: ReadMeta,
    ) -> Result<bss_orders_sdk::reads::LinePage, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.list_lines(&caller, order_id, &request).await
    }
    async fn submit(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        expected_draft_revision: DraftRevision,
        meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        // Production: submit is Stage 3 (S3-12): the gate, pin and real commercial providers are
        // undelivered. The development delivery (M2-1) runs the shared service over the stubs.
        let Some(service) = &self.submit else {
            return Err(not_delivered("submit (S3-12)").into());
        };
        // D-202 before any PDP or store work, as on the REST header path.
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        sdk_result(
            service
                .submit(
                    &caller,
                    order_id,
                    expected_draft_revision,
                    &meta,
                    &CancellationToken::new(),
                )
                .await
                .map_err(OrdersError::from)?,
        )
    }
    async fn preview(
        &self,
        ctx: &SecurityContext,
        basket: PreviewBasket,
        meta: ReadMeta,
    ) -> Result<PreviewResult, OrdersError> {
        // Production: Preview is Stage 3 (S3-11) and its Pricing, gate and Rating providers are
        // undelivered. The development delivery (M2-1) runs the shared service over the stubs.
        let Some(service) = &self.preview else {
            return Err(not_delivered("preview (S3-11)").into());
        };
        // D-202 before any PDP or store work, as on the REST header path.
        let caller = Caller::from_meta(ctx, meta.delegation_proof_ref.as_ref())?;
        service
            .preview(
                &caller,
                basket,
                meta.correlation_id,
                &CancellationToken::new(),
            )
            .await
    }
}

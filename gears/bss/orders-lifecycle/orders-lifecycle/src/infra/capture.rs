//! The draft authoring service (02 Capture; S2-09): the one entry REST and the local SDK share.
//!
//! Every operation is boundary-validated by its adapter, classified from its named fields alone
//! (02 §3.1, D-145) and handed to the transition engine: the dedicated create branch, or the
//! existing-order `draft-mutate` row with the registered capture guards and a child-document
//! contribution. Nothing here reads order state before authorization, writes the aggregate, or
//! calls a catalog, pricing or contract port (02 §3.4-3.5). An administrative-only `PATCH`
//! selects `administrative-edit`, whose S4-05 implementation is not delivered: it is refused
//! unavailable with no authorization, audit, write or idempotency activity, never written through
//! draft mutation.
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use bss_orders_lifecycle_sdk::authoring::{
    AddLine, CreateMeta, CreateOrder, HeaderPatch, LinePatch, OrderView, SelectedItem,
};
use bss_orders_lifecycle_sdk::catalog::{Reason, Trigger};
use bss_orders_lifecycle_sdk::models::{CallMeta, DraftRevision, TransitionResult};
use bss_orders_lifecycle_sdk::{OrdersError, OrdersLifecycleV1};
use serde_json::{Value, json};
use toolkit_db::secure::ScopeError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::authz::{Arrangement, ArrangementDelta, Caller};
use crate::domain::capture::{self, DraftGuardFacts, WorkingMember};
use crate::domain::contributions::{AggregateContribution, DraftHeaderEdit};
use crate::domain::idempotency::StoredResponse;
use crate::infra::engine::{
    ChildDocuments, CreateRequest, DocumentWriter, Engine, Preparation, PreparationView,
    PrepareError, Prepared, TransitionRequest,
};
use crate::infra::storage::entity::draft_content;

/// The configured capture bounds (DESIGN 02 §3.7: static per-gear configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureSettings {
    pub line_cap: usize,
}

/// Validated metadata of an existing-order draft write.
#[derive(Debug, Clone)]
pub struct WriteMeta {
    pub call: CallMeta,
    /// Optional at the boundary on `draft-mutate`; compared after admissibility (D-147).
    pub expected_draft_revision: Option<DraftRevision>,
}

/// The shared draft authoring service. The engine is present only while the gear is ready (the
/// managed producer is bound); before that, and after stop, every operation is unavailable.
#[derive(Clone)]
pub struct CaptureService {
    engine: Arc<ArcSwapOption<Engine>>,
    settings: CaptureSettings,
}
impl std::fmt::Debug for CaptureService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureService")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

fn invalid() -> OrdersError {
    OrdersError::Refused(Reason::RequestInvalid)
}

impl CaptureService {
    #[must_use]
    pub fn new(engine: Arc<ArcSwapOption<Engine>>, settings: CaptureSettings) -> Self {
        Self { engine, settings }
    }

    fn engine(&self) -> Result<Arc<Engine>, OrdersError> {
        self.engine.load_full().ok_or(OrdersError::Unavailable)
    }

    /// Create an empty draft through the engine's dedicated create branch (02 §2.1, D-105).
    ///
    /// # Errors
    /// Unsettled refusals and sanitized infrastructure outcomes; a settled refusal is the
    /// returned stored response.
    pub async fn create(
        &self,
        caller: &Caller,
        request: CreateOrder,
        meta: &CreateMeta,
    ) -> Result<StoredResponse, OrdersError> {
        let engine = self.engine()?;
        let document = fingerprint_document(&request)?;
        let create = CreateRequest {
            category: request.category.gts_id().to_owned(),
            arrangement: Arrangement {
                resource_tenant_id: request.resource_tenant_id,
                seller_tenant_id: request.seller_tenant_id,
                payer_tenant_id: request.payer_tenant_id,
            },
            // Recorded, never resolved (02 *Create Draft Order* step 5).
            contract_id: request.contract_id,
            document,
            idempotency_key: meta.idempotency_key.clone(),
            correlation_id: meta.correlation_id,
        };
        let outcome = engine.create(caller, create, &CreatePreparation).await?;
        Ok(outcome.response)
    }

    /// Edit the header. Fields alone select the trigger; no state is read to choose it.
    ///
    /// # Errors
    /// `request-invalid` input, unavailable administrative edits, unsettled refusals and
    /// sanitized infrastructure outcomes.
    pub async fn patch_order(
        &self,
        caller: &Caller,
        order_id: Uuid,
        patch: HeaderPatch,
        meta: &WriteMeta,
    ) -> Result<StoredResponse, OrdersError> {
        patch.validate().map_err(|_| invalid())?;
        let engine = self.engine()?;
        let selection =
            capture::select_header(&engine.registries().fields, &patch).ok_or_else(invalid)?;
        if !capture::is_draft_mutation(selection) {
            return Err(administrative_unavailable());
        }
        let proposed = (patch.resource_tenant_id.is_some() || patch.payer_tenant_id.is_some())
            .then_some(ArrangementDelta {
                resource_tenant_id: patch.resource_tenant_id,
                payer_tenant_id: patch.payer_tenant_id,
            });
        let preparation = DraftPreparation {
            settings: self.settings,
            operation: DraftOperation::Header {
                mixed: selection.mixed,
                names_seller: patch.seller_tenant_id.is_some(),
                edit: DraftHeaderEdit {
                    category: patch.category.map(|c| c.gts_id().to_owned()),
                    contract_id: patch.contract_id,
                },
                named_category: patch.category,
            },
        };
        let document = fingerprint_document(&json!({"fields": patch}))?;
        self.mutate(
            engine,
            caller,
            order_id,
            "patch_order",
            document,
            proposed,
            meta,
            &preparation,
        )
        .await
    }

    /// Add one working-set line with a server-reserved identity (02 §2.2).
    ///
    /// # Errors
    /// As [`Self::patch_order`].
    pub async fn add_line(
        &self,
        caller: &Caller,
        order_id: Uuid,
        line: AddLine,
        meta: &WriteMeta,
    ) -> Result<StoredResponse, OrdersError> {
        line.validate().map_err(|_| invalid())?;
        let engine = self.engine()?;
        let document = fingerprint_document(&json!({"line": line}))?;
        let preparation = DraftPreparation {
            settings: self.settings,
            operation: DraftOperation::AddLine(line),
        };
        self.mutate(
            engine,
            caller,
            order_id,
            "add_line",
            document,
            None,
            meta,
            &preparation,
        )
        .await
    }

    /// Edit a line: commercial fields select `draft-mutate`; administrative-only fields select
    /// the undelivered `administrative-edit` path.
    ///
    /// # Errors
    /// As [`Self::patch_order`].
    pub async fn patch_line(
        &self,
        caller: &Caller,
        order_id: Uuid,
        line_id: Uuid,
        patch: LinePatch,
        meta: &WriteMeta,
    ) -> Result<StoredResponse, OrdersError> {
        patch.validate().map_err(|_| invalid())?;
        let engine = self.engine()?;
        let selection =
            capture::select_line(&engine.registries().fields, &patch).ok_or_else(invalid)?;
        if !capture::is_draft_mutation(selection) {
            return Err(administrative_unavailable());
        }
        let document = fingerprint_document(&json!({"line_id": line_id, "fields": patch}))?;
        let preparation = DraftPreparation {
            settings: self.settings,
            operation: DraftOperation::EditLine {
                line_id,
                mixed: selection.mixed,
                patch,
            },
        };
        self.mutate(
            engine,
            caller,
            order_id,
            "patch_line",
            document,
            None,
            meta,
            &preparation,
        )
        .await
    }

    /// Remove a line from the working set; its reserved identity is never reused (02 §2.3).
    ///
    /// # Errors
    /// As [`Self::patch_order`].
    pub async fn remove_line(
        &self,
        caller: &Caller,
        order_id: Uuid,
        line_id: Uuid,
        meta: &WriteMeta,
    ) -> Result<StoredResponse, OrdersError> {
        let engine = self.engine()?;
        let document = json!({"line_id": line_id});
        let preparation = DraftPreparation {
            settings: self.settings,
            operation: DraftOperation::RemoveLine(line_id),
        };
        self.mutate(
            engine,
            caller,
            order_id,
            "remove_line",
            document,
            None,
            meta,
            &preparation,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn mutate(
        &self,
        engine: Arc<Engine>,
        caller: &Caller,
        order_id: Uuid,
        operation: &'static str,
        document: Value,
        proposed: Option<ArrangementDelta>,
        meta: &WriteMeta,
        preparation: &DraftPreparation,
    ) -> Result<StoredResponse, OrdersError> {
        let request = TransitionRequest {
            order_id,
            trigger: Trigger::DraftMutate,
            operation,
            expected_version: i32::try_from(i64::from(meta.call.expected_version))
                .map_err(|_| invalid())?,
            expected_draft_revision: meta.expected_draft_revision.map(i64::from),
            document,
            proposed,
            caller_reason: None,
            idempotency_key: meta.call.idempotency_key.clone(),
            correlation_id: meta.call.correlation_id,
            execution: None,
        };
        let outcome = engine.transition(caller, request, preparation).await?;
        Ok(outcome.response)
    }
}

/// The request contribution as the S1-06 fingerprint profile requires it: the normalized authored
/// content with every JSON number as its exact decimal string (authored numbers here are
/// integers: term counts and calendar units). Server identities never enter it.
fn fingerprint_document(value: &impl serde::Serialize) -> Result<Value, OrdersError> {
    fn exact(value: Value) -> Value {
        match value {
            Value::Number(n) => Value::String(n.to_string()),
            Value::Array(items) => Value::Array(items.into_iter().map(exact).collect()),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, exact(v))).collect())
            }
            other => other,
        }
    }
    serde_json::to_value(value)
        .map(exact)
        .map_err(|_| OrdersError::Integration)
}

/// The undelivered S4-05 administrative edit: explicitly unavailable, never a draft mutation.
fn administrative_unavailable() -> OrdersError {
    tracing::debug!(
        target: "orders.capture",
        "administrative-edit is not delivered (S4-05); refused unavailable before any effect"
    );
    OrdersError::Unavailable
}

/// The create row's contribution: its category guard only; no documents, no event.
struct CreatePreparation;
#[async_trait]
impl Preparation for CreatePreparation {
    async fn prepare(&self, _: &PreparationView) -> Result<Prepared, PrepareError> {
        let mut prepared = Prepared::new(AggregateContribution::None);
        prepared.guards = capture::create_bindings();
        Ok(prepared)
    }
}

enum DraftOperation {
    Header {
        mixed: bool,
        names_seller: bool,
        edit: DraftHeaderEdit,
        named_category: Option<bss_orders_lifecycle_sdk::authoring::Category>,
    },
    AddLine(AddLine),
    EditLine {
        line_id: Uuid,
        mixed: bool,
        patch: LinePatch,
    },
    RemoveLine(Uuid),
}

/// Row 2's preparation over the coherent draft snapshot: guard facts, the header edit and the
/// child-document contribution. No external port is called.
struct DraftPreparation {
    settings: CaptureSettings,
    operation: DraftOperation,
}
#[async_trait]
impl Preparation for DraftPreparation {
    async fn prepare(&self, view: &PreparationView) -> Result<Prepared, PrepareError> {
        let working: Vec<WorkingMember> = view
            .draft_lines
            .iter()
            .map(|row| WorkingMember {
                line_id: row.line_id,
                currency: row.currency.clone(),
            })
            .collect();
        let mut facts = DraftGuardFacts {
            mixed: false,
            names_seller: false,
            named_category: None,
            target_line: None,
            proposed_currency: None,
            inserts_line: false,
            working,
            line_cap: self.settings.line_cap,
        };
        let mut header = DraftHeaderEdit::default();
        let mut line_id = None;
        let documents: Option<Arc<dyn DocumentWriter>> = match &self.operation {
            DraftOperation::Header {
                mixed,
                names_seller,
                edit,
                named_category,
            } => {
                facts.mixed = *mixed;
                facts.names_seller = *names_seller;
                facts.named_category = *named_category;
                header = edit.clone();
                None
            }
            DraftOperation::AddLine(line) => {
                let id = Uuid::new_v4();
                line_id = Some(id);
                facts.inserts_line = true;
                facts.proposed_currency = Some(line.currency.as_str().to_owned());
                Some(Arc::new(InsertLine {
                    row: draft_row(view.facts.order_id, id, line)
                        .map_err(|()| PrepareError::Integration)?,
                }))
            }
            DraftOperation::EditLine {
                line_id: target,
                mixed,
                patch,
            } => {
                facts.mixed = *mixed;
                facts.target_line = Some(*target);
                facts.proposed_currency = patch.currency.as_ref().map(|c| c.as_str().to_owned());
                // A non-member never reaches the writer: the membership guard refuses first.
                match view.draft_lines.iter().find(|row| row.line_id == *target) {
                    Some(expected) => Some(Arc::new(ReplaceLine {
                        expected: expected.clone(),
                        proposed: edited_row(expected, patch)
                            .map_err(|()| PrepareError::Integration)?,
                    }) as Arc<dyn DocumentWriter>),
                    None => None,
                }
            }
            DraftOperation::RemoveLine(target) => {
                facts.target_line = Some(*target);
                Some(Arc::new(RemoveLine { line_id: *target }))
            }
        };
        let mut prepared = Prepared::new(AggregateContribution::DraftEdit(header));
        prepared.guards = facts.bindings();
        prepared.documents = documents;
        prepared.line_id = line_id;
        Ok(prepared)
    }
}

/// The authored line as its working-set row; dates, term and cycle retained exactly as authored
/// (02 *Author Line* steps 3-4: no cascade, no default, no anchor).
fn draft_row(order_id: Uuid, line_id: Uuid, line: &AddLine) -> Result<draft_content::Model, ()> {
    let (term_kind, authored_term, term_duration) =
        capture::term_columns(line.term_duration.as_ref(), line.billing_cycle);
    Ok(draft_content::Model {
        order_id,
        line_id,
        plan_id: line.plan_id,
        plan_revision_id: line.plan_revision_id,
        selected_items: items_json(&line.selected_items)?,
        currency: line.currency.as_str().to_owned(),
        contract_effective_date: line.contract_effective_date.map(|d| d.0),
        service_activation_date: line.service_activation_date.map(|d| d.0),
        acceptance_due_date: line.acceptance_due_date.map(|d| d.0),
        term_duration,
        term_kind: term_kind.to_owned(),
        authored_term,
        billing_cycle: line.billing_cycle.map(|c| c.token().to_owned()),
    })
}

/// The named commercial changes applied to the locked working row; every unnamed value is
/// preserved. The stored interval is recomputed only when the term or the cycle is named.
fn edited_row(
    expected: &draft_content::Model,
    patch: &LinePatch,
) -> Result<draft_content::Model, ()> {
    let mut row = expected.clone();
    if let Some(plan_id) = patch.plan_id {
        row.plan_id = plan_id;
    }
    if let Some(revision) = patch.plan_revision_id {
        row.plan_revision_id = revision;
    }
    if let Some(items) = &patch.selected_items {
        row.selected_items = items_json(items)?;
    }
    if let Some(currency) = &patch.currency {
        currency.as_str().clone_into(&mut row.currency);
    }
    if let Some(date) = patch.contract_effective_date {
        row.contract_effective_date = date.map(|d| d.0);
    }
    if let Some(date) = patch.service_activation_date {
        row.service_activation_date = date.map(|d| d.0);
    }
    if let Some(date) = patch.acceptance_due_date {
        row.acceptance_due_date = date.map(|d| d.0);
    }
    if patch.term_duration.is_some() || patch.billing_cycle.is_some() {
        let term = match patch.term_duration {
            Some(term) => term,
            None => capture::stored_term(&expected.term_kind, &expected.authored_term)?,
        };
        let cycle = match patch.billing_cycle {
            Some(cycle) => cycle,
            None => stored_cycle(expected.billing_cycle.as_deref())?,
        };
        let (term_kind, authored_term, term_duration) = capture::term_columns(term.as_ref(), cycle);
        let unchanged_geometry = term_kind == expected.term_kind
            && authored_term == expected.authored_term
            && cycle.map(bss_orders_lifecycle_sdk::authoring::BillingCycle::token)
                == expected.billing_cycle.as_deref();
        if !unchanged_geometry {
            term_kind.clone_into(&mut row.term_kind);
            row.authored_term = authored_term;
            row.term_duration = term_duration;
            row.billing_cycle = cycle.map(|c| c.token().to_owned());
        }
    }
    Ok(row)
}

fn stored_cycle(
    token: Option<&str>,
) -> Result<Option<bss_orders_lifecycle_sdk::authoring::BillingCycle>, ()> {
    use bss_orders_lifecycle_sdk::authoring::BillingCycle;
    match token {
        None => Ok(None),
        Some("month") => Ok(Some(BillingCycle::Month)),
        Some("year") => Ok(Some(BillingCycle::Year)),
        Some(_) => Err(()),
    }
}

fn items_json(items: &[SelectedItem]) -> Result<Value, ()> {
    serde_json::to_value(items).map_err(|_| ())
}

struct InsertLine {
    row: draft_content::Model,
}
#[async_trait]
impl DocumentWriter for InsertLine {
    async fn write(&self, documents: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        // Reserve the never-used identity and add membership in the engine's transaction.
        documents.add_draft_line(self.row.clone()).await.map(|_| ())
    }
}

struct ReplaceLine {
    expected: draft_content::Model,
    proposed: draft_content::Model,
}
#[async_trait]
impl DocumentWriter for ReplaceLine {
    async fn write(&self, documents: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        documents
            .replace_draft_content(&self.expected, self.proposed.clone())
            .await
    }
}

struct RemoveLine {
    line_id: Uuid,
}
#[async_trait]
impl DocumentWriter for RemoveLine {
    async fn write(&self, documents: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        // The membership guard admitted it, so exactly one working row must go.
        if documents.remove_draft_line(self.line_id).await? {
            Ok(())
        } else {
            Err(ScopeError::Denied(
                "removed line was not a working-set member",
            ))
        }
    }
}

/// The local SDK client: the same services and caller adapter as REST (08 §3.5), for draft
/// authoring (S2-09) and the authorized draft reads (early S6-01/S6-04).
pub struct LocalOrdersClient {
    service: Arc<CaptureService>,
    reads: Arc<crate::infra::read::ReadService>,
}
impl LocalOrdersClient {
    #[must_use]
    pub fn new(service: Arc<CaptureService>, reads: Arc<crate::infra::read::ReadService>) -> Self {
        Self { service, reads }
    }
}

/// Decode a settled outcome for the SDK: the typed success body, or the settled refusal with its
/// registered reason and the stored Problem, so SDK callers receive the same permitted
/// diagnostics (`context.data`) as REST.
///
/// # Errors
/// The settled refusal, or an integration failure for an undecodable stored body.
pub fn sdk_result<T: serde::de::DeserializeOwned>(
    response: StoredResponse,
) -> Result<T, OrdersError> {
    if (200..300).contains(&response.status) {
        return serde_json::from_value(response.body).map_err(|_| OrdersError::Integration);
    }
    let problem: toolkit_canonical_errors::Problem =
        serde_json::from_value(response.body).map_err(|_| OrdersError::Integration)?;
    let reason = problem
        .error_code
        .as_deref()
        .and_then(|code| Reason::ALL.iter().find(|r| r.mapping().code == code))
        .ok_or(OrdersError::Integration)?;
    Err(OrdersError::Settled {
        reason: *reason,
        problem: Box::new(problem),
    })
}

fn sdk_meta(meta: &CallMeta, expected_draft_revision: Option<DraftRevision>) -> WriteMeta {
    WriteMeta {
        call: meta.clone(),
        expected_draft_revision,
    }
}

#[async_trait]
impl OrdersLifecycleV1 for LocalOrdersClient {
    async fn create(
        &self,
        ctx: &SecurityContext,
        request: CreateOrder,
        meta: CreateMeta,
    ) -> Result<OrderView, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref.clone());
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
        let caller = crate::api::rest::sdk_caller(ctx, &meta);
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
        let caller = crate::api::rest::sdk_caller(ctx, &meta);
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
        let caller = crate::api::rest::sdk_caller(ctx, &meta);
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
        let caller = crate::api::rest::sdk_caller(ctx, &meta);
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
        meta: bss_orders_lifecycle_sdk::reads::ReadMeta,
    ) -> Result<OrderView, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.get(&caller, order_id).await
    }
    async fn list(
        &self,
        ctx: &SecurityContext,
        request: bss_orders_lifecycle_sdk::reads::ListOrders,
        meta: bss_orders_lifecycle_sdk::reads::ReadMeta,
    ) -> Result<bss_orders_lifecycle_sdk::reads::OrderPage, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.list(&caller, &request).await
    }
    async fn list_lines(
        &self,
        ctx: &SecurityContext,
        order_id: Uuid,
        request: bss_orders_lifecycle_sdk::reads::LineList,
        meta: bss_orders_lifecycle_sdk::reads::ReadMeta,
    ) -> Result<bss_orders_lifecycle_sdk::reads::LinePage, OrdersError> {
        let caller = Caller::new(ctx.clone(), meta.delegation_proof_ref);
        self.reads.list_lines(&caller, order_id, &request).await
    }
    async fn submit(
        &self,
        _ctx: &SecurityContext,
        _order_id: Uuid,
        _expected_draft_revision: DraftRevision,
        _meta: CallMeta,
    ) -> Result<TransitionResult, OrdersError> {
        // Submit is Stage 3 (S3-12): the gate, pin and real commercial providers are undelivered.
        Err(OrdersError::Unavailable)
    }
}

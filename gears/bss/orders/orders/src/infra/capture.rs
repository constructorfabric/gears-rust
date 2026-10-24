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
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{
    AddLine, AuthoringInvalid, CreateMeta, CreateOrder, HeaderPatch, LinePatch, SelectedItem,
};
use bss_orders_sdk::catalog::{Reason, Trigger};
use bss_orders_sdk::models::{CallMeta, DraftRevision};
use serde_json::Value;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

use crate::authz::{Arrangement, ArrangementDelta, Caller};
use crate::domain::capture::{self, DraftGuardFacts, WorkingMember};
use crate::domain::contributions::{AggregateContribution, DraftHeaderEdit};
use crate::domain::idempotency::StoredResponse;
use crate::infra::diagnostics::serde_category;
use crate::infra::engine::errors::not_delivered;
use crate::infra::engine::{
    ChildDocuments, CreateRequest, DocumentWriter, Engine, EngineOutcome, OutcomeKind, Preparation,
    PreparationView, PrepareError, Prepared, TransitionRequest,
};
use crate::infra::storage::entity::draft_content;
use crate::infra::storage::repo::idempotency::ReplayOutcome;
use crate::infra::storage::term::{self, StoredShapeError};

/// A settled write as the caller sees it: the stored response, exactly as stored, how the engine
/// settled it, and whether this call replayed it. The flag is never stored; the REST adapter
/// renders it as `Idempotency-Replayed: true`.
#[derive(Debug, Clone)]
pub struct Settled {
    /// The settled response, exactly as stored.
    pub response: StoredResponse,
    /// The engine's recorded outcome of the settlement (first attempt or replay).
    pub outcome: SettledOutcome,
    /// The order the engine recorded for the settlement: the committed order, or the order of
    /// the replayed record; none for a fresh refusal and a refused create.
    pub order_id: Option<Uuid>,
    /// Whether this call replayed an earlier settlement.
    pub replayed: bool,
}
/// The engine's recorded outcome of a settlement: the SDK path decides success or refusal from
/// it, never from the stored HTTP status (D-219).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledOutcome {
    /// The transition committed; the stored body is the success body.
    Committed,
    /// A business refusal settled the key, under the recorded reason; `None` when a replayed
    /// record names a token that is not a registered reason.
    Refused(Option<Reason>),
}
impl Settled {
    fn from_outcome(outcome: EngineOutcome) -> Self {
        let (settled, order_id, replayed) = match outcome.kind {
            OutcomeKind::Committed { order_id, .. } => {
                (SettledOutcome::Committed, Some(order_id), false)
            }
            OutcomeKind::Refused { reason, .. } => {
                (SettledOutcome::Refused(Some(reason)), None, false)
            }
            OutcomeKind::Replayed {
                outcome: ReplayOutcome::Success,
                order_id,
            } => (SettledOutcome::Committed, order_id, true),
            OutcomeKind::Replayed {
                outcome: ReplayOutcome::Refused(token),
                order_id,
            } => (
                SettledOutcome::Refused(serde_json::from_value(Value::String(token)).ok()),
                order_id,
                true,
            ),
        };
        Self {
            response: outcome.response,
            outcome: settled,
            order_id,
            replayed,
        }
    }
}
impl From<Settled> for StoredResponse {
    fn from(settled: Settled) -> Self {
        settled.response
    }
}

/// The configured capture bounds (DESIGN 02 §3.7: static per-gear configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureSettings {
    /// The maximum number of lines one order may hold (1..200).
    pub line_cap: usize,
}

/// Validated metadata of an existing-order draft write.
#[derive(Debug, Clone)]
pub struct WriteMeta {
    /// The call's expected version, idempotency key, correlation and delegation proof.
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

/// `request-invalid` for an authored value its SDK type refuses; the Problem is unchanged. The
/// debug record names the operation and the static member the validation named.
fn refused_member(
    operation: &'static str,
    AuthoringInvalid(member): AuthoringInvalid,
) -> OrdersError {
    tracing::debug!(
        target: "orders.capture",
        operation,
        member,
        "authored value refused request-invalid"
    );
    invalid()
}

impl CaptureService {
    /// A service over the engine slot, which is empty while the gear is not ready.
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
    ) -> Result<Settled, OrdersError> {
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
        Ok(Settled::from_outcome(outcome))
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
    ) -> Result<Settled, OrdersError> {
        patch
            .validate()
            .map_err(|e| refused_member("patch_order", e))?;
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
        let document = fingerprint_document(&document_of([("fields", authored(&patch)?)]))?;
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
    ) -> Result<Settled, OrdersError> {
        line.validate().map_err(|e| refused_member("add_line", e))?;
        let engine = self.engine()?;
        let document = fingerprint_document(&document_of([("line", authored(&line)?)]))?;
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
    ) -> Result<Settled, OrdersError> {
        patch
            .validate()
            .map_err(|e| refused_member("patch_line", e))?;
        let engine = self.engine()?;
        let selection =
            capture::select_line(&engine.registries().fields, &patch).ok_or_else(invalid)?;
        if !capture::is_draft_mutation(selection) {
            return Err(administrative_unavailable());
        }
        let document = fingerprint_document(&document_of([
            ("line_id", Value::String(line_id.to_string())),
            ("fields", authored(&patch)?),
        ]))?;
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
    ) -> Result<Settled, OrdersError> {
        let engine = self.engine()?;
        let document = document_of([("line_id", Value::String(line_id.to_string()))]);
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

    #[expect(
        clippy::too_many_arguments,
        reason = "the one draft-mutate entry every authoring operation shares: each argument is \
                  a distinct per-operation input of the transition request"
    )]
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
    ) -> Result<Settled, OrdersError> {
        let request = TransitionRequest {
            order_id,
            trigger: Trigger::DraftMutate,
            operation,
            expected_version: meta.call.expected_version,
            expected_draft_revision: meta.expected_draft_revision,
            document,
            proposed,
            caller_reason: None,
            idempotency_key: meta.call.idempotency_key.clone(),
            correlation_id: meta.call.correlation_id,
            execution: None,
        };
        let outcome = engine.transition(caller, request, preparation).await?;
        Ok(Settled::from_outcome(outcome))
    }
}

/// The request contribution as the S1-06 fingerprint profile requires it: the normalized authored
/// content with every JSON number as its exact decimal string (authored numbers here are
/// integers: term counts and calendar units). Server identities never enter it.
/// An authored value as JSON. Never `json!`, which unwraps the conversion: an in-process SDK
/// value outside its wire encoding (a date outside years 0..=9999) is refused `request-invalid`
/// rather than panicking in the request path.
fn authored(value: &impl serde::Serialize) -> Result<Value, OrdersError> {
    serde_json::to_value(value).map_err(|error| {
        tracing::debug!(
            target: "orders.capture",
            category = serde_category(&error),
            "authored value has no wire encoding; refused request-invalid"
        );
        invalid()
    })
}

/// A fingerprint document object from already-encoded members.
fn document_of<const N: usize>(members: [(&str, Value); N]) -> Value {
    Value::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

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
    authored(value).map(exact)
}

/// The undelivered S4-05 administrative edit: explicitly unavailable, never a draft mutation.
fn administrative_unavailable() -> OrdersError {
    not_delivered("administrative-edit (S4-05)").into()
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
        named_category: Option<bss_orders_sdk::authoring::Category>,
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
                        .map_err(|failure| failure.integration(view.facts.order_id, id))?,
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
                            .map_err(|failure| failure.integration(view.facts.order_id, *target))?,
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
fn draft_row(
    order_id: Uuid,
    line_id: Uuid,
    line: &AddLine,
) -> Result<draft_content::Model, RowFailure> {
    let term = term::term_columns(line.term_duration.as_ref(), line.billing_cycle)?;
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
        term_duration: term.duration,
        term_kind: term.kind.to_owned(),
        authored_term: term.authored,
        billing_cycle: line.billing_cycle.map(|c| c.token().to_owned()),
    })
}

/// The named commercial changes applied to the locked working row; every unnamed value is
/// preserved. The stored interval is recomputed only when the term or the cycle is named.
fn edited_row(
    expected: &draft_content::Model,
    patch: &LinePatch,
) -> Result<draft_content::Model, RowFailure> {
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
            None => term::stored_term(&expected.term_kind, &expected.authored_term)?,
        };
        let cycle = match patch.billing_cycle {
            Some(cycle) => cycle,
            None => stored_cycle(expected.billing_cycle.as_deref())?,
        };
        let columns = term::term_columns(term.as_ref(), cycle)?;
        let unchanged_geometry = columns.kind == expected.term_kind
            && columns.authored == expected.authored_term
            && cycle.map(bss_orders_sdk::authoring::BillingCycle::token)
                == expected.billing_cycle.as_deref();
        if !unchanged_geometry {
            columns.kind.clone_into(&mut row.term_kind);
            row.authored_term = columns.authored;
            row.term_duration = columns.duration;
            row.billing_cycle = cycle.map(|c| c.token().to_owned());
        }
    }
    Ok(row)
}

fn stored_cycle(
    token: Option<&str>,
) -> Result<Option<bss_orders_sdk::authoring::BillingCycle>, RowFailure> {
    token
        .map(|token| {
            bss_orders_sdk::authoring::BillingCycle::from_token(token).ok_or(RowFailure {
                column: "billing_cycle",
                category: None,
            })
        })
        .transpose()
}

fn items_json(items: &[SelectedItem]) -> Result<Value, RowFailure> {
    serde_json::to_value(items).map_err(|error| RowFailure {
        column: "selected_items",
        category: Some(serde_category(&error)),
    })
}

/// Why a working row could not be built from the locked row and the authored edit: the column
/// and, for a serde failure, its category; never the serde message or the stored content (D-44).
#[derive(Debug, Clone, Copy)]
struct RowFailure {
    column: &'static str,
    category: Option<&'static str>,
}
impl RowFailure {
    /// The one operational record of the failure; the attempt fails closed as an integration
    /// failure before any transaction.
    fn integration(self, order_id: Uuid, line_id: Uuid) -> PrepareError {
        tracing::error!(
            target: "orders.capture",
            order_id = %order_id,
            line_id = %line_id,
            column = self.column,
            category = self.category,
            "working row cannot be built"
        );
        PrepareError::Integration
    }
}
impl From<StoredShapeError> for RowFailure {
    fn from(error: StoredShapeError) -> Self {
        match error {
            StoredShapeError::UnknownKind => Self {
                column: "term_kind",
                category: None,
            },
            StoredShapeError::Undecodable { category, .. } => Self {
                column: "authored_term",
                category: Some(category),
            },
            StoredShapeError::Unrepresentable => Self {
                column: "term_duration",
                category: None,
            },
        }
    }
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

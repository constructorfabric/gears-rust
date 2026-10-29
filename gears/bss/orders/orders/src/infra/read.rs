//! The authorized draft/submitted read service with S6-04 access logging: the one
//! common read wrapper REST and the local SDK share (08 §3.6 OL-66/67).
//!
//! Every read runs the same sequence: boundary validation (page size, filters, cursor) with no
//! access-log row; the approved minimal point prefetch; the exact `order × read` decision through
//! the shared PEP, with the D-68/D-114/D-141 hidden/missing call pattern; one consistent
//! read-only snapshot started after the decision, through the PDP-produced scope (the prefetched
//! row is never returned; changed authorization facts restart authorization); composition from
//! stored rows without any chain walk; then the §4.4 logging decision. A required served log is
//! committed in its own transaction **before** the payload is returned and its failure is
//! `read-store-unavailable` with no payload; a refused log is appended on every access refusal
//! and its failure preserves the refusal and raises the operational signal.
//!
//! **Transaction decision (S2-02/S2-11 hand-off).** The snapshot is a `REPEATABLE READ`,
//! `READ ONLY` transaction, so a read can never write; the access-log row therefore commits in
//! a separate short transaction after the snapshot and before disclosure. `insert_read_access_log`
//! keeps accepting any `DBRunner` and is called with that transaction; it stamps `accessed_at`
//! from the database clock inside that transaction, as every other evidence writer does (OL-2),
//! so no application clock reaches the log. Nothing shares the snapshot with a write.
//!
//! **Changed facts.** The decided scope admitted the prefetched axes, so a row the scoped
//! re-read cannot see any more, or sees with other axes, has changed (or vanished) since the
//! decision. Both restart on the current facts (08 §3.6 *Prefetch and authorized snapshot*):
//! a vanished row takes the missing arm, changed axes take a fresh decision, and every refusal
//! arm therefore appends its refused row (*Read One Order* step 3.1). The bound is
//! [`RESTARTS`]; past it the read is refused `authorization-context-changed`, logged, nothing
//! disclosed.
//!
//! Drafts compose from their working set. Submitted orders compose exclusively from their
//! immutable current version, validated commercial pins and persisted producer totals. Later
//! lifecycle states remain unavailable until their fulfillment projections are delivered.
use std::collections::BTreeSet;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{
    BillingCycle, CalendarDate, Category, Currency, DraftLine, OrderHeader, OrderView, SalesPath,
    SelectedItem, VersionSummary,
};
use bss_orders_sdk::catalog::{OrderState, Reason};
use bss_orders_sdk::commercial::OrderPin;
use bss_orders_sdk::models::{DraftRevision, OrderVersion};
use bss_orders_sdk::reads::{
    Cursor, LineList, LinePage, ListOrders, OrderPage, OrderSummary, PageInfo, PageSize,
};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use serde::Deserialize;
use toolkit_db::Db;
use toolkit_db::secure::{TxAccessMode, TxConfig, TxIsolationLevel};
use uuid::Uuid;

use crate::authz::{Action, AuthzFailure, Caller, Pep, Prefetch};
use crate::domain::audit::{ActorIdentities, AuditActor};
use crate::domain::gate::answers::EvaluationChargeKind;
use crate::domain::gate::commercial::digest_hex;
use crate::domain::gate::totals::TotalScope;
use crate::domain::read::ProofDenial;
use crate::domain::read::{
    self, AccessOutcome, Collection, CursorBinding, GET_OPERATION, Position, ReadRuleError,
    Refusal, ServedLog, Target,
};
use crate::infra::diagnostics::serde_category;
use crate::infra::engine::errors::not_delivered;
use crate::infra::storage::entity::{
    draft_content, order, order_line, order_line_identity, read_access_log, resolved_total,
};
use crate::infra::storage::repo::{self, children, private};
use crate::infra::storage::scoped::{self, AuthorizedRead, PrivateScope};
use crate::infra::storage::term::{self, StoredShapeError};

/// How many times changed authorization facts restart the decision before the read gives up
/// (08 §3.6 *Prefetch and authorized snapshot*).
const RESTARTS: usize = 3;

/// The bound read dependencies: present only while the gear serves traffic.
pub struct ReadParts {
    /// The database the read snapshots run on.
    pub db: Db,
    /// The one shared PEP (08 §3.5).
    pub pep: Pep,
    /// Configured identities the access-log actor class derives from (D-115).
    pub identities: ActorIdentities,
}

/// Operational signals of the read surface (08 §3.8): log-write failures by outcome, refusals
/// by reason and delegation-proof denials, counted even where the public answer is sanitized
/// (D-141). Latency and page-size distributions are S6-06.
pub trait ReadSignals: Send + Sync {
    /// Count an access-log row that could not be committed, by outcome.
    fn access_log_write_failed(&self, outcome: AccessOutcome);
    /// Count a read refusal by reason, and a delegation-proof denial when `proof` is set.
    fn refused(&self, reason: Reason, proof: Option<ProofDenial>);
}

/// Counters on the process meter under `bss-orders`.
pub struct OtelReadSignals {
    log_failures: Counter<u64>,
    refusals: Counter<u64>,
    proof_denials: Counter<u64>,
}
impl Default for OtelReadSignals {
    fn default() -> Self {
        Self::new()
    }
}
impl OtelReadSignals {
    /// Counters registered on the global `bss-orders` meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("bss-orders");
        Self {
            log_failures: meter
                .u64_counter("orders_read_access_log_write_failures_total")
                .with_description("Read access-log rows that could not be committed, by outcome")
                .build(),
            refusals: meter
                .u64_counter("orders_read_refusals_total")
                .with_description("Read refusals by registered reason")
                .build(),
            proof_denials: meter
                .u64_counter("orders_read_delegation_proof_denials_total")
                .with_description(
                    "PDP delegation-proof denials on reads, including those answered order-not-found",
                )
                .build(),
        }
    }
}
impl ReadSignals for OtelReadSignals {
    fn access_log_write_failed(&self, outcome: AccessOutcome) {
        self.log_failures
            .add(1, &[KeyValue::new("outcome", outcome.token())]);
    }
    fn refused(&self, reason: Reason, proof: Option<ProofDenial>) {
        self.refusals
            .add(1, &[KeyValue::new("reason", reason.mapping().reason)]);
        if let Some(kind) = proof {
            let kind = match kind {
                ProofDenial::Required => "required",
                ProofDenial::Invalid => "invalid",
            };
            self.proof_denials.add(1, &[KeyValue::new("kind", kind)]);
        }
    }
}

/// The shared read service. Reads are unavailable while the gear is not serving (no parts).
#[derive(Clone)]
pub struct ReadService {
    parts: Arc<ArcSwapOption<ReadParts>>,
    signals: Arc<dyn ReadSignals>,
}
impl std::fmt::Debug for ReadService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadService").finish_non_exhaustive()
    }
}

/// The consistent read-only snapshot (08 §3.6: one database snapshot started after the PDP
/// decision; replica and stale-cache reads are forbidden, so it runs on the primary).
fn snapshot_config() -> TxConfig {
    TxConfig {
        isolation: Some(TxIsolationLevel::RepeatableRead),
        access_mode: Some(TxAccessMode::ReadOnly),
    }
}

/// What a targeted read loads inside its snapshot.
enum Load {
    /// The composed detail: version row, every working line identity and content.
    Detail,
    /// One line page: identities after the cursor, at most `limit`, plus their content.
    Lines { after: Option<Position>, limit: u64 },
}

/// The rows of one targeted snapshot.
struct Snapshot {
    row: order::Model,
    version: Option<crate::infra::storage::entity::order_version::Model>,
    identities: Vec<order_line_identity::Model>,
    drafts: Vec<draft_content::Model>,
    committed: Vec<order_line::Model>,
    totals: Vec<resolved_total::Model>,
}

/// Outcome of one authorized snapshot attempt.
enum Attempt {
    Loaded(Box<Snapshot>),
    /// Authorization-relevant facts changed between decision and snapshot, or the row left the
    /// decided scope (or vanished): restart on the current facts so the right arm decides and
    /// logs. The prefetched row is never disclosed.
    Restart,
}

/// The access-log target columns of a targeted read.
fn target_of(prefetch: &Prefetch) -> Target {
    Target {
        requested_order_ref: prefetch.order_id(),
        exists: matches!(prefetch, Prefetch::Found(_)),
    }
}

fn integration(what: &'static str) -> OrdersError {
    tracing::error!(target: "orders.read.integration", what, "stored row cannot be composed");
    OrdersError::Integration
}

/// A stored line whose authored columns do not compose: the row identity, the column and, for a
/// serde failure, its category; never the serde message or the stored content (D-44, D-218).
fn line_integration(
    order_id: Uuid,
    line_id: Uuid,
    column: &'static str,
    category: Option<&'static str>,
) -> OrdersError {
    tracing::error!(
        target: "orders.read.integration",
        order_id = %order_id,
        line_id = %line_id,
        column,
        category,
        "stored line cannot be composed"
    );
    OrdersError::Integration
}

/// A committed row (line or total) that fails a named integrity check: the row identity, the
/// first failing check and, where there is one, a static or codec cause; never the stored
/// content (D-44, D-218). The shape of [`line_integration`] and of the gate's
/// `submit_integration`.
fn committed_integration(
    order_id: Uuid,
    line_id: Uuid,
    check: &'static str,
    cause: Option<&dyn std::fmt::Display>,
) -> OrdersError {
    tracing::error!(
        target: "orders.read.integration",
        order_id = %order_id,
        line_id = %line_id,
        check,
        cause = cause.map(tracing::field::display),
        "committed row fails its integrity check"
    );
    OrdersError::Integration
}

impl ReadService {
    /// A service over the read-parts slot, which is empty while the gear is not serving.
    #[must_use]
    pub fn new(parts: Arc<ArcSwapOption<ReadParts>>, signals: Arc<dyn ReadSignals>) -> Self {
        Self { parts, signals }
    }

    fn parts(&self) -> Result<Arc<ReadParts>, OrdersError> {
        self.parts.load_full().ok_or(OrdersError::Unavailable)
    }

    /// The SDK path validates the proof reference exactly as the REST header does (D-202):
    /// a credential-bearing reference is `request-invalid` before any decision or read.
    fn check_proof(caller: &Caller) -> Result<(), OrdersError> {
        caller
            .supplied_proof()
            .map_or(Ok(()), crate::authz::check_proof_reference)
    }

    /// *Read One Order* (08 §3.6 scoped read): the composed current draft at its current
    /// version, ETag-bearing version and coherent draft revision.
    ///
    /// # Errors
    /// `order-not-found` on both hidden and missing arms, sanitized 503 on PDP outage, the
    /// integration failure for invalid constraints, `read-store-unavailable` when a required
    /// served log cannot be committed, or unavailable for an undelivered composition.
    pub async fn get(&self, caller: &Caller, order_id: Uuid) -> Result<OrderView, OrdersError> {
        Self::check_proof(caller)?;
        let parts = self.parts()?;
        let snapshot = self
            .targeted(&parts, caller, GET_OPERATION, order_id, &Load::Detail)
            .await?;
        let view = compose_view(&snapshot)?;
        self.served_point(&parts, caller, GET_OPERATION, &snapshot.row)
            .await?;
        Ok(view)
    }

    /// The authorized draft read of an existing-order write (submit): [`Self::get`]'s decision,
    /// snapshot and served log, but `Ok(None)` when the authorized order is no longer a draft, so
    /// the engine settles the state table's refusal (`not-admissible`) instead of the read
    /// answering the undelivered committed composition. Nothing is disclosed and no served log is
    /// written for `None`; a refusal is the read's own answer and logged as [`Self::get`]'s.
    ///
    /// # Errors
    /// As [`Self::get`].
    pub async fn get_draft(
        &self,
        caller: &Caller,
        order_id: Uuid,
    ) -> Result<Option<OrderView>, OrdersError> {
        Self::check_proof(caller)?;
        let parts = self.parts()?;
        let snapshot = self
            .targeted(&parts, caller, GET_OPERATION, order_id, &Load::Detail)
            .await?;
        if !is_draft(&snapshot.row) {
            return Ok(None);
        }
        let view = compose_view(&snapshot)?;
        self.served_point(&parts, caller, GET_OPERATION, &snapshot.row)
            .await?;
        Ok(Some(view))
    }

    /// *List Orders* (08 §3.6 paginated list): an untargeted `order × read`, the PDP scope,
    /// validated filters and the cursor boundary applied in SQL before `ORDER BY`/`LIMIT`.
    ///
    /// # Errors
    /// `cursor-invalid` (no access-log row), the untargeted denial reasons, sanitized 503,
    /// integration failure, or `read-store-unavailable`.
    pub async fn list(
        &self,
        caller: &Caller,
        request: &ListOrders,
    ) -> Result<OrderPage, OrdersError> {
        Self::check_proof(caller)?;
        let page_size = request.limit.unwrap_or_default();
        let hash = read::filters_hash(&request.filters);
        let binding = CursorBinding {
            collection: Collection::Orders,
            parent: None,
            subject_id: caller.ctx().subject_id(),
            subject_tenant_id: caller.ctx().subject_tenant_id(),
            filters_hash: &hash,
        };
        let after = decode(request.cursor.as_ref(), binding)?;
        let parts = self.parts()?;
        let scope = match parts
            .pep
            .authorize_collection(caller, Action::OrderRead)
            .await
        {
            Ok(scope) => scope,
            Err(failure) => {
                return Err(self
                    .refused(
                        &parts,
                        caller,
                        Collection::Orders.operation(),
                        None,
                        failure,
                    )
                    .await);
            }
        };
        let filters = request.filters.clone();
        let access = scope.access_scope().clone();
        let rows: Vec<order::Model> = parts
            .db
            .transaction_ref_mapped_with_config(snapshot_config(), move |tx| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        repo::read::order_page(
                            tx,
                            &access,
                            &filters,
                            after,
                            read::fetch_limit(page_size),
                        )
                        .await?,
                    )
                })
            })
            .await
            .map_err(|e| store_unavailable(Collection::Orders.operation(), None, &e))?;
        let (rows, more) = read::split_page(rows, page_size);
        let next_cursor = next_cursor(
            more,
            rows.last().map(|row| (row.created_at, row.order_id)),
            binding,
        )?;
        let orders = rows.iter().map(summary).collect::<Result<Vec<_>, _>>()?;
        let confined =
            read::confined_to_subject(scope.access_scope(), caller.ctx().subject_tenant_id());
        if read::collection_log(caller.supplied_proof().is_some(), confined) == ServedLog::Required
        {
            self.served(&parts, caller, Collection::Orders.operation(), None)
                .await?;
        }
        Ok(OrderPage {
            items: orders,
            page_info: PageInfo::forward(next_cursor, page_size),
        })
    }

    /// The per-order line page through the current scoped parent: identity order
    /// `(created_at, line_id)` joined to working or committed-version membership.
    ///
    /// # Errors
    /// As [`Self::get`], plus `cursor-invalid` before the access decision.
    pub async fn list_lines(
        &self,
        caller: &Caller,
        order_id: Uuid,
        request: &LineList,
    ) -> Result<LinePage, OrdersError> {
        Self::check_proof(caller)?;
        let page_size = request.limit.unwrap_or_default();
        let hash = read::no_filters_hash();
        let binding = CursorBinding {
            collection: Collection::Lines,
            parent: Some(order_id),
            subject_id: caller.ctx().subject_id(),
            subject_tenant_id: caller.ctx().subject_tenant_id(),
            filters_hash: &hash,
        };
        let decoded = request
            .cursor
            .as_ref()
            .map(|token| read::decode_cursor_bound(token, binding))
            .transpose()
            .map_err(|_| OrdersError::Refused(Reason::CursorInvalid))?;
        let after = decoded.map(|d| d.position);
        let parts = self.parts()?;
        let operation = Collection::Lines.operation();
        let load = Load::Lines {
            after,
            limit: read::fetch_limit(page_size),
        };
        let snapshot = self
            .targeted(&parts, caller, operation, order_id, &load)
            .await?;
        ensure_readable(&snapshot.row)?;
        // A line continuation is bound to the version it was issued at; one at another version
        // is refused (D-64: `current_version` is set from creation). A token `main` issued
        // carries no binding and was issued at the creation version (D-242).
        if let Some(decoded) = decoded
            && decoded.line_version() != snapshot.row.current_version
        {
            return Err(OrdersError::Refused(Reason::CursorInvalid));
        }
        let (identities, more) = read::split_page(snapshot.identities, page_size);
        let next_cursor = if more {
            identities
                .last()
                .map(|row| {
                    let position = Position::of(row.created_at, row.line_id)
                        .map_err(|_| integration("line cursor position"))?;
                    read::encode_cursor_version(
                        binding,
                        position,
                        Some(snapshot.row.current_version),
                    )
                    .map_err(|_| integration("line cursor encoding"))
                })
                .transpose()?
        } else {
            None
        };
        let lines = if is_draft(&snapshot.row) {
            compose_lines(&identities, &snapshot.drafts)?
        } else {
            compose_committed_lines(&snapshot.row, &identities, &snapshot.committed)?
        };
        let page = LinePage {
            items: lines,
            page_info: PageInfo::forward(next_cursor, page_size),
            current_version: version_of(snapshot.row.current_version)?,
            draft_revision: draft_revision(&snapshot.row)?,
        };
        self.served_point(&parts, caller, operation, &snapshot.row)
            .await?;
        Ok(page)
    }

    /// The common targeted wrapper: prefetch → exact decision → snapshot through the decided
    /// scope, restarting on changed facts; both refusal arms log and make the same calls.
    async fn targeted(
        &self,
        parts: &Arc<ReadParts>,
        caller: &Caller,
        operation: &'static str,
        order_id: Uuid,
        load: &Load,
    ) -> Result<Snapshot, OrdersError> {
        for _ in 0..RESTARTS {
            let prefetch = scoped::prefetch(&parts.db, order_id)
                .await
                .map_err(|e| store_unavailable(operation, Some(order_id), &e))?;
            let target = target_of(&prefetch);
            let decision = parts
                .pep
                .authorize_target(caller, Action::OrderRead, &prefetch)
                .await;
            let authorized = match decision {
                Ok(authorized) => authorized,
                Err(failure) => {
                    // D-68: the hidden and missing arms perform the same scoped re-read whose
                    // result is discarded, so neither arm skips a round trip.
                    let _discarded = scoped::prefetch(&parts.db, order_id).await;
                    return Err(self
                        .refused(parts, caller, operation, Some(target), failure)
                        .await);
                }
            };
            match Self::snapshot(parts, operation, &authorized, load).await? {
                Attempt::Loaded(snapshot) => return Ok(*snapshot),
                Attempt::Restart => {}
            }
        }
        // Facts kept changing under the decision: no disclosure on possibly stale authority.
        Err(self
            .refused(
                parts,
                caller,
                operation,
                Some(Target {
                    requested_order_ref: order_id,
                    exists: true,
                }),
                AuthzFailure::Refused {
                    reason: Reason::AuthorizationContextChanged,
                    proof: None,
                },
            )
            .await)
    }

    /// One consistent snapshot through the PDP-produced scope. The prefetched row is never
    /// returned; the scoped re-read decides. A row whose axes changed, or that the decided
    /// scope no longer admits, restarts authorization (see the module documentation): the
    /// refusal, if any, is then decided and logged by the ordinary arms, never silently here.
    async fn snapshot(
        parts: &Arc<ReadParts>,
        operation: &'static str,
        authorized: &crate::authz::TargetAuthorization,
        load: &Load,
    ) -> Result<Attempt, OrdersError> {
        let order_id = authorized.facts().order_id();
        let authorized = authorized.clone();
        let load = match load {
            Load::Detail => None,
            Load::Lines { after, limit } => Some((*after, *limit)),
        };
        parts
            .db
            .transaction_ref_mapped_with_config(snapshot_config(), move |tx| {
                Box::pin(async move {
                    let row = match scoped::read_authorized(tx, &authorized).await? {
                        AuthorizedRead::Current(row) => *row,
                        AuthorizedRead::FactsChanged | AuthorizedRead::NotVisible => {
                            return Ok::<_, anyhow::Error>(Attempt::Restart);
                        }
                    };
                    let scope = authorized.access_scope();
                    let (after, limit) = load.map_or((None, None), |(a, l)| (a, Some(l)));
                    let version = if load.is_none() {
                        children::version(tx, scope, row.order_id, row.current_version).await?
                    } else {
                        None
                    };
                    let (identities, drafts, committed, totals) = if is_draft(&row) {
                        let identities =
                            repo::read::draft_line_page(tx, scope, row.order_id, after, limit)
                                .await?;
                        let ids = identities.iter().map(|i| i.line_id).collect::<Vec<_>>();
                        let drafts =
                            children::draft_content_for_lines(tx, scope, row.order_id, &ids)
                                .await?;
                        (identities, drafts, Vec::new(), Vec::new())
                    } else {
                        let identities = repo::read::committed_line_page(
                            tx,
                            scope,
                            row.order_id,
                            row.current_version,
                            after,
                            limit,
                        )
                        .await?;
                        let ids = identities.iter().map(|i| i.line_id).collect::<Vec<_>>();
                        let committed = children::committed_content_for_lines(
                            tx,
                            scope,
                            row.order_id,
                            row.current_version,
                            &ids,
                        )
                        .await?;
                        let totals = if load.is_none() {
                            children::committed_totals(tx, scope, row.order_id, row.current_version)
                                .await?
                        } else {
                            Vec::new()
                        };
                        (identities, Vec::new(), committed, totals)
                    };
                    Ok(Attempt::Loaded(Box::new(Snapshot {
                        row,
                        version,
                        identities,
                        drafts,
                        committed,
                        totals,
                    })))
                })
            })
            .await
            .map_err(|e| store_unavailable(operation, Some(order_id), &e))
    }

    /// Apply §4.4 to a served point/child read and commit the required log before disclosure.
    async fn served_point(
        &self,
        parts: &Arc<ReadParts>,
        caller: &Caller,
        operation: &'static str,
        row: &order::Model,
    ) -> Result<(), OrdersError> {
        let decision = read::point_read_log(
            caller.supplied_proof().is_some(),
            caller.ctx().subject_tenant_id(),
            row.resource_tenant_id,
        );
        if decision == ServedLog::Required {
            self.served(
                parts,
                caller,
                operation,
                Some(Target {
                    requested_order_ref: row.order_id,
                    exists: true,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Commit a served row; failure is `read-store-unavailable` and nothing is disclosed.
    async fn served(
        &self,
        parts: &Arc<ReadParts>,
        caller: &Caller,
        operation: &'static str,
        target: Option<Target>,
    ) -> Result<(), OrdersError> {
        let actor = parts
            .identities
            .classify(caller.ctx())
            .map_err(|_| OrdersError::Integration)?;
        let row = log_row(
            caller,
            &actor,
            operation,
            AccessOutcome::Served,
            target,
            None,
        );
        append(&parts.db, caller, row).await.map_err(|error| {
            self.signals.access_log_write_failed(AccessOutcome::Served);
            tracing::error!(
                target: "orders.read.access_log",
                operation,
                error = %error,
                "served access log could not be committed; nothing disclosed"
            );
            OrdersError::Refused(Reason::ReadStoreUnavailable)
        })
    }

    /// Map an authorization failure to the caller's error, appending the refused row on every
    /// access refusal. Outage and integration failures are not refusals and log nothing.
    async fn refused(
        &self,
        parts: &Arc<ReadParts>,
        caller: &Caller,
        operation: &'static str,
        target: Option<Target>,
        failure: AuthzFailure,
    ) -> OrdersError {
        let AuthzFailure::Refused { reason, proof } = failure else {
            return failure.into();
        };
        self.signals.refused(reason, proof);
        let refusal = Refusal { reason, proof };
        let Ok(actor) = parts.identities.classify(caller.ctx()) else {
            self.signals.access_log_write_failed(AccessOutcome::Refused);
            tracing::error!(
                target: "orders.read.access_log",
                operation,
                "refused access log has no stable actor; refusal preserved"
            );
            return OrdersError::Refused(reason);
        };
        let row = log_row(
            caller,
            &actor,
            operation,
            AccessOutcome::Refused,
            target,
            Some(refusal),
        );
        if let Err(error) = append(&parts.db, caller, row).await {
            self.signals.access_log_write_failed(AccessOutcome::Refused);
            tracing::error!(
                target: "orders.read.access_log",
                operation,
                error = %error,
                "refused access log could not be committed; refusal preserved"
            );
        }
        OrdersError::Refused(reason)
    }
}

fn decode(
    cursor: Option<&Cursor>,
    binding: CursorBinding<'_>,
) -> Result<Option<Position>, OrdersError> {
    cursor
        .map(|token| read::decode_cursor(token, binding))
        .transpose()
        .map_err(|_| OrdersError::Refused(Reason::CursorInvalid))
}

fn next_cursor(
    more: bool,
    last: Option<(time::OffsetDateTime, Uuid)>,
    binding: CursorBinding<'_>,
) -> Result<Option<Cursor>, OrdersError> {
    match (more, last) {
        (true, Some((created_at, id))) => {
            let position = Position::of(created_at, id).map_err(|_| integration("cursor"))?;
            read::encode_cursor(binding, position)
                .map(Some)
                .map_err(|_: ReadRuleError| integration("cursor"))
        }
        _ => Ok(None),
    }
}

/// A store failure before or during the snapshot: fail closed, never a stale answer
/// (08 §2.2 *Fail closed on store unavailability*).
fn store_unavailable(
    operation: &'static str,
    order_id: Option<Uuid>,
    error: &anyhow::Error,
) -> OrdersError {
    tracing::warn!(
        target: "orders.read.store",
        operation,
        order_id = order_id.map(tracing::field::display),
        error = %error,
        "read store unavailable"
    );
    OrdersError::Refused(Reason::ReadStoreUnavailable)
}

/// The access-log row from trusted inputs only (08 §3.7): immutable subject UUID and class,
/// the catalog operation, outcome, requested/resolved target, public reason, operational-only
/// proof detail and the supplied proof reference recorded as supplied. `accessed_at` is a
/// placeholder: the writer stamps the database clock inside the log transaction.
fn log_row(
    caller: &Caller,
    actor: &AuditActor,
    operation: &'static str,
    outcome: AccessOutcome,
    target: Option<Target>,
    refusal: Option<Refusal>,
) -> read_access_log::Model {
    read_access_log::Model {
        access_id: Uuid::new_v4(),
        order_id: target.and_then(Target::order_id),
        requested_order_ref: target.map(|t| t.requested_order_ref),
        actor: actor.reference(),
        actor_class: actor.class().token().to_owned(),
        operation: operation.to_owned(),
        outcome: outcome.token().to_owned(),
        refusal_reason: refusal.map(|r| r.reason.mapping().reason.to_owned()),
        internal_refusal_detail: refusal
            .and_then(Refusal::internal_detail)
            .map(str::to_owned),
        delegation_proof_ref: caller.supplied_proof().map(|p| p.as_str().to_owned()),
        accessed_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

/// Append one row in its own short transaction under the subject-bound private scope; the
/// writer stamps `accessed_at` from the database clock of that transaction.
async fn append(db: &Db, caller: &Caller, row: read_access_log::Model) -> anyhow::Result<()> {
    let scope = PrivateScope::for_read_log(caller);
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            private::insert_read_access_log(tx, scope.access_scope(), row).await?;
            Ok(())
        })
    })
    .await
}

fn version_of(version: i32) -> Result<OrderVersion, OrdersError> {
    OrderVersion::try_from(i64::from(version)).map_err(|_| integration("version"))
}

fn is_draft(row: &order::Model) -> bool {
    row.state == OrderState::Draft.token()
}

fn draft_revision(row: &order::Model) -> Result<Option<DraftRevision>, OrdersError> {
    if is_draft(row) {
        DraftRevision::try_from(row.draft_revision)
            .map(Some)
            .map_err(|_| integration("draft revision"))
    } else {
        Ok(None)
    }
}

fn header(row: &order::Model) -> Result<OrderHeader, OrdersError> {
    Ok(OrderHeader {
        order_id: row.order_id,
        order_number: row.order_number.clone(),
        state: crate::domain::audit::parse_state(&row.state).map_err(|_| integration("state"))?,
        category: Category::parse(&row.category).ok_or_else(|| integration("category"))?,
        resource_tenant_id: row.resource_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        payer_tenant_id: row.payer_tenant_id,
        contract_id: row.contract_id,
        sales_path: SalesPath::from_token(&row.sales_path)
            .ok_or_else(|| integration("sales path"))?,
        created_at: row.created_at,
        state_entered_at: row.state_entered_at,
    })
}

fn summary(row: &order::Model) -> Result<OrderSummary, OrdersError> {
    Ok(OrderSummary {
        order: header(row)?,
        current_version: version_of(row.current_version)?,
        draft_revision: draft_revision(row)?,
    })
}

/// Compose the authorized current draft or submitted commercial snapshot.
fn compose_view(snapshot: &Snapshot) -> Result<OrderView, OrdersError> {
    let row = &snapshot.row;
    ensure_readable(row)?;
    let version = snapshot
        .version
        .as_ref()
        .ok_or_else(|| integration("current version row"))?;
    let lines = if is_draft(row) {
        compose_lines(&snapshot.identities, &snapshot.drafts)?
    } else {
        compose_committed_lines(row, &snapshot.identities, &snapshot.committed)?
    };
    if !is_draft(row) {
        validate_totals(&lines, &snapshot.totals)?;
    }
    Ok(OrderView {
        resolved_totals: if is_draft(row) {
            None
        } else {
            Some(
                snapshot
                    .totals
                    .iter()
                    .map(committed_total)
                    .collect::<Result<_, _>>()?,
            )
        },
        order: header(row)?,
        version: VersionSummary {
            version: version_of(version.version)?,
            supersedes_version: version.supersedes_version.map(version_of).transpose()?,
            created_at: version.created_at,
        },
        lines,
        draft_revision: draft_revision(row)?,
    })
}

/// Lines in identity order; every identity the membership subquery returned has its working
/// row in the same snapshot.
fn compose_lines(
    identities: &[order_line_identity::Model],
    drafts: &[draft_content::Model],
) -> Result<Vec<DraftLine>, OrdersError> {
    identities
        .iter()
        .map(|identity| {
            drafts
                .iter()
                .find(|row| row.line_id == identity.line_id)
                .ok_or_else(|| integration("draft membership"))
                .and_then(|row| draft_line(row.into()))
        })
        .collect()
}

/// The authored columns a working row and an immutable committed row share, borrowed from
/// either table so one decoder composes both and neither path builds the other's entity.
#[derive(Clone, Copy)]
struct AuthoredColumns<'a> {
    order_id: Uuid,
    line_id: Uuid,
    plan_id: Uuid,
    plan_revision_id: Uuid,
    selected_items: &'a serde_json::Value,
    currency: &'a str,
    contract_effective_date: Option<time::Date>,
    service_activation_date: Option<time::Date>,
    acceptance_due_date: Option<time::Date>,
    term_kind: &'a str,
    authored_term: &'a serde_json::Value,
    billing_cycle: Option<&'a str>,
}

impl<'a> From<&'a draft_content::Model> for AuthoredColumns<'a> {
    fn from(row: &'a draft_content::Model) -> Self {
        Self {
            order_id: row.order_id,
            line_id: row.line_id,
            plan_id: row.plan_id,
            plan_revision_id: row.plan_revision_id,
            selected_items: &row.selected_items,
            currency: &row.currency,
            contract_effective_date: row.contract_effective_date,
            service_activation_date: row.service_activation_date,
            acceptance_due_date: row.acceptance_due_date,
            term_kind: &row.term_kind,
            authored_term: &row.authored_term,
            billing_cycle: row.billing_cycle.as_deref(),
        }
    }
}

impl<'a> From<&'a order_line::Model> for AuthoredColumns<'a> {
    fn from(row: &'a order_line::Model) -> Self {
        Self {
            order_id: row.order_id,
            line_id: row.line_id,
            plan_id: row.plan_id,
            plan_revision_id: row.plan_revision_id,
            selected_items: &row.selected_items,
            currency: &row.currency,
            contract_effective_date: Some(row.contract_effective_date),
            service_activation_date: row.service_activation_date,
            acceptance_due_date: row.acceptance_due_date,
            term_kind: &row.term_kind,
            authored_term: &row.authored_term,
            billing_cycle: Some(&row.billing_cycle),
        }
    }
}

fn draft_line(cols: AuthoredColumns<'_>) -> Result<DraftLine, OrdersError> {
    let fail = |column, category| line_integration(cols.order_id, cols.line_id, column, category);
    let selected_items = Vec::<SelectedItem>::deserialize(cols.selected_items)
        .map_err(|error| fail("selected_items", Some(serde_category(&error))))?;
    let term_duration =
        term::stored_term(cols.term_kind, cols.authored_term).map_err(|error| match error {
            StoredShapeError::Undecodable { category, .. } => fail("authored_term", Some(category)),
            StoredShapeError::UnknownKind | StoredShapeError::Unrepresentable => {
                fail("term_kind", None)
            }
        })?;
    let billing_cycle = cols
        .billing_cycle
        .map(|token| BillingCycle::from_token(token).ok_or_else(|| fail("billing_cycle", None)))
        .transpose()?;
    Ok(DraftLine {
        committed: None,
        line_id: cols.line_id,
        plan_id: cols.plan_id,
        plan_revision_id: cols.plan_revision_id,
        selected_items,
        currency: Currency::try_from(cols.currency.to_owned())
            .map_err(|_| fail("currency", None))?,
        contract_effective_date: cols.contract_effective_date.map(CalendarDate),
        service_activation_date: cols.service_activation_date.map(CalendarDate),
        acceptance_due_date: cols.acceptance_due_date.map(CalendarDate),
        term_duration,
        billing_cycle,
    })
}

/// A page size from an untyped boundary value (REST): outside 1–200 is `page-size-exceeded`.
///
/// # Errors
/// `page-size-exceeded`.
pub fn page_size(value: Option<u64>) -> Result<Option<PageSize>, OrdersError> {
    value
        .map(PageSize::try_from)
        .transpose()
        .map_err(|_| OrdersError::Refused(Reason::PageSizeExceeded))
}

/// Only the delivered submitted representation is served outside draft.
fn ensure_readable(row: &order::Model) -> Result<(), OrdersError> {
    if is_draft(row) || row.state == OrderState::Submitted.token() {
        Ok(())
    } else {
        Err(not_delivered("committed state composition").into())
    }
}

fn compose_committed_lines(
    parent: &order::Model,
    identities: &[order_line_identity::Model],
    rows: &[order_line::Model],
) -> Result<Vec<DraftLine>, OrdersError> {
    identities
        .iter()
        .map(|identity| {
            let row = rows
                .iter()
                .find(|row| row.line_id == identity.line_id)
                .ok_or_else(|| integration("committed membership"))?;
            committed_line(parent, row)
        })
        .collect()
}

fn committed_line(
    parent: &order::Model,
    row: &order_line::Model,
) -> Result<DraftLine, OrdersError> {
    let fail = |check, cause: Option<&dyn std::fmt::Display>| {
        committed_integration(row.order_id, row.line_id, check, cause)
    };
    let value = row.order_pin.as_ref().ok_or_else(|| fail("pin", None))?;
    let bytes = serde_json::to_vec(value).map_err(|_| fail("pin", Some(&"re-encoding")))?;
    let pin = OrderPin::decode(&bytes).map_err(|error| fail("pin", Some(&error)))?;
    let receipt = pin.receipt();
    let query = &receipt.query;
    let axes = &query.tenant_axes;
    let request_digest = digest_hex(&receipt.request_digest);
    let terms_digest = digest_hex(&receipt.terms_digest);
    // Validate the independent row selection, not merely a self-consistent receipt; the first
    // failing check is the one logged.
    let checks = [
        (axes.seller_tenant_id == parent.seller_tenant_id, "seller"),
        (axes.payer_tenant_id == parent.payer_tenant_id, "payer"),
        (
            axes.resource_tenant_id == parent.resource_tenant_id,
            "resource",
        ),
        (query.order_id == row.order_id, "order"),
        (query.line_id == row.line_id, "line"),
        (
            i64::try_from(query.order_version).ok() == Some(i64::from(row.version)),
            "version",
        ),
        (query.plan_id == row.plan_id, "plan"),
        (query.plan_revision_id == row.plan_revision_id, "revision"),
        (
            row.pricing_acceptance_id == Some(receipt.acceptance_id),
            "acceptance",
        ),
        (
            row.pricing_request_digest.as_deref() == Some(request_digest.as_str()),
            "request_digest",
        ),
        (
            row.pricing_terms_digest.as_deref() == Some(terms_digest.as_str()),
            "terms_digest",
        ),
    ];
    if let Some(&(_, check)) = checks.iter().find(|(holds, _)| !holds) {
        return Err(fail(check, None));
    }
    let mut line = draft_line(row.into())?;
    let selected = crate::infra::gate::preview::gate_line(
        row.line_id,
        row.plan_id,
        row.plan_revision_id,
        &line.selected_items,
        &row.currency,
    );
    let version = u64::try_from(row.version).map_err(|_| fail("version", None))?;
    if !crate::domain::gate::commercial::verify_commit_line(query, row.order_id, version, &selected)
    {
        return Err(fail("authored selection", None));
    }
    line.committed = Some(bss_orders_sdk::reads::CommittedLine {
        commercial_attempt_id: row
            .commercial_attempt_id
            .ok_or_else(|| fail("attempt", None))?,
        order_pin: pin,
    });
    Ok(line)
}

fn committed_total(
    row: &resolved_total::Model,
) -> Result<bss_orders_sdk::reads::CommittedTotal, OrdersError> {
    // The entity keeps the Postgres `integer`; the DB CHECK (0..28) means this never fires.
    let currency_scale = u8::try_from(row.currency_scale).map_err(|e| {
        committed_integration(row.order_id, row.line_id, "currency_scale", Some(&e))
    })?;
    Ok(bss_orders_sdk::reads::CommittedTotal {
        scope: row.scope.clone(),
        line_id: row.line_id,
        currency: row.currency.clone(),
        assessment_id: row.assessment_id,
        currency_scale,
        exclusions: row.exclusions.clone(),
        item_breakdown: row.item_breakdown.clone(),
        recurring_by_cycle: row.recurring_by_cycle.clone(),
        amount_status: row.amount_status.clone(),
        amount_basis: row.amount_basis.clone(),
        period_evidence: row.period_evidence.clone(),
        gross_exact: row.gross_exact.clone(),
        gross_display: row.gross_display.clone(),
        net_exact: row.net_exact.clone(),
        net_display: row.net_display.clone(),
        discount_status: row.discount_status.clone(),
        discount_exact: row.discount_exact.clone(),
        discount_display: row.discount_display.clone(),
        promotion_ref: row.promotion_ref.clone(),
        charge_kind: row.charge_kind.clone(),
        tcv_exact: row.tcv_exact.clone(),
        tcv_display: row.tcv_display.clone(),
        tcv_basis: row.tcv_basis.clone(),
        tcv_evidence: row.tcv_evidence.clone(),
    })
}

/// A stored total's scope and charge kind as the domain enums, or the named check it fails.
fn total_tokens(
    total: &resolved_total::Model,
) -> Result<(TotalScope, EvaluationChargeKind), OrdersError> {
    let fail = |check| committed_integration(total.order_id, total.line_id, check, None);
    let scope = TotalScope::from_token(&total.scope).ok_or_else(|| fail("total scope"))?;
    let kind =
        EvaluationChargeKind::from_token(&total.charge_kind).ok_or_else(|| fail("charge kind"))?;
    if scope == TotalScope::Order && !total.line_id.is_nil() {
        return Err(fail("total scope"));
    }
    Ok((scope, kind))
}

/// Check membership and assessment provenance without recomputing producer amounts.
fn validate_totals(
    lines: &[DraftLine],
    totals: &[resolved_total::Model],
) -> Result<(), OrdersError> {
    let first = lines
        .first()
        .and_then(|line| line.committed.as_ref())
        .ok_or_else(|| integration("missing committed lines"))?;
    let assessment = first.order_pin.assessment_id();
    let totals = totals
        .iter()
        .map(|total| total_tokens(total).map(|(scope, kind)| (scope, kind, total)))
        .collect::<Result<Vec<_>, _>>()?;
    for line in lines {
        let evidence = line
            .committed
            .as_ref()
            .ok_or_else(|| integration("missing committed pin"))?;
        if evidence.order_pin.assessment_id() != assessment {
            return Err(integration("inconsistent committed assessments"));
        }
        if !totals
            .iter()
            .any(|(scope, _, total)| *scope == TotalScope::Line && total.line_id == line.line_id)
        {
            return Err(integration("missing committed line totals"));
        }
    }
    let mut line_kinds = BTreeSet::new();
    let mut order_kinds = BTreeSet::new();
    for &(scope, kind, _) in &totals {
        match scope {
            TotalScope::Line => line_kinds.insert(kind),
            TotalScope::Order => order_kinds.insert(kind),
        };
    }
    // Rating components are producer declarations, not inferred from Pricing binding kinds.
    // Every committed order also has the recurring TCV carrier, including usage-only orders.
    line_kinds.insert(EvaluationChargeKind::Recurring);
    if line_kinds != order_kinds {
        return Err(integration("incomplete committed totals"));
    }
    for (scope, _, total) in totals {
        let currency = match scope {
            TotalScope::Order => &first.order_pin.receipt().query.market.currency,
            TotalScope::Line => {
                &lines
                    .iter()
                    .find(|line| line.line_id == total.line_id)
                    .and_then(|line| line.committed.as_ref())
                    .ok_or_else(|| integration("total membership"))?
                    .order_pin
                    .receipt()
                    .query
                    .market
                    .currency
            }
        };
        if total.assessment_id != assessment || &total.currency != currency {
            return Err(integration("committed total provenance"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;

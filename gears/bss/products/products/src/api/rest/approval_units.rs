//! @cpt-dod:cpt-cf-bss-products-dod-unit-contended:p1
//! @cpt-dod:cpt-cf-bss-products-dod-stale-refresh-generation:p1
//! @cpt-dod:cpt-cf-bss-products-dod-sod-excludes-authors:p1
//! Approval queue and generation-bound decisions on the caller's transaction.
use super::{
    ApiState, TxError, category_tx_config,
    closed_sets::ProductsVoteOutcome,
    contention_db_err,
    dto::{
        ProductsApprovalUnitCounts, ProductsApprovalUnitKindCounts,
        ProductsApprovalUnitStateCounts, UnitCountsQuery, UnitDto, UnitList, UnitListQuery,
        VoteReceipt, VoteRequest,
    },
    governance as g, json_body, replay, require_authenticated, tx_to_canonical,
    unit_tx_to_canonical,
};
use crate::{
    authz::{actions, resource_types},
    domain::{
        approvals::{
            ApprovalKind, KIND_SKU_CHANGE, KIND_SKU_RETIRE, SkuProposal, Subject,
            change::SkuChange, publish::SkuPublish, retire::SkuRetire,
        },
        error::DomainError,
        recognized::UsageRefAnswer,
        sku::SkuPatch,
        validation::ValidationReport,
    },
    infra::{
        events::{self, TxOutbox},
        storage::repo,
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{
        Path, Query,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bss_approval::{
    ApprovalError, ApprovalSubject, ApproveOutcome, Engine, RejectOutcome, Store, Unit, UnitState,
};
use bss_products_sdk::models::SkuContent;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::CanonicalError,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_db::{DbTx, secure::AccessScope};
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub mod inbox_source;

#[derive(Clone, Copy)]
enum Vote {
    Approve,
    Reject,
    Withdraw,
}
/// Register the queue, card and three decision operations.
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::get("/bss-products/v1/approval-units")
        .operation_id("bss_products.list_approval_units")
        .summary("list_approval_units")
        .description(
            "One page of the tenant's approval units in submission order (P-D-224), filtered by \
             state, kind and SKU, each with its stored snapshot, the decisions of every \
             generation and caller_can_approve, whether the caller may approve it now (the \
             approval engine's rule, P-D-228). `$orderby=submitted_at desc` pages it newest first, \
             and `submitted_at asc`, the default, oldest first; the unit id breaks a tie in the \
             same direction (P-D-227). A client merging pages of several gears compares \
             submitted_at as an instant, never as text, then the id as lower-case hex. `limit` \
             (default 200, clamped at 500) and `cursor` from `page_info` page it; a cursor carries \
             its order, so a continuation sends no `$orderby`. Refusals: 400 for an unknown state, \
             a kind other than sku_publish, sku_change or sku_retire (on kind), or a query that \
             does not parse; 400 FILTER_MISMATCH for a cursor replayed with \
             another state, kind or SKU; 400 for a cursor that does not read; 400 \
             ORDER_WITH_CURSOR for `$orderby` beside a cursor; 400 INVALID_ORDERBY_FIELD for any \
             other order; 400 for any other query key (P-D-254).",
        )
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "state",
            false,
            "Unit state: pending, approved, rejected or withdrawn",
            "string",
        )
        .query_param_typed(
            "kind",
            false,
            "Approval kind: sku_publish, sku_change or sku_retire",
            "string",
        )
        .query_param_typed("ref_id", false, "SKU id (a UUID)", "string")
        .query_param_typed(
            "limit",
            false,
            "Page size (default 200, clamped at 500)",
            "integer",
        )
        .query_param_typed("cursor", false, "Continuation from page_info", "string")
        .with_odata_orderby::<UnitOrderField>()
        .handler(list)
        .json_response_with_schema::<UnitList>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-products/v1/approval-units/counts")
        .operation_id("bss_products.count_approval_units")
        .summary("count_approval_units")
        .description(
            "Counts the tenant's approval units that the list's narrowing keeps (state, kind and \
             SKU), under the list's own grant: by_state (pending, approved, rejected, withdrawn), \
             by_kind (sku_publish, sku_change, sku_retire), each named with 0 when none, and \
             total, the length of the list under the same narrowing (P-D-227). It reads one \
             grouped statement, whatever the number of units. It takes nothing but the narrowing. \
             Refusals: the list's: 400 for an unknown state, a kind other than sku_publish, \
             sku_change or sku_retire (on kind), or a query that does not parse, and 400 for any \
             other key (limit, cursor, $orderby).",
        )
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "state",
            false,
            "Unit state: pending, approved, rejected or withdrawn",
            "string",
        )
        .query_param_typed(
            "kind",
            false,
            "Approval kind: sku_publish, sku_change or sku_retire",
            "string",
        )
        .query_param_typed("ref_id", false, "SKU id (a UUID)", "string")
        .handler(counts)
        .json_response_with_schema::<ProductsApprovalUnitCounts>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-products/v1/approval-units/{id}")
        .operation_id("bss_products.get_approval_unit")
        .summary("get_approval_unit")
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .path_param("id", "Unit id")
        .handler(get)
        .json_response_with_schema::<UnitDto>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-products/v1/approval-units/{id}/approve")
        .operation_id("bss_products.approve_unit")
        .summary("approve_unit")
        .description(
            "Approves the unit at the generation its reviewer saw. The body is generation and an \
             optional note; a missing generation is 400 GENERATION_REQUIRED and any other key is \
             400 BODY_UNEXPECTED (P-D-253). The note is at most 2000 \
             characters (the approval engine's cap). The apply judges the SKU as it is then: a \
             published usage SKU keeps its usage type and its unit (P-D-258), except a raw meter \
             moving onto the identity wrapper of that meter in the same unit (P-D-251). Every \
             other move of the ref or the unit, or a type change away from usage, is 409 \
             METERING_IMMUTABLE, on usage_type_ref when the ref moves and on unit when only the \
             unit moves. Refusals include 400 NOTE_TOO_LONG on \
             a longer note, 400 GENERATION_MISMATCH, 400 UNIT_STALE after a refresh, 403 \
             SOD_VIOLATION, 409 DUPLICATE_VOTE, and the apply's 409 METERING_IMMUTABLE, \
             DERIVED_UNIT_MISMATCH or DERIVED_USAGE_TYPE_UNKNOWN.",
        )
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .path_param("id", "Unit id")
        .json_request::<VoteRequest>(openapi, "Generation reviewed and optional note")
        .param(replay::param())
        .handler(approve)
        .json_response_with_schema::<VoteReceipt>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-products/v1/approval-units/{id}/reject")
        .operation_id("bss_products.reject_unit")
        .summary("reject_unit")
        .description(
            "Rejects the unit at the generation its reviewer saw; a reject needs a note. The \
             note is at most 2000 characters (the approval engine's cap). A reject judges no \
             separation of duties: the submitter and the SKU's creator may reject (P-D-228). \
             Refusals include 400 NOTE_TOO_LONG on a longer note and 400 NOTE_REQUIRED without \
             one, 400 GENERATION_MISMATCH, 400 UNIT_STALE after a refresh, and 409 DUPLICATE_VOTE \
             or UNIT_ALREADY_DECIDED.",
        )
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .path_param("id", "Unit id")
        .json_request::<VoteRequest>(openapi, "Generation reviewed and optional note")
        .param(replay::param())
        .handler(reject)
        .json_response_with_schema::<VoteReceipt>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-products/v1/approval-units/{id}/withdraw")
        .operation_id("bss_products.withdraw_unit")
        .summary("withdraw_unit")
        .description(
            "The submitter withdraws a pending unit. The body is empty or {}. Any other body is \
             400 BODY_UNEXPECTED and the unit stays pending. The idempotency digest is the body \
             sent, so an empty body and {} do not share a row (P-D-253).",
        )
        .tag("Approval units")
        .authenticated()
        .no_license_required()
        .path_param("id", "Unit id")
        .param(replay::param())
        .handler(withdraw)
        .json_response_with_schema::<VoteReceipt>(openapi, StatusCode::OK, "Result")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(state))
}
async fn approve(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::APPROVE,
    )
    .await?;
    let body = json_body(body)?;
    let request = parse_vote(&body)?;
    vote(
        &enforcer,
        state,
        scope,
        ctx,
        id,
        Vote::Approve,
        Some(request),
        body,
        headers,
    )
    .await
}
async fn reject(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::APPROVE,
    )
    .await?;
    let body = json_body(body)?;
    let request = parse_vote(&body)?;
    vote(
        &enforcer,
        state,
        scope,
        ctx,
        id,
        Vote::Reject,
        Some(request),
        body,
        headers,
    )
    .await
}
async fn withdraw(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::SUBMIT,
    )
    .await?;
    let digest = withdraw_body(&body)?;
    vote(
        &enforcer,
        state,
        scope,
        ctx,
        id,
        Vote::Withdraw,
        None,
        digest,
        headers,
    )
    .await
}
async fn list(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    query: Result<Query<UnitListQuery>, QueryRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::READ,
    )
    .await?;
    let Query(q) =
        query.map_err(|e| CanonicalError::from(g::validation("query", e.to_string())))?;
    let filter = narrowing(q.state.as_deref(), q.kind.as_deref(), q.ref_id)?;
    let page = unit_page(&filter, q.limit, q.cursor.as_deref(), q.orderby.as_deref())?;
    let approve_scope = g::grant_scope(&enforcer, &ctx, actions::APPROVE).await?;
    let submit_scope = g::grant_scope(&enforcer, &ctx, actions::SUBMIT).await?;
    let mut list = page_of(
        &state,
        scope,
        &ctx,
        filter,
        page,
        approve_scope,
        submit_scope,
    )
    .await?;
    // P-D-262: the page's submitters and voters in one lookup, after the page's transaction.
    state.actor_names.fill(&ctx, &mut list).await;
    Ok(Json(list).into_response())
}
/// The list door's read (P-D-224, P-D-228): one page under `scope` and `filter`, in the order
/// `page` carries (`unit_page` puts it on the query, a cursor carries its own), as `ctx` reads it.
/// The approvals inbox's source reads its page here too (P-D-250).
async fn page_of(
    state: &ApiState,
    scope: AccessScope,
    ctx: &SecurityContext,
    filter: repo::UnitListFilter,
    page: toolkit_odata::ODataQuery,
    approve_scope: AccessScope,
    submit_scope: AccessScope,
) -> Result<UnitList, CanonicalError> {
    let (tenant, reader) = (ctx.subject_tenant_id(), ctx.subject_id());
    state
        .db
        .db()
        .transaction_with_retry(category_tx_config(state), contention_db_err, move |tx| {
            let (scope, filter, page) = (scope.clone(), filter.clone(), page.clone());
            let (approve_scope, submit_scope) = (approve_scope.clone(), submit_scope.clone());
            Box::pin(async move {
                // One page, all its units' decisions and all their items, one read each
                // (P-D-224, P-D-228): the same statements whatever the page's size.
                let page = repo::page_units(tx, &scope, tenant, &filter, &page)
                    .await
                    .map_err(|e| match e {
                        repo::UnitListError::Query(e) => TxError::OData(e),
                        repo::UnitListError::Repo(e) => TxError::Repo(e),
                    })?;
                let ids: Vec<Uuid> = page.items.iter().map(|u| u.id).collect();
                let mut decisions = repo::decisions_of_units(tx, &scope, tenant, &ids)
                    .await
                    .map_err(TxError::Repo)?;
                // The items' authors alone: the flag judges who authored an item, never its
                // content (the phase 9 review's R73, R74).
                let mut authored = repo::item_authors_of_units(tx, &scope, tenant, &ids)
                    .await
                    .map_err(TxError::Repo)?;
                let items = page
                    .items
                    .into_iter()
                    .map(|unit| {
                        let id = unit.id;
                        UnitDto::of(
                            unit,
                            &authored.remove(&id).unwrap_or_default(),
                            decisions.remove(&id).unwrap_or_default(),
                            reader,
                            &approve_scope,
                            &submit_scope,
                        )
                        .map_err(TxError::Repo)
                    })
                    .collect::<Result<_, _>>()?;
                Ok(UnitList {
                    items,
                    page_info: page.page_info,
                })
            })
        })
        .await
        .map_err(tx_to_canonical)
}
/// The unit list's narrowing, which the counts take too (P-D-224, P-D-227): a known state (else
/// 400 on `state`), a kind products records (else 400 on `kind`) and the SKU.
fn narrowing(
    state: Option<&str>,
    kind: Option<&str>,
    ref_id: Option<Uuid>,
) -> Result<repo::UnitListFilter, CanonicalError> {
    let state = state
        .map(|s| {
            UnitState::parse(s)
                .ok_or_else(|| CanonicalError::from(g::validation("state", "unknown unit state")))
        })
        .transpose()?;
    let kind = kind
        .map(|k| {
            ApprovalKind::parse(k).ok_or_else(|| {
                CanonicalError::from(g::validation(
                    "kind",
                    "unknown approval kind: sku_publish, sku_change or sku_retire",
                ))
            })
        })
        .transpose()?;
    Ok(repo::UnitListFilter {
        state,
        kind,
        ref_id,
    })
}
/// The one field the unit list's `$orderby` takes (P-D-227): `submitted_at`, ascending or
/// descending. The unit id breaks a tie in the same direction; it is the pager's tie-break, not a
/// client key, so it is not declared. `.with_odata_orderby` publishes it in the served contract
/// (`x-odata-orderby`), and [`unit_order`] accepts exactly it (the phase 9 review's theme I).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UnitOrderField {
    SubmittedAt,
}
impl UnitOrderField {
    const fn field(self) -> repo::UnitListField {
        match self {
            Self::SubmittedAt => repo::UnitListField::SubmittedAt,
        }
    }
}
impl toolkit_odata::filter::FilterField for UnitOrderField {
    const FIELDS: &'static [Self] = &[Self::SubmittedAt];
    fn name(&self) -> &'static str {
        toolkit_odata::filter::FilterField::name(&self.field())
    }
    fn kind(&self) -> toolkit_odata::filter::FieldKind {
        toolkit_odata::filter::FilterField::kind(&self.field())
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS
            .iter()
            .copied()
            .find(|f| toolkit_odata::filter::FilterField::name(f) == name)
    }
}
/// The unit list's order (P-D-227): `submitted_at` ascending (also when omitted, P-D-224) or
/// descending, the id breaking a tie in the same direction. Any other `$orderby` is 400
/// `INVALID_ORDERBY_FIELD`, the toolkit's refusal of an order a list does not take. The list parses
/// its own query rather than take the toolkit's `OData` extractor, which would refuse `limit=0`
/// (the house pager reads one unit), drop a cursor's cause from its refusal, and bind the `$`
/// options this list does not read (theme I).
fn unit_order(orderby: Option<&str>) -> Result<toolkit_odata::SortDir, CanonicalError> {
    let Some(raw) = orderby else {
        return Ok(toolkit_odata::SortDir::Asc);
    };
    let order = toolkit::api::odata::parse_orderby(raw).map_err(CanonicalError::from)?;
    let declared = |key: &toolkit_odata::OrderKey| {
        <UnitOrderField as toolkit_odata::filter::FilterField>::from_name(&key.field).is_some()
    };
    match order.0.as_slice() {
        [] => Ok(toolkit_odata::SortDir::Asc),
        [key] if declared(key) => Ok(key.dir),
        // The refusal names the key it refuses, never the whole order, so a supported field is
        // never called unsupported (the phase 9 review's R67).
        keys => Err(toolkit_odata::Error::InvalidOrderByField(
            keys.iter().find(|key| !declared(key)).map_or_else(
                || "only one key, submitted_at, is accepted".to_owned(),
                |key| key.field.clone(),
            ),
        )
        .into()),
    }
}
/// The unit list's page (P-D-224): `limit`, and `cursor` from a page's `page_info`, which carries a
/// hash of the narrowing (`state`, `kind` and `ref_id`), so a cursor replayed under another is 400
/// `FILTER_MISMATCH`, as pricing's list's is (D-458). The order is not part of the hash
/// (P-D-227): a cursor carries its own (`CursorV1.s`) and a continuation follows it, so every
/// cursor minted before the descending order still reads. `$orderby` beside a cursor is the toolkit's 400
/// `ORDER_WITH_CURSOR`, judged first, as its `OData` extractor does. Without a cursor, the query
/// carries the order (`repo::submission_order`), the one source the repository reads (the phase 9
/// review's R36).
fn unit_page(
    filter: &repo::UnitListFilter,
    limit: Option<u64>,
    cursor: Option<&str>,
    orderby: Option<&str>,
) -> Result<toolkit_odata::ODataQuery, CanonicalError> {
    if cursor.is_some() && orderby.is_some() {
        return Err(toolkit_odata::Error::OrderWithCursor.into());
    }
    let direction = unit_order(orderby)?;
    let narrowing = serde_json::json!({
        "state": filter.state.map(UnitState::as_str),
        "kind": filter.kind.map(ApprovalKind::as_str),
        "ref_id": filter.ref_id,
    });
    let hash = super::sku_list::cursor_hash(&narrowing);
    let mut query = toolkit_odata::ODataQuery::new().with_filter_hash(hash.clone());
    if let Some(limit) = limit {
        query = query.with_limit(limit);
    }
    if let Some(token) = cursor {
        let cursor = toolkit_odata::CursorV1::decode(token).map_err(CanonicalError::from)?;
        if cursor.f.as_deref() != Some(hash.as_str()) {
            return Err(toolkit_odata::Error::FilterMismatch.into());
        }
        query = query.with_cursor(cursor);
    } else {
        query = query.with_order(repo::submission_order(direction));
    }
    Ok(query)
}
/// `GET /approval-units/counts` (P-D-227): under the list's grant, the list's narrowing, counted
/// by state and kind in ONE grouped statement, read outside any transaction.
async fn counts(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    query: Result<Query<UnitCountsQuery>, QueryRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::READ,
    )
    .await?;
    let Query(q) =
        query.map_err(|e| CanonicalError::from(g::validation("query", e.to_string())))?;
    let filter = narrowing(q.state.as_deref(), q.kind.as_deref(), q.ref_id)?;
    // One grouped statement is its own snapshot: it runs on the plain connection, never in the
    // serializable transaction category writes take, whose read locks over the scanned units
    // would push concurrent submits and votes into serialization failures (the phase 9 review's
    // R32).
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    let rows = repo::count_units(&conn, &scope, ctx.subject_tenant_id(), &filter)
        .await
        .map_err(|e| tx_to_canonical(TxError::Repo(e)))?;
    let mut by_state = ProductsApprovalUnitStateCounts::default();
    let mut by_kind = ProductsApprovalUnitKindCounts::default();
    let mut total = 0_u64;
    for row in rows {
        *match row.state {
            UnitState::Pending => &mut by_state.pending,
            UnitState::Approved => &mut by_state.approved,
            UnitState::Rejected => &mut by_state.rejected,
            UnitState::Withdrawn => &mut by_state.withdrawn,
        } += row.units;
        *match row.kind {
            ApprovalKind::SkuPublish => &mut by_kind.sku_publish,
            ApprovalKind::SkuChange => &mut by_kind.sku_change,
            ApprovalKind::SkuRetire => &mut by_kind.sku_retire,
        } += row.units;
        total += row.units;
    }
    Ok(Json(ProductsApprovalUnitCounts {
        by_state,
        by_kind,
        total,
    })
    .into_response())
}
/// A unit as `reader` reads it in a receipt: the decisions actually stored for all generations, and
/// whether `reader` may approve it over its stored items' authors (P-D-228), one statement each;
/// the items' content is not read (the phase 9 review's R70, R72).
pub(super) async fn as_read_by(
    tx: &DbTx<'_>,
    store: &repo::ProductsApprovalStore,
    unit: Unit,
    reader: Uuid,
    approve_scope: &AccessScope,
    submit_scope: &AccessScope,
) -> Result<UnitDto, TxError> {
    let authors = authors_of(tx, store, unit.id).await?;
    let decisions = store.decisions(tx, unit.id).await?;
    UnitDto::of(
        unit,
        &authors,
        decisions,
        reader,
        approve_scope,
        submit_scope,
    )
    .map_err(TxError::Repo)
}
/// The authors of one unit's stored items, in one statement that reads nothing else.
async fn authors_of(
    tx: &DbTx<'_>,
    store: &repo::ProductsApprovalStore,
    unit: Uuid,
) -> Result<Vec<Uuid>, TxError> {
    Ok(
        repo::item_authors_of_units(tx, &store.scope, store.tenant_id, &[unit])
            .await
            .map_err(TxError::Repo)?
            .remove(&unit)
            .unwrap_or_default(),
    )
}

/// `GET /approval-units/{id}`: the card, then its names (P-D-262).
async fn get(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let mut card = card(&state, &enforcer, &ctx, id).await?;
    // P-D-262: the submitter, the voters and the live SKU's creator and archiver, after the
    // transaction.
    state.actor_names.fill(&ctx, &mut card).await;
    Ok(Json(card).into_response())
}

/// The card door's read for an authenticated caller, before any name is filled. The inbox source
/// answers this unnamed card, and the inbox names it once (AP-D-11): one lookup per inbox card.
async fn card(
    state: &Arc<ApiState>,
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<UnitDto, CanonicalError> {
    let scope = g::scope(enforcer, ctx, &resource_types::APPROVAL_UNIT, actions::READ).await?;
    let approve_scope = g::grant_scope(enforcer, ctx, actions::APPROVE).await?;
    let submit_scope = g::grant_scope(enforcer, ctx, actions::SUBMIT).await?;
    let ttl = state.fence_ttl_minutes;
    let caller = ctx.clone();
    state
        .db
        .db()
        .transaction_with_retry(category_tx_config(state), contention_db_err, move |tx| {
            let scope = scope.clone();
            let ctx = caller.clone();
            let approve_scope = approve_scope.clone();
            let submit_scope = submit_scope.clone();
            Box::pin(async move {
                let store = repo::ProductsApprovalStore {
                    scope: scope.clone(),
                    tenant_id: ctx.subject_tenant_id(),
                };
                let unit = load(tx, &store, id).await?;
                let scope = AccessScope::for_tenant(unit.tenant_id);
                g::expire(
                    tx,
                    &scope,
                    ctx.subject_tenant_id(),
                    unit.ref_id,
                    ttl,
                    crate::infra::storage::stored_now(),
                )
                .await?;
                // A never-published draft may be deleted after its unit was rejected or withdrawn
                // (P-D-206): the unit stays readable, with no live SKU to recompute against.
                let live = repo::find_sku(tx, &scope, ctx.subject_tenant_id(), unit.ref_id)
                    .await
                    .map_err(TxError::Repo)?;
                let authors = authors_of(tx, &store, id).await?;
                let decisions = store.decisions(tx, id).await?;
                let mut dto = UnitDto::of(
                    unit,
                    &authors,
                    decisions,
                    ctx.subject_id(),
                    &approve_scope,
                    &submit_scope,
                )
                .map_err(TxError::Repo)?;
                dto.impact_live = live
                    .map(|live| {
                        serde_json::to_value(super::dto::SkuDto::from(live))
                            .map_err(|e| TxError::from(ApprovalError::Store(e.to_string())))
                    })
                    .transpose()?;
                Ok(dto)
            })
        })
        .await
        .map_err(tx_to_canonical)
}
async fn load(
    tx: &DbTx<'_>,
    store: &repo::ProductsApprovalStore,
    id: Uuid,
) -> Result<Unit, TxError> {
    store
        .unit(tx, id)
        .await?
        .ok_or(TxError::Refused(DomainError::NotFound {
            what: "approval_unit",
            id,
        }))
}
fn proposal(value: &serde_json::Value) -> Result<SkuProposal, TxError> {
    serde_json::from_value(value.clone())
        .map_err(|e| TxError::from(ApprovalError::Store(e.to_string())))
}
/// Recover only fields changed by the original proposal, leaving untouched live fields visible to
/// refresh. Every field of the patch is written out and every field of the content named, so a
/// field added to `SkuPatch` or `SkuContent` is a compile error here, never a silent `None` (RS-50).
fn patch_between(before: &SkuProposal, after: &SkuProposal) -> SkuPatch {
    /// The proposal's value where it differs from the one before it.
    fn changed<T: PartialEq + Clone>(a: &T, b: &T) -> Option<T> {
        (a != b).then(|| b.clone())
    }
    let SkuContent {
        code: _,
        name,
        r#type,
        category_id,
        description,
        sellable,
        gl_code,
        tax_category,
        invoice_line_template,
        billing_timing,
        usage_type_ref,
        unit,
    } = &before.content;
    let b = &after.content;
    SkuPatch {
        name: changed(name, &b.name),
        category_id: changed(category_id, &b.category_id),
        description: changed(description, &b.description),
        sellable: changed(sellable, &b.sellable),
        gl_code: changed(gl_code, &b.gl_code),
        tax_category: changed(tax_category, &b.tax_category),
        invoice_line_template: changed(invoice_line_template, &b.invoice_line_template),
        billing_timing: changed(billing_timing, &b.billing_timing),
        usage_type_ref: changed(usage_type_ref, &b.usage_type_ref),
        unit: changed(unit, &b.unit),
        lifecycle: after.lifecycle,
        r#type: changed(r#type, &b.r#type),
    }
}
async fn subject(
    outbox: &TxOutbox,
    tx: &DbTx<'_>,
    store: &repo::ProductsApprovalStore,
    ctx: &SecurityContext,
    unit: &Unit,
    usage: Option<UsageRefAnswer>,
) -> Result<Subject, TxError> {
    let sku_scope = AccessScope::for_tenant(store.tenant_id);
    let base = SkuPublish {
        scope: sku_scope.clone(),
        tenant_id: store.tenant_id,
        outbox: outbox.clone(),
        actor: ctx.subject_id(),
        now: crate::infra::storage::stored_now(),
        usage_type: usage,
    };
    let fence = repo::find_sku_fence(tx, &sku_scope, store.tenant_id, unit.ref_id)
        .await
        .map_err(TxError::Repo)?
        .ok_or(TxError::Refused(DomainError::NotFound {
            what: "sku",
            id: unit.ref_id,
        }))?;
    // The repository read the kind through its closed set; a kind outside it stays a store
    // failure here, never judged as another kind.
    let kind = ApprovalKind::parse(&unit.kind)
        .ok_or_else(|| TxError::from(ApprovalError::Store("unknown approval kind".into())))?;
    match kind {
        ApprovalKind::SkuPublish => Ok(Subject::Publish(base)),
        ApprovalKind::SkuRetire => Ok(Subject::Retire(SkuRetire {
            base,
            fence_op_id: fence
                .fence_op_id
                .ok_or_else(|| g::conflict("SKU_FENCED", "retire fence missing"))?,
        })),
        ApprovalKind::SkuChange => {
            let items = store.items(tx, unit.id).await?;
            let item = items
                .first()
                .ok_or_else(|| TxError::from(ApprovalError::Empty))?;
            let after = proposal(&item.after)?;
            let before = proposal(item.before.as_ref().ok_or_else(|| {
                TxError::from(ApprovalError::Store("change before missing".into()))
            })?)?;
            let mut patch = patch_between(&before, &after);
            if fence.type_change_pending {
                patch.r#type = Some(after.content.r#type);
            }
            Ok(Subject::Change(SkuChange {
                base,
                patch,
                effective_from: unit.common_effective_date.ok_or_else(|| {
                    TxError::from(ApprovalError::Store("change date missing".into()))
                })?,
                fence_op_id: if fence.type_change_pending {
                    fence.fence_op_id
                } else {
                    None
                },
            }))
        }
    }
}
async fn proposed(subject: &Subject, tx: &DbTx<'_>, unit: &Unit) -> Result<SkuContent, TxError> {
    let items = subject.collect(tx, &[unit.ref_id]).await?;
    let first = items
        .first()
        .ok_or_else(|| TxError::from(ApprovalError::Empty))?;
    if unit.kind == KIND_SKU_CHANGE {
        Ok(proposal(&first.after)?.content)
    } else {
        serde_json::from_value(first.after.clone())
            .map_err(|e| TxError::from(ApprovalError::Store(e.to_string())))
    }
}
/// Approve and reject take `generation` and an optional `note`. A missing generation is 400
/// `GENERATION_REQUIRED`. Any other key is 400 `BODY_UNEXPECTED` on that key (P-D-253).
fn parse_vote(value: &serde_json::Value) -> Result<VoteRequest, CanonicalError> {
    let Some(fields) = value.as_object() else {
        return Err(vote_violation(
            "body",
            "VALIDATION",
            "the body is a JSON object",
        ));
    };
    if let Some(key) = fields
        .keys()
        .find(|key| key.as_str() != "generation" && key.as_str() != "note")
    {
        return Err(vote_violation(
            key,
            "BODY_UNEXPECTED",
            "this field is not part of the vote",
        ));
    }
    if !fields.contains_key("generation") {
        return Err(vote_violation(
            "generation",
            "GENERATION_REQUIRED",
            "generation is required",
        ));
    }
    serde_json::from_value(value.clone())
        .map_err(|error| vote_violation("body", "VALIDATION", &error.to_string()))
}

/// Withdraw accepts an empty body or `{}` and nothing else. The two digest as different payloads,
/// so they do not share an idempotency row (P-D-253).
fn withdraw_body(body: &[u8]) -> Result<serde_json::Value, CanonicalError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Value::Null);
    }
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| vote_violation("body", "VALIDATION", &error.to_string()))?;
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(value)
    } else {
        Err(vote_violation(
            "body",
            "BODY_UNEXPECTED",
            "withdraw takes an empty body or {}",
        ))
    }
}

fn vote_violation(field: &str, code: &'static str, detail: &str) -> CanonicalError {
    let mut report = ValidationReport::new();
    report.violate(code, field, detail);
    DomainError::Validation(report).into()
}

/// @cpt-cf-bss-products-fr-concurrency-idempotency
#[expect(
    clippy::too_many_lines,
    reason = "the vote door keeps grant, replay, resolve and receipt in one sequence"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the derived pin needs the same enforcer the door already judged"
)]
async fn vote(
    enforcer: &PolicyEnforcer,
    state: Arc<ApiState>,
    scope: AccessScope,
    ctx: SecurityContext,
    id: Uuid,
    action: Vote,
    body: Option<VoteRequest>,
    digest: serde_json::Value,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let suffix = match action {
        Vote::Approve => "approve",
        Vote::Reject => "reject",
        Vote::Withdraw => "withdraw",
    };
    let claim = replay::input(
        &state,
        &headers,
        format!("/bss-products/v1/approval-units/{id}/{suffix}"),
        &digest,
    )?;
    // Check resource authorization even for a receipt replay, without requiring Pending.
    let conn = state.db.conn().map_err(|e| tx_to_canonical(e.into()))?;
    if repo::find_unit(&conn, &scope, ctx.subject_tenant_id(), id)
        .await
        .map_err(|e| tx_to_canonical(e.into()))?
        .is_none()
    {
        return Err(DomainError::NotFound {
            what: "approval_unit",
            id,
        }
        .into());
    }
    if let Some(response) = replay::lookup(&conn, ctx.subject_tenant_id(), claim.as_ref())
        .await
        .map_err(tx_to_canonical)?
    {
        return Ok(response);
    }
    let mut resolved_ref = None;
    let mut usage = None;
    if matches!(action, Vote::Approve) {
        let content = review_content(&state, &scope, &ctx, id).await?;
        if let Some(content) = content {
            resolved_ref = content.usage_type_ref.clone();
            usage = g::resolve(&state, enforcer, &ctx, &content).await?;
        }
    }
    let approve_scope = if matches!(action, Vote::Approve | Vote::Reject) {
        scope.clone()
    } else {
        g::grant_scope(enforcer, &ctx, actions::APPROVE).await?
    };
    let submit_scope = if matches!(action, Vote::Withdraw) {
        scope.clone()
    } else {
        g::grant_scope(enforcer, &ctx, actions::SUBMIT).await?
    };
    let seen = body.as_ref().map(|b| b.generation);
    let note = body.and_then(|b| b.note);
    let (db, sink, config) = (
        state.db.db(),
        state.sink.clone(),
        category_tx_config(&state),
    );
    let result = events::transaction(&db, &sink, config, contention_db_err, move |tx, outbox| {
        let scope = scope.clone();
        let ctx = ctx.clone();
        let usage = usage.clone();
        let resolved_ref = resolved_ref.clone();
        let note = note.clone();
        let claim = claim.clone();
        let approve_scope = approve_scope.clone();
        let submit_scope = submit_scope.clone();
        Box::pin(async move {
            let store = repo::ProductsApprovalStore {
                scope: scope.clone(),
                tenant_id: ctx.subject_tenant_id(),
            };
            let mut unit = load(tx, &store, id).await?;
            if let Some(response) =
                replay::begin(tx, ctx.subject_tenant_id(), claim.as_ref()).await?
            {
                return Ok(response);
            }
            if unit.state != UnitState::Pending {
                return Err(ApprovalError::AlreadyDecided.into());
            }
            let sub = subject(&outbox, tx, &store, &ctx, &unit, usage).await?;
            // P-D-249: snapshot `lifecycle_next` before the act.
            let (found, before_next) =
                lifecycle_before(tx, ctx.subject_tenant_id(), unit.ref_id).await?;
            let now = crate::infra::storage::stored_now();
            let outcome = match action {
                Vote::Approve => {
                    if unit.kind != KIND_SKU_RETIRE
                        && proposed(&sub, tx, &unit).await?.usage_type_ref != resolved_ref
                    {
                        return Err(g::conflict(
                            "STALE_REVISION",
                            "meter changed during resolution; retry",
                        ));
                    }
                    Engine::approve(
                        &store,
                        &sub,
                        tx,
                        id,
                        ctx.subject_id(),
                        seen.ok_or_else(|| {
                            TxError::Refused(g::validation("generation", "required"))
                        })?,
                        note.as_deref(),
                        now,
                    )
                    .await?
                }
                Vote::Reject => {
                    let note = note
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or(ApprovalError::NoteRequired)?;
                    let seen = seen
                        .ok_or_else(|| TxError::Refused(g::validation("generation", "required")))?;
                    // A stale unit is refreshed without applying or resolving the catalog.
                    match Engine::reject(&store, &sub, tx, id, ctx.subject_id(), seen, note, now)
                        .await?
                    {
                        RejectOutcome::Refreshed { generation } => {
                            ApproveOutcome::Refreshed { generation }
                        }
                        RejectOutcome::Rejected => ApproveOutcome::Applied,
                    }
                }
                Vote::Withdraw => {
                    Engine::withdraw(&store, &sub, tx, id, ctx.subject_id(), now).await?;
                    ApproveOutcome::Applied
                }
            };
            let (label, have, need) = match outcome {
                ApproveOutcome::Refreshed { generation } => {
                    decision_audit(
                        tx,
                        &ctx,
                        Audited::Refreshed,
                        &unit,
                        found,
                        before_next,
                        None,
                        now,
                    )
                    .await?;
                    let mut problem = toolkit::api::canonical_prelude::Problem::from(
                        CanonicalError::from(DomainError::StaleUnit { generation }),
                    );
                    problem.context["generation"] = serde_json::json!(generation);
                    return replay::finish(
                        tx,
                        ctx.subject_tenant_id(),
                        claim.as_ref(),
                        StatusCode::BAD_REQUEST,
                        &problem,
                    )
                    .await;
                }
                ApproveOutcome::Pending { have, need } => {
                    (ProductsVoteOutcome::Pending, Some(have), Some(need))
                }
                ApproveOutcome::Applied => (
                    match action {
                        Vote::Approve => ProductsVoteOutcome::Applied,
                        Vote::Reject => ProductsVoteOutcome::Rejected,
                        Vote::Withdraw => ProductsVoteOutcome::Withdrawn,
                    },
                    None,
                    None,
                ),
            };
            decision_audit(
                tx,
                &ctx,
                Audited::Vote(label),
                &unit,
                found,
                before_next,
                note,
                now,
            )
            .await?;
            if matches!(outcome, ApproveOutcome::Applied) {
                unit = load(tx, &store, id).await?;
                g::decided(&outbox, tx, &store, &unit, ctx.subject_id()).await?;
            }
            let receipt = VoteReceipt {
                have,
                need,
                outcome: label,
                unit: as_read_by(
                    tx,
                    &store,
                    unit,
                    ctx.subject_id(),
                    &approve_scope,
                    &submit_scope,
                )
                .await?,
            };
            replay::finish(
                tx,
                ctx.subject_tenant_id(),
                claim.as_ref(),
                StatusCode::OK,
                &receipt,
            )
            .await
        })
    })
    .await;
    match result {
        Ok(receipt) => Ok(receipt),
        Err(TxError::GenerationMismatch { seen, current }) => Ok(g::generation_problem(
            DomainError::from(ApprovalError::GenerationMismatch { seen, current }).into(),
            current,
        )),
        Err(e) => Err(unit_tx_to_canonical(e)),
    }
}

/// What a decision's audit row records: a vote's outcome, or the stale refresh a vote made.
#[derive(Clone, Copy)]
enum Audited {
    Refreshed,
    Vote(ProductsVoteOutcome),
}
impl Audited {
    /// The audit action, one per outcome in an exhaustive match (RS-21): a new outcome is a
    /// compile error here, never an `approval.approved` by default.
    const fn action(self) -> &'static str {
        match self {
            Self::Refreshed => "approval.refreshed",
            Self::Vote(ProductsVoteOutcome::Pending) => "approval.vote",
            Self::Vote(ProductsVoteOutcome::Applied) => "approval.approved",
            Self::Vote(ProductsVoteOutcome::Rejected) => "approval.rejected",
            Self::Vote(ProductsVoteOutcome::Withdrawn) => "approval.withdrawn",
        }
    }
}
/// The SKU's lifecycle in force, and its `lifecycle_next`, before an act (P-D-249).
async fn lifecycle_before(
    tx: &DbTx<'_>,
    tenant: Uuid,
    id: Uuid,
) -> Result<
    (
        bss_products_sdk::models::Lifecycle,
        Option<bss_products_sdk::models::LifecycleNext>,
    ),
    TxError,
> {
    let before = g::find(tx, &AccessScope::for_tenant(tenant), tenant, id).await?;
    Ok((before.lifecycle, before.lifecycle_next))
}
/// A decision's audit row (P-D-193) for what it did, with the SKU lifecycle move it made
/// (P-D-213): `found` before the decision, and the lifecycle it left, read now.
#[expect(
    clippy::too_many_arguments,
    reason = "the audit row names the act, the lifecycle it found, and the next it found"
)]
async fn decision_audit(
    tx: &DbTx<'_>,
    ctx: &SecurityContext,
    audited: Audited,
    unit: &Unit,
    found: bss_products_sdk::models::Lifecycle,
    before_next: Option<bss_products_sdk::models::LifecycleNext>,
    note: Option<String>,
    now: OffsetDateTime,
) -> Result<(), TxError> {
    let action = audited.action();
    let left = g::recorded_to(tx, ctx.subject_tenant_id(), unit.ref_id, before_next).await?;
    let moved = repo::LifecycleMove::between(found, left);
    g::audit(tx, ctx, action, "approval_unit", unit.id, note, now, moved).await
}

/// Load the authorized unit's proposed content before external catalog resolution.
async fn review_content(
    state: &Arc<ApiState>,
    scope: &AccessScope,
    ctx: &SecurityContext,
    id: Uuid,
) -> Result<Option<SkuContent>, CanonicalError> {
    let scope = scope.clone();
    let ctx_tx = ctx.clone();
    // It enqueues nothing, but the subject it reads through takes the attempt's handle (P-D-221).
    events::transaction(
        &state.db.db(),
        &state.sink,
        category_tx_config(state),
        contention_db_err,
        move |tx, outbox| {
            let scope = scope.clone();
            let ctx = ctx_tx.clone();
            Box::pin(async move {
                let store = repo::ProductsApprovalStore {
                    scope,
                    tenant_id: ctx.subject_tenant_id(),
                };
                let unit = load(tx, &store, id).await?;
                if unit.state != UnitState::Pending {
                    return Err(ApprovalError::AlreadyDecided.into());
                }
                if unit.kind == KIND_SKU_RETIRE {
                    return Ok(None);
                }
                let sub = subject(&outbox, tx, &store, &ctx, &unit, None).await?;
                Ok(Some(proposed(&sub, tx, &unit).await?))
            })
        },
    )
    .await
    .map_err(tx_to_canonical)
}

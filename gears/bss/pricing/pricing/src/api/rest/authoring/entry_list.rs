//! `GET /price-books/{id}/entries` on the toolkit's `OData` pager (D-483), each entry with its
//! usage, its price in force and its next price, all judged on one day (D-428, D-440, D-472,
//! D-473).
//!
//! The list takes `as_of`, `limit` (alias `$top`; default 500, clamped at 500), `cursor` (alias
//! `$skiptoken`) and `$filter` over [`EntryFilterField`] (`sku_id`, `charge_kind`, `model`,
//! `reference_state`). Its order is fixed, `(sku_id, charge_kind, model, id)`, so it takes no
//! `$orderby`. The cursor carries a hash of `$filter` and of the day, so a cursor replayed under
//! another filter or another `as_of` is 400 `FILTER_MISMATCH`. Any other plain key, or one given
//! twice, is 400 `QUERY_INVALID`; the pager refuses `$select`, `$count` and the other `$` options.
//! There is no `q`: SKU names live in Products.
use super::{
    AuthoringState, books,
    dto::PricingPriceBookEntryList,
    price_book_entries,
    support::{self, authz_failure, invalid_because, require_authenticated, transaction},
};
use crate::{
    authz::{self, actions, resource_types},
    infra::storage::repo::price_book_entry_repo::{EntryListField, EntryListMapping},
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Router,
    extract::Path,
    http::{StatusCode, Uri},
    response::Response,
};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    odata::OData,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::odata::filter_node_to_condition;
use toolkit_db::secure::AccessScope;
use toolkit_odata::{
    Error as ODataError, ODataQuery,
    errors::OdataError,
    filter::{FieldKind, FilterField, convert_expr_to_filter_node},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The fields a list `$filter` names: the pager's fields without `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryFilterField {
    SkuId,
    ChargeKind,
    Model,
    ReferenceState,
}
impl EntryFilterField {
    const fn field(self) -> EntryListField {
        match self {
            Self::SkuId => EntryListField::SkuId,
            Self::ChargeKind => EntryListField::ChargeKind,
            Self::Model => EntryListField::Model,
            Self::ReferenceState => EntryListField::ReferenceState,
        }
    }
}
impl FilterField for EntryFilterField {
    const FIELDS: &'static [Self] = &[
        Self::SkuId,
        Self::ChargeKind,
        Self::Model,
        Self::ReferenceState,
    ];
    fn name(&self) -> &'static str {
        self.field().name()
    }
    fn kind(&self) -> FieldKind {
        self.field().kind()
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}

/// Register the list under `price_book_entry × read`.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/bss-pricing/v1/price-books/{id}/entries")
        .operation_id("bss_pricing.list_entries")
        .summary("List a book's entries")
        .description(
            "One page of the price book entries of one book of the tenant (D-483), ordered by \
             sku_id, charge_kind, model and id, all ascending: month and year entries of one \
             model follow their id. Each entry carries its usage (D-428): its prices by state (a \
             rejected price is not counted; the approved ones also as scheduled, active and \
             superseded on the day, D-440), the distinct plans whose draft, pending, scheduled or \
             published revisions name it, and the distinct plans that name it only through \
             superseded revisions; its current_price, the default chain's approved price in force \
             on the day; and its next_price, the default chain's earliest price scheduled after \
             the day, else its newest draft or pending price (the highest version_no), else null \
             (D-472). Both prices are shown to a caller who also holds price_book read on the book \
             and are null otherwise (D-434, D-440). The day is as_of, a YYYY-MM-DD date, else \
             today (UTC): every price's status, the usage split and both prices are judged on that \
             one day, on every page (D-473). A day before the book's valid_from, or on or after \
             its valid_until, still answers with the prices in force on it, but a price outside \
             the book's validity is not sellable: the book allows no sale on that day. limit \
             (alias $top) is 500 by default and clamped at 500, and cursor (alias $skiptoken) \
             continues from page_info.next_cursor: a caller that does not follow next_cursor reads \
             the first 500. OData $filter takes sku_id (eq, ne, in), charge_kind, model and \
             reference_state, each compared with one of its closed values. There is no q: SKU \
             names live in Products, so a search asks GET /bss-products/v1/skus?q= and narrows \
             this list with $filter=sku_id in (...). The cursor carries a hash of $filter and of \
             the day, so a cursor replayed under another $filter or another as_of is 400 \
             FILTER_MISMATCH. Refusals, in order: 403 without price_book_entry read; 503 when the \
             policy cannot judge the money; 400 QUERY_INVALID for a plain key other than as_of, \
             limit and cursor, or one given twice, then 400 DATE_INVALID for an as_of that is not \
             a YYYY-MM-DD date; 400 for $orderby (the order is fixed), $select, $count, a $filter \
             the list does not take, a limit of 0 or a cursor that does not read; 400 \
             FILTER_MISMATCH; 404 for a book the tenant does not hold; then 403 \
             PRICE_BOOK_READ_REQUIRED for an as_of other than today when the caller's price_book \
             read does not admit the book, judged before any entry is read: the usage split on \
             another day dates every approved price, so it is money, and a caller without it \
             reads today only.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .query_param(
            "as_of",
            false,
            "The day the prices are judged on, YYYY-MM-DD; today (UTC) by default",
        )
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 500, clamped at 500)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .handler(list_entries)
        .with_odata_filter::<EntryFilterField>()
        .json_response_with_schema::<PricingPriceBookEntryList>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// The plain keys the list takes; the `$` options are the extractor's.
const PLAIN: &[&str] = &["as_of", "limit", "cursor"];

/// The day the list is judged on (D-473): `as_of`, a `YYYY-MM-DD` date, else today (UTC). Only
/// [`PLAIN`] keys, each once: any other plain key, or one given twice, is 400 `QUERY_INVALID`; an
/// `as_of` that is not such a date, an empty one included, is 400 `DATE_INVALID`.
fn day_of(uri: &Uri, today: time::Date) -> Result<time::Date, CanonicalError> {
    let axum::extract::Query(pairs) =
        axum::extract::Query::<Vec<(String, String)>>::try_from_uri(uri)
            .map_err(|_| invalid_because("query", "QUERY_INVALID", "a malformed query string"))?;
    support::plain_keys(
        &pairs,
        PLAIN,
        |key| key.starts_with('$'),
        |key| {
            format!(
                "`{key}` is not a parameter of this read; it takes as_of, limit, cursor and $filter"
            )
        },
    )?;
    let as_of = pairs
        .iter()
        .find(|(k, _)| k == "as_of")
        .map(|(_, v)| v.clone());
    Ok(support::date(as_of, "as_of")?.unwrap_or(today))
}

/// The `$filter`, checked the way the pager will read it, before any read: only
/// [`EntryFilterField`]s, and each closed value through the mapping.
fn checked_filter(odata: &ODataQuery) -> Result<(), CanonicalError> {
    let Some(expr) = odata.filter.as_deref() else {
        return Ok(());
    };
    convert_expr_to_filter_node::<EntryFilterField>(expr)
        .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
    let node = convert_expr_to_filter_node::<EntryListField>(expr)
        .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
    filter_node_to_condition::<EntryListField, EntryListMapping>(&node)
        .map_err(ODataError::InvalidFilter)?;
    Ok(())
}

/// The cursor's filter hash over everything that narrows or dates a page: the extractor's hash of
/// `$filter` and the day — the first 8 bytes of the SHA-256 of their canonical JSON, as hex. The
/// day is the one the page is judged on, so `as_of` of today and no `as_of` are one narrowing.
fn page_hash(odata: &ODataQuery, day: time::Date) -> Result<String, CanonicalError> {
    support::page_hash(&serde_json::json!({
        "filter": odata.filter_hash,
        "day": day.to_string(),
    }))
}

async fn list_entries(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    uri: Uri,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    // D-440: the money is shown as D-434 shows it — price_book read, judged a second time.
    let books = super::money_scope(&enforcer, &ctx).await?;
    // D-473: the one day the whole answer is judged on, refused after the money's policy and
    // before the book (D-440's order); then the pager's query (D-483), before any read.
    let today = time::OffsetDateTime::now_utc().date();
    let day = day_of(&uri, today)?;
    let OData(mut odata) = odata?;
    if odata.select.is_some() {
        return Err(OdataError::invalid_argument()
            .with_field_violation(
                "$select",
                "an entry is not projected; drop `$select`",
                "UNSUPPORTED_QUERY_PARAM",
            )
            .create());
    }
    if !odata.order.0.is_empty() {
        return Err(OdataError::invalid_argument()
            .with_field_violation(
                "$orderby",
                "the entries keep one order, (sku_id, charge_kind, model, id); drop `$orderby`",
                "UNSUPPORTED_QUERY_PARAM",
            )
            .create());
    }
    checked_filter(&odata)?;
    let hash = page_hash(&odata, day)?;
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let dated = day != today;
    let tenant = ctx.subject_tenant_id();
    let body = transaction(&state.db.db(), move |tx| {
        let (scope, books, odata) = (scope.clone(), books.clone(), odata.clone());
        Box::pin(async move {
            books::find(tx, &AccessScope::for_tenant(tenant), tenant, id).await?;
            // D-428, D-440, D-472: every entry's usage, price in force and next price in a fixed
            // number of reads per page.
            let shown = price_book_entries::shows_money(tx, books.as_ref(), tenant, id).await?;
            // D-473 (amended): the usage split on another day moves with the start and the end of
            // every approved price, so it is money: without the grant on the book, a day other
            // than today is refused before any entry, price or usage is read (D-483).
            if dated && !shown {
                return Err(support::forbidden_because(
                    "PRICE_BOOK_READ_REQUIRED",
                    "a book's entries on a day other than today are money: reading them takes \
                     price_book read on the book",
                )
                .into());
            }
            let page = books::entries_page(tx, &scope, tenant, id, &odata).await?;
            Ok(PricingPriceBookEntryList {
                items: price_book_entries::read(tx, tenant, page.items, shown, day).await?,
                page_info: page.page_info,
            })
        })
    })
    .await?;
    super::names::named(&state, &ctx, body, None).await
}

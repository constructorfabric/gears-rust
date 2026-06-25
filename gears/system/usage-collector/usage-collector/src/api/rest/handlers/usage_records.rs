//! REST handlers for the foundation `/usage-collector/v1/records`
//! create + deactivation surface. Each handler is a thin pass-through:
//! it pulls the gateway-resolved `SecurityContext`, dispatches to the
//! domain [`Service`], and lifts `UsageCollectorError` through the
//! host-owned canonical mapping. PDP authorization runs inside each
//! `Service` create / deactivation method.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use toolkit::api::canonical_prelude::*;
use toolkit_canonical_errors::Problem;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use toolkit_security::SecurityContext;
use usage_collector_sdk::{
    AggregationSpec, IdempotencyKey, MetadataFilter, MetadataKey, ResourceRef, SubjectRef,
    UsageCollectorError, UsageRecord, UsageRecordStatus, UsageTypeGtsId,
};
use uuid::Uuid;

use crate::api::rest::dto::{
    AggregationResultDto, CreateUsageRecordRequest, CreateUsageRecordResultDto,
    CreateUsageRecordsRequest, CreateUsageRecordsResponse, QueryAggregatedUsageRecordsRequest,
    UsageRecordDto,
};
use crate::domain::Service;
use crate::domain::service::MAX_BATCH_RECORDS;
use crate::infra::sdk_error_mapping::{
    UsageRecordResource,
    usage_collector_error_to_canonical_for_usage_record as usage_collector_error_to_canonical,
    usage_record_error_to_problem,
};

/// `POST /usage-collector/v1/records`
///
/// Batch-create one or more usage records. Per-record validation /
/// authorization / dispatch failures surface as `Rejected` entries inside
/// the response envelope at their input index; the response status is
/// `200 OK` when every record was accepted and `207 Multi-Status` when
/// at least one record was rejected.
///
/// Whole-request failures (handle resolution, batch SPI dispatch) still
/// short-circuit through the canonical `Problem` envelope.
// @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-security-context:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-api-post-records:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-security-context:p1
pub async fn handle_create_usage_records(
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-missing-ctx
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-missing-ctx
    Extension(service): Extension<Arc<Service>>,
    Json(req): Json<CreateUsageRecordsRequest>,
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-submit
) -> ApiResult<impl IntoResponse> {
    // Mirror `Service::create_usage_records`' `1..=MAX_BATCH_RECORDS` gate at
    // the handler so an oversized or empty wire payload is rejected as
    // `InvalidArgument` before the per-record loop allocates / iterates.
    // The service still enforces the same invariant for non-REST callers.
    let actual = req.records.len();
    if actual == 0 || actual > MAX_BATCH_RECORDS {
        return Err(usage_collector_error_to_canonical(
            UsageCollectorError::invalid_batch_size(actual, 1, MAX_BATCH_RECORDS),
        ));
    }

    let mut indexed_results: Vec<(usize, CreateUsageRecordResultDto)> =
        Vec::with_capacity(req.records.len());
    let mut eligible: Vec<(usize, UsageRecord)> = Vec::new();

    for (index, item) in req.records.into_iter().enumerate() {
        match record_request_into_domain(item) {
            Ok(record) => eligible.push((index, record)),
            Err(problem) => indexed_results.push((
                index,
                CreateUsageRecordResultDto::Rejected {
                    index,
                    error: problem,
                },
            )),
        }
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx

    if !eligible.is_empty() {
        let (indices, batch): (Vec<usize>, Vec<UsageRecord>) = eligible.into_iter().unzip();

        // Batch-level dispatch failure (plugin resolution, SPI size
        // mismatch) bubbles through `?` as a whole-request canonical
        // envelope — the same failure would have hit every record
        // identically. `Service::create_usage_records` post-condition:
        // one result per dispatched record, in order.
        let per_record = service
            .create_usage_records(&ctx, batch)
            .await
            .map_err(usage_collector_error_to_canonical)?;
        for (index, outcome) in indices.into_iter().zip(per_record) {
            indexed_results.push((index, per_record_outcome(index, outcome)));
        }
    }

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-compose-response
    indexed_results.sort_by_key(|(idx, _)| *idx);
    let results: Vec<CreateUsageRecordResultDto> =
        indexed_results.into_iter().map(|(_, item)| item).collect();
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-compose-response

    let any_rejected = results
        .iter()
        .any(|item| matches!(item, CreateUsageRecordResultDto::Rejected { .. }));

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-return-200
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-return-207
    let status = if any_rejected {
        StatusCode::MULTI_STATUS
    } else {
        StatusCode::OK
    };

    Ok((status, Json(CreateUsageRecordsResponse { results })))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-return-207
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-return-200
}

/// `GET /usage-collector/v1/records/{id}`
///
/// Read a single usage record by `uuid`. A malformed `uuid` path segment
/// surfaces as the canonical `InvalidArgument` problem; a missing record
/// surfaces as the canonical `NotFound` problem; a PDP denial surfaces as
/// `Forbidden`; a Plugin SPI transport / readiness / persistence fault
/// surfaces as `ServiceUnavailable`. On success the response is HTTP 200
/// with the wire-projected [`UsageRecordDto`] body.
// @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-get-record:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-api-get-records-id:p1
pub async fn handle_get_usage_record(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-missing-ctx
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-missing-ctx
    Extension(service): Extension<Arc<Service>>,
    Path(uuid_raw): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let id = parse_record_id(&uuid_raw)?;
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-spi-fail
    let record = service
        .get_usage_record(&ctx, id)
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-spi-fail
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-success
    Ok((StatusCode::OK, Json(UsageRecordDto::from(record))))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-success
}

/// `GET /usage-collector/v1/records`
///
/// Keyset-paginated raw read over the persisted usage records.
///
/// `gts_id` is the only mandatory non-OData query parameter (the SDK
/// trait carries it as a typed [`UsageTypeGtsId`] and the plugin SPI
/// takes it as a typed named parameter). A bounded `[from, to)` time
/// window is also mandatory: it is expressed inside `$filter` as
/// `created_at ge … and created_at lt …` —
/// `UsageRecordFilterField::created_at` is on the `OData` filter schema
/// for exactly that purpose — and the service rejects an absent or
/// one-sided window with `400 InvalidArgument`
/// (`MISSING_TIME_WINDOW`) before any plugin dispatch. `$filter` /
/// `$orderby` / `$top` / `cursor` flow through the standard [`OData`]
/// extractor.
///
/// Gateway-side guards applied before the service is invoked:
///
/// * **`$top` cap** — `ODataQuery.limit` is bounded by [`MAX_PAGE_SIZE`].
///   A caller passing `?$top=1000000` receives a `400 InvalidArgument`
///   so they cannot silently misinterpret a clamped page as complete.
/// * **Cursor validation** — when a `cursor` is present, the toolkit's
///   [`validate_cursor_against`] confirms it was minted under the same
///   `$filter` AST and `$orderby` projection; mismatches surface as
///   the canonical `cursor_decode` / `order_mismatch` / `filter_mismatch`
///   `Problem`. The decoded `CursorV1` flows to the plugin via
///   `ODataQuery.cursor` unchanged.
/// * **`$orderby` default** — when the caller omits `$orderby`, the
///   gateway substitutes `(created_at asc, uuid asc)` so the plugin
///   has a stable keyset for pagination.
///
/// Per-key metadata filtering is the typed side-channel
/// [`MetadataFilter`] from the SDK — `toolkit-odata` has no surface for
/// filtering on dynamic JSON-map keys. The wire encoding is **repeated
/// query parameters of the form `metadata.<key>=<value>`**:
///
/// * `?metadata.user_id=u1&metadata.user_id=u2` → one filter on
///   `user_id` whose value set is `{u1, u2}` (OR within the key).
/// * `?metadata.user_id=u1&metadata.region=eu` → two filters
///   `user_id ∈ {u1}` AND `region ∈ {eu}` (AND across keys).
/// * Missing entirely → no metadata filter.
///
/// PDP authorization, PDP-constraint composition into the `OData` filter,
/// and the plugin SPI dispatch all happen inside
/// [`Service::list_usage_records`]; this handler is a thin wrapper that
/// applies the gateway-side guards, parses the typed query parameters,
/// and projects the returned records to the wire [`UsageRecordDto`]
/// shape.
// @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-raw:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-raw:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-query-constraint-nfr-thresholds:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-query-cursor-v1-toolkit-adoption:p1
pub async fn handle_list_usage_records(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-missing-ctx
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-missing-ctx
    Extension(service): Extension<Arc<Service>>,
    Query(params): Query<Vec<(String, String)>>,
    OData(query): OData,
) -> ApiResult<Json<ODataPage<UsageRecordDto>>> {
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-request-received
    let (gts_id, metadata_filter, query) = prepare_list_request(&params, query)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-request-received

    let page = service
        .list_usage_records(&ctx, gts_id, &query, &metadata_filter)
        .await
        .map_err(usage_collector_error_to_canonical)?;

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-return
    Ok(Json(page.map_items(UsageRecordDto::from)))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-return
}

/// `POST /usage-collector/v1/records/aggregate`
///
/// Server-side aggregated read over the persisted usage records.
///
/// The wire shape mirrors `GET /usage-collector/v1/records`: `gts_id` is a
/// mandatory typed query parameter, the `OData` `$filter` (carrying the
/// `[from, to)` time window as a `created_at` predicate) and the
/// `metadata.<key>=<value>` typed side-channel flow through query
/// parameters, and only the [`AggregationSpec`] (operator + group-by
/// dimensions) ships in the JSON body. `$orderby`, `$top` /
/// `limit`, and `cursor` are intentionally NOT accepted here — the
/// aggregation result is not paginated (the SDK contract emits one
/// `AggregationResult` per call).
///
/// PDP authorization, PDP-constraint composition into the `OData` filter,
/// and the plugin SPI dispatch all happen inside
/// [`Service::query_aggregated_usage_records`]; this handler is a thin
/// wrapper that parses the typed query parameters, lifts the
/// [`QueryAggregatedUsageRecordsRequest`] body into an [`AggregationSpec`],
/// dispatches to the service, and projects the result to the wire
/// [`AggregationResultDto`] shape.
// @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-aggregation:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-query-api-post-records-aggregate:p1
pub async fn handle_query_aggregated_usage_records(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-missing-ctx
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-missing-ctx
    Extension(service): Extension<Arc<Service>>,
    Query(params): Query<Vec<(String, String)>>,
    OData(query): OData,
    Json(req): Json<QueryAggregatedUsageRecordsRequest>,
) -> ApiResult<Json<AggregationResultDto>> {
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-request-received
    let (gts_id, metadata_filter, query, aggregation) =
        prepare_aggregate_request(&params, query, req)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-request-received

    let result = service
        .query_aggregated_usage_records(&ctx, gts_id, &query, &metadata_filter, aggregation)
        .await
        .map_err(usage_collector_error_to_canonical)?;

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-return
    Ok(Json(AggregationResultDto::from(result)))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-return
}

/// Result of every aggregate-path pre-service validator.
type PreparedAggregateRequest = (
    UsageTypeGtsId,
    Vec<MetadataFilter>,
    ODataQuery,
    AggregationSpec,
);

/// Bundle of every aggregate-path pre-service validator: parameter
/// allowlist, typed `gts_id`, metadata filters, and the body-shape
/// projection into a typed [`AggregationSpec`]. Propagates the canonical
/// envelope verbatim on the first failing validator.
fn prepare_aggregate_request(
    params: &[(String, String)],
    query: ODataQuery,
    req: QueryAggregatedUsageRecordsRequest,
) -> Result<PreparedAggregateRequest, CanonicalError> {
    reject_unknown_aggregate_params(params)?;
    let gts_id = parse_required_gts_id(params)?;
    let metadata_filter = parse_metadata_filters(params)?;
    let aggregation = AggregationSpec::try_from(req).map_err(usage_collector_error_to_canonical)?;
    Ok((gts_id, metadata_filter, query, aggregation))
}

/// `$`-prefixed `OData` parameters accepted on the aggregate path. `$top`,
/// `cursor`, and `$select` are intentionally excluded — aggregation is
/// not paginated and its projection is fixed (operator + group-by
/// dimensions ship in the body).
const AGGREGATE_ODATA_PARAMS: &[&str] = &["$filter"];

/// Reject any query parameter on the aggregate path that is not in the
/// declared aggregate-OData set, the typed list parameters (`gts_id`),
/// or a `metadata.<key>` entry. Silent drop of unrecognised parameters
/// is a documented contract-drift surface, mirroring `list_usage_records`.
fn reject_unknown_aggregate_params(params: &[(String, String)]) -> Result<(), CanonicalError> {
    if let Some((key, _)) = params.iter().find(|(k, _)| {
        !AGGREGATE_ODATA_PARAMS.contains(&k.as_str())
            && !TYPED_LIST_PARAMS.contains(&k.as_str())
            && !k.starts_with(METADATA_PREFIX)
    }) {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                key,
                format!(
                    "unrecognised query parameter `{key}`; expected one of \
                     {TYPED_LIST_PARAMS:?}, OData parameters \
                     {AGGREGATE_ODATA_PARAMS:?}, or `metadata.<key>` entries"
                ),
                "VALIDATION",
            )
            .create());
    }
    Ok(())
}

/// Result of every pre-service validator, returned as a typed tuple
/// so the handler can propagate the canonical envelope verbatim.
type PreparedListRequest = (UsageTypeGtsId, Vec<MetadataFilter>, ODataQuery);

/// Bundle of every pre-service validator: parameter allowlist, typed
/// `gts_id`, metadata filters, and the `prepare_list_query`
/// gateway-side guards. Propagates the canonical envelope verbatim on
/// the first failing validator.
fn prepare_list_request(
    params: &[(String, String)],
    query: ODataQuery,
) -> Result<PreparedListRequest, CanonicalError> {
    reject_unknown_list_params(params)?;
    let gts_id = parse_required_gts_id(params)?;
    let metadata_filter = parse_metadata_filters(params)?;
    let query = prepare_list_query(query)?;
    Ok((gts_id, metadata_filter, query))
}

/// Maximum number of records the gateway will request from the plugin
/// in a single page per
/// `cpt-cf-usage-collector-dod-usage-query-constraint-nfr-thresholds`.
/// A caller-supplied `limit` above this ceiling is rejected with 400
/// `InvalidArgument` (never silently clamped) so the plugin cannot be
/// coaxed into unbounded reads.
pub const MAX_PAGE_SIZE: u64 = 1000;

/// Maximum number of distinct `metadata.<key>` filters accepted on a
/// single list / aggregate query. Each filter expands into a plugin- /
/// DB-side predicate, so the cap bounds query cost end-to-end.
pub const MAX_METADATA_FILTERS: usize = 16;

/// Maximum number of values inside a single `metadata.<key>=…` filter
/// (OR-within-key). The plugin must translate the value set into a
/// `key IN (...)` predicate; capping the cardinality keeps that
/// rewrite bounded.
pub const MAX_METADATA_FILTER_VALUES: usize = 32;

/// `$`-prefixed `OData` parameters (parsed by the toolkit `OData`
/// extractor) and the non-`OData` scalars the toolkit also accepts.
/// Both `$top` (canonical `OData`) and `limit` (toolkit alias) are
/// admitted — the toolkit extractor folds them onto the same
/// `ODataQuery.limit` slot.
const OUR_ODATA_PARAMS: &[&str] = &["$filter", "$orderby", "$select", "$top", "limit", "cursor"];

/// Typed query parameters carrying SDK values that are NOT part of the
/// `OData` surface.
const TYPED_LIST_PARAMS: &[&str] = &["gts_id"];

/// Prefix marking the typed-side-channel [`MetadataFilter`] entries
/// (`metadata.<key>=<value>`, repeatable).
const METADATA_PREFIX: &str = "metadata.";

/// Default `$orderby` projection — the canonical cursor keyset.
const DEFAULT_ORDER_KEYS: &[(&str, toolkit_odata::SortDir)] = &[
    ("created_at", toolkit_odata::SortDir::Asc),
    ("uuid", toolkit_odata::SortDir::Asc),
];

/// Apply gateway-side guards on the parsed [`ODataQuery`]:
/// 1. reject `limit > MAX_PAGE_SIZE` as `InvalidArgument` (no silent
///    clamp; a caller asking for more rows than the page-size cap MUST
///    be told so they can paginate explicitly);
/// 2. default `$orderby` to `(created_at asc, uuid asc)` when empty;
/// 3. validate the optional cursor against the (now-defaulted) order
///    and the parsed filter hash.
///
/// The defaulted order is applied BEFORE cursor validation so a cursor
/// minted against `(created_at, uuid)` continues to validate when the
/// caller omits `$orderby` on subsequent calls.
// @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-odata-parse
// @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-cursor-validate
fn prepare_list_query(mut query: ODataQuery) -> Result<ODataQuery, CanonicalError> {
    // 1. $top cap. Reject above the cap so the caller observes the
    //    boundary rather than silently receiving a truncated page that
    //    looks complete.
    match query.limit {
        Some(l) if l > MAX_PAGE_SIZE => {
            return Err(UsageRecordResource::invalid_argument()
                .with_field_violation(
                    "$top",
                    format!("$top must be <= {MAX_PAGE_SIZE}, got {l}"),
                    "VALIDATION",
                )
                .create());
        }
        // Within the cap: keep the caller's $top unchanged.
        Some(_) => {}
        None => query.limit = Some(MAX_PAGE_SIZE),
    }

    // 2. $orderby default.
    if query.order.is_empty() && query.cursor.is_none() {
        // When a cursor IS present the toolkit OData extractor has
        // already left `order` empty by design (derived from
        // `cursor.s` at validate-against time), so we only inject the
        // canonical keys when there is no cursor either.
        query.order = toolkit_odata::ODataOrderBy(
            DEFAULT_ORDER_KEYS
                .iter()
                .map(|&(field, dir)| toolkit_odata::OrderKey {
                    field: field.to_owned(),
                    dir,
                })
                .collect(),
        );
    }

    // 3. Cursor validation + order materialization. When a cursor is
    // present, `query.order` is empty by toolkit convention; the effective
    // keyset order lives in the cursor's signed-token payload (`cursor.s`).
    // Derive it, validate the cursor against it and the parsed filter hash,
    // then write it back into `query.order` so it propagates to the storage
    // plugin. The plugin reads `query.order` directly to build BOTH the
    // `ORDER BY` and the keyset continuation predicate and has no access to
    // the cursor's token derivation — leaving the order empty makes the
    // plugin reject the continuation with "keyset order must not be empty"
    // (surfacing as a 500 on every cursor follow-up).
    if let Some(cursor) = query.cursor.as_ref() {
        let effective_order = toolkit_odata::ODataOrderBy::from_signed_tokens(&cursor.s)
            .map_err(CanonicalError::from)?;
        toolkit_odata::validate_cursor_against(
            cursor,
            &effective_order,
            query.filter_hash.as_deref(),
        )
        .map_err(CanonicalError::from)?;
        query.order = effective_order;
    }

    Ok(query)
}
// @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-cursor-validate
// @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-odata-parse

/// Reject any query parameter that is not one of the declared `OData`
/// parameters, one of the typed list parameters, or a
/// `metadata.<key>` entry — silent drop of unrecognised parameters is a
/// documented contract-drift surface.
fn reject_unknown_list_params(params: &[(String, String)]) -> Result<(), CanonicalError> {
    if let Some((key, _)) = params.iter().find(|(k, _)| {
        !OUR_ODATA_PARAMS.contains(&k.as_str())
            && !TYPED_LIST_PARAMS.contains(&k.as_str())
            && !k.starts_with(METADATA_PREFIX)
    }) {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                key,
                format!(
                    "unrecognised query parameter `{key}`; expected one of \
                     {TYPED_LIST_PARAMS:?}, OData parameters \
                     {OUR_ODATA_PARAMS:?}, or `metadata.<key>` entries"
                ),
                "VALIDATION",
            )
            .create());
    }
    Ok(())
}

/// Extract the mandatory `gts_id` query parameter and validate it
/// through [`UsageTypeGtsId::new`]. A missing value surfaces as the
/// canonical `InvalidArgument` `Problem` with a field violation on
/// `gts_id`; a malformed value lifts through the SDK's
/// [`UsageCollectorError::InvalidArgument`] mapping. A duplicate
/// occurrence is rejected so silent last-wins ambiguity cannot mask a
/// caller bug.
fn parse_required_gts_id(params: &[(String, String)]) -> Result<UsageTypeGtsId, CanonicalError> {
    let raw = require_single_value(params, "gts_id")?;
    UsageTypeGtsId::new(raw.clone()).map_err(usage_collector_error_to_canonical)
}

/// Group `metadata.<key>=<value>` entries into a `Vec<MetadataFilter>`
/// — one filter per distinct key, with that key's full ordered value
/// list (duplicates preserved verbatim — the OR semantics within a
/// single filter make them harmless).
///
/// An empty key (`metadata.=value`) is rejected as a canonical
/// `InvalidArgument` `Problem`; a `metadata.<key>` with an empty value
/// is admitted verbatim because the SDK does not constrain
/// [`MetadataFilter`] values beyond their `String` type.
// @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-metadata-filter-parse
fn parse_metadata_filters(
    params: &[(String, String)],
) -> Result<Vec<MetadataFilter>, CanonicalError> {
    // BTreeMap so the resulting filter vector is in deterministic key
    // order regardless of query-string ordering, which keeps plugin
    // request shapes idempotent across equivalent inputs.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in params {
        let Some(key) = k.strip_prefix(METADATA_PREFIX) else {
            continue;
        };
        if key.is_empty() {
            return Err(UsageRecordResource::invalid_argument()
                .with_field_violation(
                    k,
                    format!(
                        "metadata filter key must be non-empty (`{k}=...` has no key after the `metadata.` prefix)"
                    ),
                    "VALIDATION",
                )
                .create());
        }
        groups.entry(key.to_owned()).or_default().push(v.clone());
    }
    if groups.len() > MAX_METADATA_FILTERS {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                "metadata",
                format!(
                    "{} distinct `metadata.<key>` filters exceeds cap {MAX_METADATA_FILTERS}",
                    groups.len()
                ),
                "VALIDATION",
            )
            .create());
    }
    if let Some((key, values)) = groups
        .iter()
        .find(|(_, v)| v.len() > MAX_METADATA_FILTER_VALUES)
    {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                format!("metadata.{key}"),
                format!(
                    "{} values on `metadata.{key}` exceeds cap {MAX_METADATA_FILTER_VALUES}",
                    values.len()
                ),
                "VALIDATION",
            )
            .create());
    }
    groups
        .into_iter()
        .map(|(key, values)| {
            MetadataFilter::new(key, values).map_err(usage_collector_error_to_canonical)
        })
        .collect()
}
// @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-metadata-filter-parse

/// Find the unique value for `key`, rejecting both absence and
/// duplicates with a canonical `InvalidArgument` envelope.
fn require_single_value<'a>(
    params: &'a [(String, String)],
    key: &'static str,
) -> Result<&'a String, CanonicalError> {
    let mut iter = params.iter().filter(|(k, _)| k == key);
    let Some((_, value)) = iter.next() else {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                key,
                format!("missing required query parameter `{key}`"),
                "VALIDATION",
            )
            .create());
    };
    if iter.next().is_some() {
        return Err(UsageRecordResource::invalid_argument()
            .with_field_violation(
                key,
                format!("query parameter `{key}` must appear at most once"),
                "VALIDATION",
            )
            .create());
    }
    Ok(value)
}

/// `POST /usage-collector/v1/records/{uuid}/deactivate`
///
/// Deactivate a previously-emitted record by `uuid`. On success the
/// targeted row and any active referencing compensation rows have been
/// flipped from `active` to `inactive` inside a single backend
/// transaction; the response is HTTP 204 No Content. A malformed `uuid`
/// path segment surfaces as the canonical `InvalidArgument` problem; a
/// missing record surfaces as the canonical `NotFound` problem; a Plugin
/// SPI transport / readiness / persistence fault surfaces as the
/// canonical `ServiceUnavailable` problem.
pub async fn handle_deactivate_usage_record(
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-receive-ctx
    // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-missing-ctx
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-missing-ctx
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-receive-ctx
    Extension(service): Extension<Arc<Service>>,
    Path(uuid_raw): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let id = parse_record_id(&uuid_raw)?;
    // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-fail
    // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-fail-propagate
    service
        .deactivate_usage_record(&ctx, id)
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-fail-propagate
    // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-fail
    // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-transitioned
    // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-success
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-success
    // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping:p1:inst-algo-outcome-transitioned
}

/// Convert one per-record submission into the domain shape, lifting
/// `gts_id`-, attribution-, `idempotency_key`-, and metadata-shape
/// failures into per-record `Problem` envelopes. `created_at` is
/// caller-supplied and forwarded verbatim — neither the handler nor
/// [`Service::create_usage_records`] rewrites it.
#[allow(clippy::result_large_err)]
fn record_request_into_domain(req: CreateUsageRecordRequest) -> Result<UsageRecord, Problem> {
    let gts_id = UsageTypeGtsId::new(req.gts_id)
        .map_err(|err| Problem::from(usage_collector_error_to_canonical(err)))?;

    let resource_ref = ResourceRef::try_from(req.resource_ref)
        .map_err(|err| Problem::from(usage_collector_error_to_canonical(err)))?;

    let subject_ref = req
        .subject_ref
        .map(SubjectRef::try_from)
        .transpose()
        .map_err(|err| Problem::from(usage_collector_error_to_canonical(err)))?;

    let idempotency_key = IdempotencyKey::new(req.idempotency_key)
        .map_err(|err| Problem::from(usage_collector_error_to_canonical(err)))?;

    let metadata = metadata_from_wire(req.metadata)
        .map_err(|err| Problem::from(usage_collector_error_to_canonical(err)))?;

    Ok(UsageRecord {
        uuid: req.uuid,
        gts_id,
        tenant_id: req.tenant_id,
        resource_ref,
        subject_ref,
        metadata,
        value: req.value,
        idempotency_key,
        corrects_id: req.corrects_id,
        status: UsageRecordStatus::Active,
        created_at: req.created_at,
    })
}

/// Convert the typed wire `BTreeMap<String, String>` into the SDK's
/// validating [`BTreeMap<MetadataKey, String>`]. Structural shape errors
/// (non-object, non-string value, etc.) are already rejected at axum's
/// JSON boundary by the DTO type; only per-key validation remains here.
///
/// Closed-shape membership against `UsageType.metadata_fields` and the
/// configurable size cap remain a service-layer check
/// (`validate_submit_record_metadata`) that runs after this conversion.
fn metadata_from_wire(
    raw: BTreeMap<String, String>,
) -> Result<BTreeMap<MetadataKey, String>, UsageCollectorError> {
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        let key = MetadataKey::new(k)?;
        out.insert(key, v);
    }
    Ok(out)
}

/// Lift one per-record service outcome into the wire-shaped envelope.
// @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-return
fn per_record_outcome(
    index: usize,
    outcome: Result<UsageRecord, UsageCollectorError>,
) -> CreateUsageRecordResultDto {
    match outcome {
        Ok(record) => CreateUsageRecordResultDto::Accepted {
            index,
            record: UsageRecordDto::from(record),
        },
        Err(err) => CreateUsageRecordResultDto::Rejected {
            index,
            error: usage_record_error_to_problem(err),
        },
    }
}
// @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-return

/// Parse the URL path `{uuid}` segment shared by the GET single-record and
/// deactivate handlers. A malformed input surfaces as the canonical
/// `InvalidArgument` `Problem` with a field violation on `id`; this error
/// shape is host-private (it cannot originate inside the transport-agnostic
/// SDK).
///
fn parse_record_id(uuid_raw: &str) -> Result<Uuid, CanonicalError> {
    Uuid::parse_str(uuid_raw).map_err(|_| {
        UsageRecordResource::invalid_argument()
            .with_field_violation(
                "id",
                format!("usage record id `{uuid_raw}` is not a valid UUID"),
                "VALIDATION",
            )
            .create()
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_records_tests.rs"]
mod usage_records_tests;

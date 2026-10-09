//! Books, entries, prices, approvals, plans, dimension keys and settings REST doors.
mod approvals;
mod book_list;
mod books;
mod caps;
pub(crate) mod configuration;
pub mod dto;
mod entry_list;
pub mod inbox_source;
mod names;
pub mod plan_items;
mod plan_list;
mod plan_routes;
pub(crate) mod plans;
mod price_book_entries;
pub(crate) mod prices;
pub(crate) mod support;
use super::{correlation, preconditions};
use crate::authz::{self, OwnerTenant, ResourceRef, actions, resource_types};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Router,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use caps::Capped;
use dto::{
    PriceBookCreate, PriceBookDto, PriceBookExport, PriceBookPatch, PricingDimensionKeyPatch,
    PricingDimensionRegistry, PricingDimensions, PricingPriceBookReadDto, PricingSettingsDto,
    PricingSettingsPut,
};
use std::sync::Arc;
use support::{authz_failure, etag, header, require_authenticated, response, transaction};

/// The approve and submit grants behind `caller_can_approve` (D-471). The two PDP questions are
/// independent, so they are asked together: a unit list or card waits for one PDP round trip,
/// not two.
async fn approval_flag_scopes(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<(toolkit_security::AccessScope, toolkit_security::AccessScope), CanonicalError> {
    tokio::try_join!(
        async {
            authz::grant_scope(enforcer, ctx, actions::APPROVE)
                .await
                .map_err(authz_failure)
        },
        async {
            authz::grant_scope(enforcer, ctx, actions::SUBMIT)
                .await
                .map_err(authz_failure)
        },
    )
}

use toolkit::api::{
    OpenApiRegistry,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// The actors that are not people (D-519): pricing's own system actor, and the nil id of the
/// platform's system context. A read names them "System" and never asks Account Management.
pub const SYSTEM_ACTORS: [Uuid; 2] = [bss_products_sdk::PRICING_SYSTEM_ACTOR, Uuid::nil()];
/// Dependencies shared by every authoring request.
pub struct AuthoringState {
    pub db: toolkit_db::DBProvider<toolkit_db::DbError>,
    pub hub: Arc<toolkit::ClientHub>,
    /// Where every pricing event is enqueued, inside the transaction of its act.
    pub outbox: crate::infra::events::EventSink,
    /// The clock the approval doors read their instant from: the wall clock, or a test's.
    clock: Arc<dyn crate::infra::reference_work::Clock>,
    /// The names of the actors a read shows (D-519), through Account Management when the hub
    /// holds it.
    actor_names: bss_rest::actor_names::ActorNames,
    pipeline: tokio::sync::Mutex<Option<Pipeline>>,
}
/// The running outbox processor: the broker SDK's producer, or the holding one.
enum Pipeline {
    Broker(Box<event_broker_sdk::ProducerOutboxHandle>),
    Interim(toolkit_db::outbox::OutboxHandle),
}
impl AuthoringState {
    /// Attach the durable event queue to the runtime database. With an `EventBrokerApi` in
    /// the hub its processor is the broker SDK's `DbProducer`; without one it holds every
    /// envelope and reports nothing delivered.
    /// # Errors
    /// Fails initialization if a present broker refuses the producer or the queue cannot start.
    pub async fn new(
        db: toolkit_db::DBProvider<toolkit_db::DbError>,
        hub: Arc<toolkit::ClientHub>,
    ) -> anyhow::Result<Self> {
        use anyhow::Context;
        let partitions = toolkit_db::outbox::Partitions::of(1);
        let bound = crate::infra::broker::bind_producer(
            &hub,
            db.db(),
            crate::infra::events::OUTBOX_TABLE_PREFIX,
            partitions,
        )
        .await
        .context("bss-pricing: the event-broker producer could not be bound")?;
        let (outbox, pipeline) = if let Some((sink, handle)) = bound {
            tracing::info!(
                queue = crate::infra::events::QUEUE,
                topic = crate::infra::events::TOPIC,
                "bss-pricing: publishing through the event-broker SDK producer"
            );
            (sink, Pipeline::Broker(Box::new(handle)))
        } else {
            tracing::warn!(
                "bss-pricing: no EventBrokerApi in the ClientHub; events accumulate \
                 undelivered on the interim queue and no delivery is ever reported"
            );
            let handle = toolkit_db::outbox::Outbox::builder(db.db())
                .table_prefix(crate::infra::events::OUTBOX_TABLE_PREFIX)?
                .queue(crate::infra::events::QUEUE, partitions)
                .leased(crate::infra::events::PendingProducer)
                .start()
                .await?;
            (
                crate::infra::events::EventSink::Interim(handle.outbox().clone()),
                Pipeline::Interim(handle),
            )
        };
        let actor_names = bss_rest::actor_names::ActorNames::from_hub(hub.clone(), &SYSTEM_ACTORS);
        Ok(Self {
            db,
            hub,
            outbox,
            clock: Arc::new(crate::infra::reference_work::WallClock),
            actor_names,
            pipeline: tokio::sync::Mutex::new(Some(pipeline)),
        })
    }
    /// The same state with the approval doors reading `clock`; tests inject one whose instants
    /// carry digits finer than a microsecond, which the wall clock of some hosts never has.
    #[must_use]
    pub fn with_clock(self, clock: Arc<dyn crate::infra::reference_work::Clock>) -> Self {
        Self { clock, ..self }
    }
    /// The same state naming actors with `actor_names`; tests install a fake directory.
    #[must_use]
    pub fn with_actor_names(self, actor_names: bss_rest::actor_names::ActorNames) -> Self {
        Self {
            actor_names,
            ..self
        }
    }
    pub(crate) async fn stop(&self) {
        match self.pipeline.lock().await.take() {
            Some(Pipeline::Broker(handle)) => handle.stop().await,
            Some(Pipeline::Interim(handle)) => handle.stop().await,
            None => {}
        }
    }
}
/// The enforcer and the canonical error layer the served gear and the in-process vote share.
#[must_use = "the layered router is what the gear and the inbox serve"]
pub fn with_caller_layers(router: Router, enforcer: PolicyEnforcer) -> Router {
    router
        .layer(Extension(enforcer))
        .layer(axum::middleware::from_fn(
            toolkit::api::canonical_error_middleware,
        ))
}

/// Mount the complete authoring surface and establish one audit correlation per request.
#[allow(
    clippy::too_many_lines,
    reason = "one OperationBuilder chain per route keeps every door's contract in one place"
)]
pub fn router(state: Arc<AuthoringState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-pricing/v1/price-books")
        .operation_id("bss_pricing.create_book")
        .summary("Create a price book")
        .description(
            "Creates a price book of the tenant with a code, a name, one currency, an optional \
             validity window and an optional description of at most 2000 characters (D-444); the \
             Idempotency-Key replays the first answer. The currency must be one the tenant \
             settings offer, when they offer any (D-438). The code is at most 64 characters and \
             the name 200 (D-457). Refusals: 400 BOOK_CODE_REQUIRED, BOOK_NAME_REQUIRED, \
             BOOK_CURRENCY_INVALID, BOOK_VALIDITY_INVALID, BOOK_DESCRIPTION_TOO_LONG, or \
             FIELD_TOO_LONG on a code or a name over its cap; 409 CURRENCY_NOT_OFFERED, \
             BOOK_CODE_TAKEN or IDEMPOTENCY_CONFLICT.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .json_request::<PriceBookCreate>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(create_book)
        .json_response_with_schema::<PriceBookDto>(openapi, StatusCode::CREATED, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = book_list::register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/price-books/{id}")
        .operation_id("bss_pricing.get_book")
        .summary("Read a price book")
        .description(
            "Returns one price book of the tenant with its stats (D-441), as the list answers \
             it, and its version as the ETag a following PATCH sends back as If-Match. \
             Refusals: 404 for a book the tenant does not hold.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .handler(get_book)
        .json_response_with_schema::<PricingPriceBookReadDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-pricing/v1/price-books/{id}")
        .operation_id("bss_pricing.patch_book")
        .summary("Rename, re-date or describe a price book")
        .description(
            "Changes a book's name, validity window or description at the version the caller read \
             (If-Match); an omitted description is kept and null clears it (D-444). The name is \
             at most 200 characters (D-457). Refusals: 400 BOOK_NAME_REQUIRED, \
             BOOK_VALIDITY_INVALID, BOOK_DESCRIPTION_TOO_LONG, or FIELD_TOO_LONG on a name over \
             its cap; 404 for an unknown book; 409 STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .json_request::<PriceBookPatch>(openapi, "Request")
        .param(header("If-Match"))
        .handler(patch_book)
        .json_response_with_schema::<PriceBookDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete("/bss-pricing/v1/price-books/{id}")
        .operation_id("bss_pricing.delete_book")
        .summary("Delete an unused price book")
        .description(
            "Deletes a book no entry and no plan revision names, at the version the caller read \
             (If-Match), with an audit row (D-444). Units that named it stay readable, their cards \
             without the book; the create's Idempotency-Key still replays its answer for a day. \
             Refusals, in order: 403 without the book write grant; 400 for a missing or malformed \
             If-Match; 404 for a book the tenant does not hold; 409 STALE_REVISION; 409 \
             BOOK_HAS_ENTRIES (an entry of any state); 409 BOOK_IN_PLAN (a plan with a draft, \
             pending, scheduled or published revision on the book, as stats.plans counts it, \
             D-441); 409 BOOK_IN_PLAN_HISTORY (only superseded revisions name it, as \
             stats.plans_superseded_only counts it): the delete succeeds exactly when \
             stats.entries, stats.plans and stats.plans_superseded_only are 0. A row added by a \
             concurrent writer is the same 409.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .param(header("If-Match"))
        .handler(delete_book)
        .no_content_response(StatusCode::NO_CONTENT, "Deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = archive_routes(router, openapi);
    let router = entry_list::register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/price-books/{id}/export")
        .operation_id("bss_pricing.export_book")
        .summary("Export a price book")
        .description(
            "Returns a book with every entry and all its prices, of every state, in chain order. \
             Refusals: 404 for a book the tenant does not hold.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .handler(export_book)
        .json_response_with_schema::<PriceBookExport>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/settings")
        .operation_id("bss_pricing.get_settings")
        .summary("Read the tenant settings")
        .description(
            "Returns the tenant's billing defaults (timing, rounding, GL code, tax category), \
             invoice-line templates by SKU type and the currencies a new book may take (empty: \
             any), with who wrote them last and when, and its version as the ETag; the defaults \
             apply, with no writer, until the settings are first written. An If-None-Match that \
             matches that ETag by weak comparison is 304 with an empty body and the same ETag; \
             both answers carry Cache-Control private, no-cache (D-518).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .param(support::if_none_match_version())
        .handler(get_settings)
        .json_response_with_schema::<PricingSettingsDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .response_header(support::revalidate_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match tag matches the settings version",
        )
        .response_header(etag())
        .response_header(support::revalidate_header())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::put("/bss-pricing/v1/settings")
        .operation_id("bss_pricing.put_settings")
        .summary("Write the tenant settings")
        .description(
            "Replaces the tenant settings at the version the caller read (If-Match), currencies \
             included (required; [] offers any currency, D-438), and records the caller and the \
             time. Rounding is half_up, half_even, half_down, up or down (D-437). A GL code or a \
             tax category other than the stored one is at most 64 characters, and a line template \
             other than the one stored for its SKU type 2000; a stored text sent back unchanged \
             passes whatever its length (D-457). Refusals: 400 TIMING_INVALID, ROUNDING_REQUIRED, \
             ROUNDING_INVALID, SKU_TYPE_INVALID, CURRENCY_INVALID, an invalid line template \
             (LINE_TEMPLATE_EMPTY, LINE_TEMPLATE_INVALID), or FIELD_TOO_LONG on a changed \
             default_gl, default_tax_category or invoice_line_templates over its cap; 409 \
             STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .json_request::<PricingSettingsPut>(openapi, "Request")
        .param(header("If-Match"))
        .handler(put_settings)
        .json_response_with_schema::<PricingSettingsDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/dimension-keys")
        .operation_id("bss_pricing.get_dimensions")
        .summary("Read the dimension registry")
        .description(
            "Returns the tenant's dimension keys with their values, each value with the prices \
             of any state that use it (D-436), and a content ETag a following PUT or PATCH sends \
             back as If-Match. Only a caller without config read is refused (403).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .handler(get_dimensions)
        .json_response_with_schema::<PricingDimensionRegistry>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::put("/bss-pricing/v1/dimension-keys")
        .operation_id("bss_pricing.put_dimensions")
        .summary("Write the dimension registry")
        .description(
            "Replaces the tenant's dimension keys and values at the content the caller read \
             (If-Match). A key or a value the stored registry does not hold is at most 64 \
             characters; one it holds passes whatever its length (D-457). Refusals: 400 \
             DIM_KEY_INVALID, DIM_VALUES_FEW, DIM_VALUE_INVALID, DIM_KEY_DUPLICATE, or \
             FIELD_TOO_LONG on a new key or value over its cap; 409 DIMENSION_KEY_IN_USE or \
             DIM_VALUE_IN_USE for a key an entry names or a value a price uses (naming it); 409 \
             STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .json_request::<PricingDimensions>(openapi, "Request")
        .param(header("If-Match"))
        .handler(put_dimensions)
        .json_response_with_schema::<PricingDimensionRegistry>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-pricing/v1/dimension-keys")
        .operation_id("bss_pricing.patch_dimension_values")
        .summary("Add or remove values of one dimension key")
        .description(
            "Adds and removes values of one declared key (stored, or the seed key while nothing \
             is stored) at the content the caller read (If-Match); keys themselves are added and \
             removed by the PUT (D-436). A value it adds is at most 64 characters (D-457). \
             Refusals: 400 DIM_NOT_DECLARED, DIM_VALUE_DUPLICATE, DIM_VALUE_UNKNOWN, \
             DIM_VALUE_INVALID, DIM_VALUES_FEW, or FIELD_TOO_LONG on an added value over its cap; \
             409 DIM_VALUE_IN_USE naming the value a price uses, or STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .json_request::<PricingDimensionKeyPatch>(openapi, "Request")
        .param(header("If-Match"))
        .handler(patch_dimensions)
        .json_response_with_schema::<PricingDimensionRegistry>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/price-books/{id}/entries")
        .operation_id("bss_pricing.create_entry")
        .summary("Add a SKU to a price book")
        .description(
            "Adds an entry for a SKU to a book in a model fixed for the entry's life (D-427), \
             reserving the SKU reference in Products before the write and confirming it after; \
             the Idempotency-Key replays the receipt. The invoice-line override is at most 2000 \
             characters (D-457). Usage entries require an immutable usage_rating_policy; other \
             charge kinds refuse one. Policy identity is server-issued (D-502). \
             `quantity_semantics.fold`, `reset` and `partial_window` may be absent or null; \
             the server fills `SUM`, `rating_window_start` and \
             `actual_quantity_full_thresholds` (D-513). An unknown value is still refused. \
             Refusals: \
             400 MISSING_RATING_POLICY, UNEXPECTED_RATING_POLICY, METER_POLICY_MISMATCH, \
             MODEL_INVALID, MODEL_KIND_CHARGEKIND_MISMATCH \
             (judged at the door and again after the reservation), ENTRY_PERIOD_INVALID, \
             DIM_NOT_DECLARED, or FIELD_TOO_LONG on an override over its cap; 409 ENTRY_KEY_TAKEN \
             (the SKU, charge kind, normalized period, model and policy digest are taken in the book), SKU_DRAFT, \
             SKU_DEPRECATED, SKU_RETIRING, SKU_FENCED, BUNDLE_SKU_NOT_PRICEABLE or \
             CHARGE_KIND_SKU_TYPE; 503 REGISTRY_UNAVAILABLE.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .json_request::<dto::PricingPriceBookEntryCreate>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(create_entry)
        .json_response_with_schema::<dto::PricingPriceBookEntryDto>(
            openapi,
            StatusCode::CREATED,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // D-517: `$filter` is declared by hand. This read accepts `id eq` and `id in` only, and
    // `with_odata_filter` publishes the toolkit's operator table for a uuid field, `eq|ne|in`, so
    // the contract would offer `ne`, which the read refuses. The exemption from DE0802 sits on this
    // one route's statement, so the other routes registered here keep the rule (products' browse
    // door declares its free `$filter` the same way).
    #[allow(unknown_lints, de0802_use_odata_ext)]
    let router = OperationBuilder::get("/bss-pricing/v1/price-book-entries")
        .operation_id("bss_pricing.list_sku_entries")
        .summary("Where a SKU is priced")
        .description(
            "Lists one page of the tenant's price book entries of one SKU across its books \
             (D-434, D-486), or the entries named by `$filter=id in (...)`, at most 200 ids, \
             instead of sku_id (D-517). Each item carries its book's code, name and currency, \
             its usage (D-428), \
             its status and changing on today, its current_price, the default chain's approved \
             price in force today, and its next_price, the default chain's earliest price \
             scheduled after today, else its newest draft or pending price (the highest \
             version_no), else null (D-472). status is priced when an approved price is in force \
             today, else scheduled when an approved price starts later, else unpriced; changing \
             is true when a draft or pending price exists. Both prices are shown to a caller who \
             also holds price_book read (the export's grant) and are null otherwise; status and \
             changing are not money. The list narrows in memory by book_id (1 to 50 distinct \
             ids), currency, q (a case-insensitive literal substring of the book's code or \
             name), status (priced, scheduled, unpriced) and changing (true or false). $orderby \
             is book_name (the default) or status, asc or desc; the id breaks a tie in that \
             direction. limit (default 500, clamped at 500) and cursor from page_info page it; a \
             cursor carries the order and a hash of the plain keys, so a continuation sends no \
             $orderby. `$filter` is `id in (...)` of at most 200 ids, or `id eq` one id, and \
             replaces sku_id; another \
             field, `or`, `ne`, more than 200 ids, or a filter longer than 8192 bytes is refused. Refusals: 400 QUERY_INVALID without \
             exactly one well-formed sku_id and without that filter, for a repeated key, for any \
             other key, for $select or $count, for sku_id beside $filter, for a malformed \
             book_id, currency, status, changing or limit, or for more than 50 book ids; 400 \
             INVALID_ORDERBY_FIELD for any other order; 400 ORDER_WITH_CURSOR for $orderby \
             beside a cursor; 400 FILTER_MISMATCH for a cursor replayed under another narrowing; \
             400 for a cursor that does not read.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param(
            "sku_id",
            false,
            "The SKU whose entries are listed. Required unless $filter=id in (...) is sent",
        )
        .query_param(
            "$filter",
            false,
            "`id eq <id>` or `id in (<id>, ...)`: 1 to 200 distinct price book entry ids, at most \
             8192 bytes; replaces sku_id and takes no other key. Another field, `ne`, `or` or \
             any other shape is 400 QUERY_INVALID",
        )
        .query_param(
            "book_id",
            false,
            "1 to 50 distinct price book ids, comma-separated",
        )
        .query_param("currency", false, "Three-letter currency code")
        .query_param(
            "q",
            false,
            "Case-insensitive literal substring of the book's code or name",
        )
        .query_param(
            "status",
            false,
            "priced, scheduled or unpriced, comma-separated",
        )
        .query_param_typed(
            "changing",
            false,
            "true when a draft or pending price exists",
            "boolean",
        )
        .query_param_typed(
            "limit",
            false,
            "Page size (default 500, clamped at 500)",
            "integer",
        )
        .query_param_typed("cursor", false, "Continuation from page_info", "string")
        .with_odata_orderby::<price_book_entries::SkuEntryOrderField>()
        .handler(list_sku_entries)
        .json_response_with_schema::<dto::PricingSkuEntryList>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/price-book-entries/{id}")
        .operation_id("bss_pricing.get_entry")
        .summary("Read a price book entry")
        .description(
            "Returns one price book entry of the tenant, its version as the ETag a following PATCH \
             sends back as If-Match, its usage (D-428): its prices by state (a rejected price is \
             not counted; the approved ones also as scheduled, active and superseded today, \
             D-440), the distinct plans whose draft, pending, scheduled or published revisions \
             name it, and the distinct plans that name it only through superseded revisions; its \
             current_price, the default chain's approved price in force today; and its \
             next_price, the default chain's earliest price scheduled after today, else its \
             newest draft or pending price (the highest version_no), else null (D-472). Both \
             prices are shown to a caller who also holds price_book read on its book and are null \
             otherwise (D-434, D-440). Refusals: 404 ENTRY_NOT_FOUND.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book entry id")
        .handler(get_entry)
        .json_response_with_schema::<dto::PricingPriceBookEntryReadDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/price-book-entries/{id}/prices")
        .operation_id("bss_pricing.list_entry_prices")
        .summary("List an entry's prices")
        .description(
            "Lists every price of one entry of the tenant in every state (D-440), each with its \
             status today (draft, pending, rejected, or an approved price's scheduled, active or \
             superseded): the default chain first, then each dimension value's chain in \
             ascending order, each chain by effective_from, then version_no. status keeps one \
             status or several, comma-separated. The prices are money: the caller reaches the \
             entry with price_book_entry read and needs price_book read on its book as well, \
             judged a second time (D-434). Refusals: 400 QUERY_INVALID for an unknown status or \
             any other key; 404 ENTRY_NOT_FOUND; 403 PRICE_BOOK_READ_REQUIRED without price_book \
             read on the entry's book; 503 when the policy cannot judge it.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book entry id")
        .query_param(
            "status",
            false,
            "Display statuses to keep, comma-separated: draft, pending, rejected, scheduled, \
             active, superseded",
        )
        .handler(list_entry_prices)
        .json_response_with_schema::<dto::PricingEntryPriceList>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-pricing/v1/price-book-entries/{id}")
        .operation_id("bss_pricing.patch_entry")
        .summary("Change a price book entry")
        .description(
            "Changes an entry's invoice-line override or its dimension key at the version the \
             caller read (If-Match). The override is at most 2000 characters (D-457). Refusals: \
             400 DIM_NOT_DECLARED, an invalid line template, or FIELD_TOO_LONG on an override \
             over its cap; 404 ENTRY_NOT_FOUND; 409 DIMENSION_KEY_IN_USE while a price uses the \
             key, INVOICE_LINE_LOCKED for an override change once the entry has an approved or \
             pending price (D-426), or STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book entry id")
        .json_request::<dto::PricingPriceBookEntryPatch>(openapi, "Request")
        .param(header("If-Match"))
        .handler(patch_entry)
        .json_response_with_schema::<dto::PricingPriceBookEntryDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete("/bss-pricing/v1/price-book-entries/{id}")
        .operation_id("bss_pricing.delete_entry")
        .summary("Delete a price book entry")
        .description(
            "Deletes an entry with its draft and rejected prices and releases its SKU reference in \
             Products. Refusals: 403 NOT_DRAFT_AUTHOR for another author's draft; 409 \
             ENTRY_PRICES_IN_USE (approved or pending prices), ENTRY_IN_USE (a plan item names it) \
             or ENTRY_CONFIRMATION_PENDING.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book entry id")
        .handler(delete_entry)
        .no_content_response(StatusCode::NO_CONTENT, "Deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/reference-ops")
        .operation_id("bss_pricing.list_reference_ops")
        .summary("List durable reference work")
        .description(
            "Lists the tenant's durable Products reference operations (reserve, confirm and \
             release work) in op-id order, filtered by state, at most limit (default 100) after \
             the cursor. Refusals: 400 REFERENCE_OP_STATE_INVALID, LIMIT_INVALID or QUERY_INVALID.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param("state", false, "Reference op state")
        .query_param("limit", false, "Batch size, 1 to 1000")
        .query_param("cursor", false, "Exclusive op-id cursor")
        .handler(list_reference_ops)
        .json_response_with_schema::<dto::PricingReferenceOpPage>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = plan_routes::item_routes(plan_routes::routes(router, openapi), openapi);
    approval_routes(price_routes(router, openapi), openapi)
        .layer(Extension(state))
        .layer(axum::middleware::from_fn(correlation::establish))
}
/// Submission, publish changes, the approval queue, votes and the quorum policy.
#[allow(
    clippy::too_many_lines,
    reason = "one OperationBuilder chain per route keeps every door's contract in one place"
)]
fn approval_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post("/bss-pricing/v1/prices/{id}/cancel")
        .operation_id("bss_pricing.cancel_price")
        .summary("Cancel a scheduled price")
        .description(
            "Opens a draft cancel of an approved price that has not started (D-520): a price \
             row with change_kind cancel and target_price_id, which carries the price's money \
             unchanged and is never a price in force. Submit it as any draft price, with POST \
             /prices/{id}/submit on the row or the book's publish-changes; the guards run here, \
             at submit and again at apply. On approval the price becomes cancelled and the price \
             before it re-opens onto the next start that remains. Refusals: 400 BODY_UNEXPECTED \
             for a body other than {}; 409 PRICE_NOT_SCHEDULED (not approved, or started), \
             PRICE_CHANGE_PENDING (another pending change names it), PRICE_BOUND (a consumer's \
             binding names it: an acceptance whose bindings name the price; keep_for_bound alone \
             never refuses) or ENTRY_REFERENCE_LOST; the vote that applies the unit answers 409 \
             PRICE_ALREADY_STARTED when the price started after the submit. The Idempotency-Key \
             replays the answer.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .param(header("Idempotency-Key"))
        .handler(cancel_price)
        .json_response_with_schema::<dto::PricingPriceDto>(openapi, StatusCode::CREATED, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/prices/{id}/end")
        .operation_id("bss_pricing.end_price")
        .summary("End a live price")
        .description(
            "Opens a draft end of an approved price, live or scheduled, that has not ended \
             (D-521): a price row with change_kind end, target_price_id and the new end, which \
             carries the price's money unchanged and is never a price in force. effective_to is \
             after today, after the price's start, and no later than its current end (the next \
             start, or its own explicit end). Submit it as any draft price; the guards run here, \
             at submit and again at apply. On approval the price is closed explicitly at that \
             end, and a successor that starts inside it still ends it at its start (D-390). \
             Refusals: 400 END_DATE_INVALID; 409 PRICE_CHANGE_PENDING (another pending change \
             names it), PRICE_ALREADY_ENDED (not approved, or ended) or ENTRY_REFERENCE_LOST. \
             The Idempotency-Key replays the answer.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .json_request::<dto::PricingPriceEnd>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(end_price)
        .json_response_with_schema::<dto::PricingPriceDto>(openapi, StatusCode::CREATED, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/prices/{id}/submit")
        .operation_id("bss_pricing.submit_price")
        .summary("Submit a draft price")
        .description(
            "Puts one draft price into a prices approval unit; at quorum 0 the unit applies at \
             once. A half of a temporary pair is not submitted alone: it is 400 PAIR_SPLIT, and \
             the pair goes through POST /price-books/{id}/publish-changes, which completes it \
             (D-405). It takes no body and no note: a note for the approver travels with \
             publish-changes (D-464). Refusals: 400 BODY_UNEXPECTED for a body with any key; 400 \
             PAIR_SPLIT, or a rule the price breaks at submit (for example WINDOW_START_IN_PAST, \
             PAIR_RETURN_STALE or CHAIN_MODEL_CHANGED, or END_DATE_INVALID for an end, D-521); \
             409 PRICE_NOT_DRAFT, PRICE_LOCKED_PENDING or UNIT_CONTENDED, or a guard of a cancel \
             or an end (PRICE_NOT_SCHEDULED, PRICE_CHANGE_PENDING, PRICE_BOUND, \
             PRICE_ALREADY_ENDED, D-520, D-521), or a price of a released entry (BOOK_ARCHIVED \
             while its book is archived, else ENTRY_REFERENCE_RELEASED, D-522); 503 \
             REGISTRY_UNAVAILABLE when Products cannot answer a usage chain's dated metering \
             read (D-402).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .param(header("Idempotency-Key"))
        .handler(submit_price)
        .json_response_with_schema::<dto::PricingSubmitReceipt>(
            openapi,
            StatusCode::CREATED,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/plan-revisions/{id}/submit")
        .operation_id("bss_pricing.submit_plan_revision")
        .summary("Submit a plan revision")
        .description(
            "Puts an unlocked draft revision whose checks are all green into a plan_revision \
             approval unit (plan submit); at quorum 0 it applies at once. An applied revision is \
             published, or scheduled when its sale date is after today: it takes effect on that \
             date (D-449). An optional body carries the submitter's note for the approver, stored \
             on the unit as submit_note (D-464): no body, {} and a null note carry none. The \
             receipt's revision says when it was submitted and approved (D-461) and, while it is \
             pending, its vote progress (D-462). Refusals: 400 NOTE_TOO_LONG for a note over 2000 \
             characters, judged before anything is read, and BODY_UNEXPECTED for any other key; \
             400 REVISION_CHECKS_RED with the red checks; 409 REVISION_NOT_DRAFT or \
             ROW_LOCKED_PENDING; 503 REGISTRY_UNAVAILABLE when Products cannot answer the checks' \
             SKU reads.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Plan revision id")
        .json_request::<dto::PricingPlanRevisionSubmitRequest>(
            openapi,
            "Optional: the submitter's note, at most 2000 characters (400 NOTE_TOO_LONG); stored \
             on the unit as submit_note (D-464)",
        )
        .request_optional()
        .param(header("Idempotency-Key"))
        .handler(submit_plan_revision)
        .json_response_with_schema::<dto::PricingPlanRevisionSubmitReceipt>(
            openapi,
            StatusCode::CREATED,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/plan-revisions/{id}/unschedule")
        .operation_id("bss_pricing.unschedule_plan_revision")
        .summary("Withdraw a scheduled revision")
        .description(
            "Returns a plan revision that is approved and waiting for its sale date to an \
             unlocked draft of its author (plan submit, D-452): its items and their SKU \
             references stay, the applied unit stays in the history, and no event is sent. A \
             revision whose date has come is switched first and is then in effect. The \
             Idempotency-Key replays the answer. Refusals: 404 for a revision the tenant does \
             not hold; 409 REVISION_IN_EFFECT for a published revision, REVISION_NOT_SCHEDULED \
             for any other.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Plan revision id")
        .param(header("Idempotency-Key"))
        .handler(unschedule_plan_revision)
        .json_response_with_schema::<dto::PricingPlanRevisionDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/price-books/{id}/publish-changes")
        .operation_id("bss_pricing.list_publish_changes")
        .summary("Preview a book's publish changes")
        .description(
            "Lists the book's draft prices as they would be published, each with its entry, chain, \
             approved predecessor and pair partner, and the live impact on entries and plans. \
             Refusals: 404 for a book the tenant does not hold.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .handler(list_publish_changes)
        .json_response_with_schema::<dto::PricingPublishChanges>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/price-books/{id}/publish-changes")
        .operation_id("bss_pricing.publish_changes")
        .summary("Publish a book's draft prices")
        .description(
            "Submits the book's draft prices, all of them or the listed price_ids, optionally on a \
             common effective date, as one prices approval unit; at quorum 0 it applies at once. \
             An optional note for the approver is stored on the unit as submit_note (D-464). \
             Refusals: 400 NOTE_TOO_LONG for a note over 2000 characters, judged before anything \
             is read; 400 NO_DRAFT_PRICES, PRICE_NOT_IN_BOOK or PAIR_SPLIT, or END_DATE_INVALID \
             for an end (D-521); 409 PRICE_LOCKED_PENDING or UNIT_CONTENDED, or a guard of a \
             cancel or an end (PRICE_NOT_SCHEDULED, PRICE_CHANGE_PENDING when another pending \
             change or the same unit names its price, PRICE_BOUND, PRICE_ALREADY_ENDED, D-520, \
             D-521), or a price of a released entry (BOOK_ARCHIVED while its book is archived, \
             else ENTRY_REFERENCE_RELEASED, D-522); 503 REGISTRY_UNAVAILABLE when Products \
             cannot answer a usage chain's dated metering read (D-402).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .json_request::<dto::PricingPublishChangesRequest>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(publish_changes)
        .json_response_with_schema::<dto::PricingSubmitReceipt>(
            openapi,
            StatusCode::CREATED,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/approval-units")
        .operation_id("bss_pricing.list_approval_units")
        .summary("List the approval units")
        .description(
            "One page of the tenant's approval units in submission order (D-458), filtered by \
             state, kind and referenced aggregate, each with its stored snapshot, its decisions, \
             its live impact and caller_can_approve, whether the caller may approve it now (the \
             approval engine's rule, D-471). `$orderby=submitted_at desc` pages it newest first, \
             and `submitted_at asc`, the default, oldest first; the unit id breaks a tie in the \
             same direction (D-470). A client merging pages of several gears compares \
             submitted_at as an instant, never as text, then the id as lower-case hex. \
             `impact=false` skips the live impact read: every unit answers impact null. `limit` \
             (default 200, clamped at 500) and `cursor` from `page_info` page it; a cursor carries \
             its order, so a continuation sends no `$orderby`. Refusals: 400 UNIT_STATE_INVALID \
             for an unknown state; 400 QUERY_INVALID on kind for a kind other than prices or \
             plan_revision, and for a query that does not parse; 400 FILTER_MISMATCH for a cursor \
             replayed with another state, kind \
             or referenced aggregate; 400 for a cursor that does not read; 400 ORDER_WITH_CURSOR \
             for `$orderby` beside a cursor; 400 INVALID_ORDERBY_FIELD for any other order.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param("state", false, "Unit state")
        .query_param("kind", false, "Approval kind: prices or plan_revision")
        .query_param("ref_id", false, "Referenced aggregate id")
        .query_param("book_id", false, "Price book id")
        .query_param_typed(
            "limit",
            false,
            "Page size (default 200, clamped at 500)",
            "integer",
        )
        .query_param_typed("cursor", false, "Continuation from page_info", "string")
        .with_odata_orderby::<UnitOrderField>()
        .query_param_typed(
            "impact",
            false,
            "false skips the live impact read (impact null); true by default",
            "boolean",
        )
        .handler(list_approval_units)
        .json_response_with_schema::<dto::PricingApprovalUnitList>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/approval-units/counts")
        .operation_id("bss_pricing.count_approval_units")
        .summary("Count the approval units")
        .description(
            "Counts the tenant's approval units that the list's narrowing keeps (state, kind and \
             the referenced aggregate, ref_id or book_id), under the list's own grant: by_state \
             (pending, approved, rejected, withdrawn), by_kind (prices, plan_revision), each named \
             with 0 when none, and total, the length of the list under the same narrowing \
             (D-470). It reads one grouped statement, whatever the number of units. It takes \
             nothing but the narrowing. Refusals: the list's: 400 UNIT_STATE_INVALID for an \
             unknown state; 400 QUERY_INVALID on kind for a kind other than prices or \
             plan_revision; 400 QUERY_INVALID for a ref_id and a book_id that differ, a malformed \
             id, or any other key (limit, cursor, $orderby, impact).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param("state", false, "Unit state")
        .query_param("kind", false, "Approval kind: prices or plan_revision")
        .query_param("ref_id", false, "Referenced aggregate id")
        .query_param("book_id", false, "Price book id")
        .handler(count_approval_units)
        .json_response_with_schema::<dto::PricingApprovalUnitCounts>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/approval-units/{id}")
        .operation_id("bss_pricing.get_approval_unit")
        .summary("Read an approval unit")
        .description(
            "Returns one approval unit with its stored snapshot, its decisions, the live impact \
             and caller_can_approve, whether the caller may approve it now (D-471). Refusals: 404 \
             for a unit the tenant does not hold.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Approval unit id")
        .handler(get_approval_unit)
        .json_response_with_schema::<dto::PricingApprovalUnitDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/approval-units/{id}/approve")
        .operation_id("bss_pricing.approve_unit")
        .summary("Approve an approval unit")
        .description(
            "Records an approving vote on the generation the reviewer saw; the vote that reaches \
             the quorum applies the unit (a plan revision whose sale date is after today is \
             scheduled for that date, D-449). The vote's note is at most 2000 characters. \
             Refusals: 400 GENERATION_MISMATCH, UNIT_STALE or NOTE_TOO_LONG; 403 SOD_VIOLATION \
             for the submitter or the author; 409 DUPLICATE_VOTE, UNIT_ALREADY_DECIDED or \
             APPLY_REFUSED, and PRICE_ALREADY_STARTED when a price the unit cancels started \
             after its submit (D-520); 503 REGISTRY_UNAVAILABLE when Products cannot answer a \
             read the applying vote's rules make: a plan revision's checks, or a usage chain's \
             dated metering (D-402).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Approval unit id")
        .json_request::<dto::PricingVoteRequest>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(approve_unit)
        .json_response_with_schema::<dto::PricingVoteReceipt>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/approval-units/{id}/reject")
        .operation_id("bss_pricing.reject_unit")
        .summary("Reject an approval unit")
        .description(
            "Rejects a pending unit on the generation the reviewer saw, with a note of at most \
             2000 characters. A plan revision returns to draft. The prices of a prices unit stay \
             rejected, with their review history, and are not edited again: a replacement is a \
             new draft price (POST /price-book-entries/{id}/prices). Refusals: 400 NOTE_REQUIRED, \
             NOTE_TOO_LONG, GENERATION_MISMATCH or UNIT_STALE; 409 DUPLICATE_VOTE or \
             UNIT_ALREADY_DECIDED.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Approval unit id")
        .json_request::<dto::PricingVoteRequest>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(reject_unit)
        .json_response_with_schema::<dto::PricingVoteReceipt>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-pricing/v1/approval-units/{id}/withdraw")
        .operation_id("bss_pricing.withdraw_unit")
        .summary("Withdraw an approval unit")
        .description(
            "The submitter withdraws a pending unit and its content returns to draft. Refusals: \
             403 NOT_SUBMITTER for anyone else; 409 UNIT_ALREADY_DECIDED.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Approval unit id")
        .param(header("Idempotency-Key"))
        .handler(withdraw_unit)
        .json_response_with_schema::<dto::PricingVoteReceipt>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/approval-policy")
        .operation_id("bss_pricing.get_approval_policy")
        .summary("Read the approval policy")
        .description(
            "Returns the tenant's default quorum and the per-kind overrides, with a content ETag a \
             following PUT sends back as If-Match. Only a caller without config read is refused \
             (403).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .handler(get_approval_policy)
        .json_response_with_schema::<dto::PricingApprovalPolicyDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete("/bss-pricing/v1/approval-policy/{kind}")
        .operation_id("bss_pricing.delete_approval_policy_override")
        .summary("Reset a kind's quorum to the default")
        .description(
            "Removes one kind's override (prices, plan_revision) at the policy the caller read \
             (If-Match), so the kind follows the default quorum again (D-435); answers the policy \
             with its new ETag. Refusals: 400 POLICY_DEFAULT_REQUIRED for the default (*), which \
             is never deleted, or POLICY_KIND_INVALID; 404 when the kind has no override; 409 \
             STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("kind", "Approval kind: prices or plan_revision")
        .param(header("If-Match"))
        .handler(delete_approval_policy)
        .json_response_with_schema::<dto::PricingApprovalPolicyDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/approval-policy/{kind}/effective")
        .operation_id("bss_pricing.get_effective_approval_policy")
        .summary("Read the quorum a submit needs")
        .description(
            "Returns kind and quorum_required, the quorum a submit of that kind needs now \
             (D-481): the kind's override, or the tenant default. kind is prices or \
             plan_revision. prices is read under price_book_entry read; plan_revision under plan \
             read. The quorum is not money, and pricing serves no price read of its own, so \
             price read is not the grant. One statement. Refusals: 400 QUERY_INVALID for a kind \
             outside that set; 403 without the kind's grant; 503 when the policy cannot judge.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("kind", "Approval kind: prices or plan_revision")
        .handler(get_effective_policy)
        .json_response_with_schema::<dto::PricingEffectivePolicyDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::put("/bss-pricing/v1/approval-policy")
        .operation_id("bss_pricing.put_approval_policy")
        .summary("Set an approval quorum")
        .description(
            "Sets the default quorum, or one kind's (prices, plan_revision), at the policy the \
             caller read (If-Match). Refusals: 400 POLICY_KIND_INVALID or QUORUM_INVALID; 409 \
             STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .json_request::<dto::PricingApprovalPolicyPut>(openapi, "Request")
        .param(header("If-Match"))
        .handler(put_approval_policy)
        .json_response_with_schema::<dto::PricingApprovalPolicyDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}
async fn cancel_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let digest = preconditions::request_digest(&support::empty_body(&body)?)?;
    let request = prices::ChangeRequest {
        change: crate::infra::prices::Change::Cancel {
            id: Uuid::nil(),
            target: id,
        },
        today: state.clock.now().date(),
    };
    prices::open_change(
        &state.db.db(),
        scope,
        ctx,
        correlation,
        request,
        key,
        digest,
    )
    .await
}
async fn end_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let input: dto::PricingPriceEnd = preconditions::parse_body(&body)?;
    let end = support::date(Some(input.effective_to), "effective_to")
        .ok()
        .flatten()
        .ok_or_else(|| support::invalid("effective_to", "END_DATE_INVALID"))?;
    let request = prices::ChangeRequest {
        change: crate::infra::prices::Change::End {
            id: Uuid::nil(),
            target: id,
            end,
        },
        today: state.clock.now().date(),
    };
    prices::open_change(
        &state.db.db(),
        scope,
        ctx,
        correlation,
        request,
        key,
        digest,
    )
    .await
}
async fn submit_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::SUBMIT,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let digest = preconditions::request_digest(&support::empty_body(&body)?)?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::submit_price(&state.db.db(), cmd, id).await
}
async fn submit_plan_revision(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // D-418: the plan label's own submit action, as `price:submit` gates a price's submit.
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PLAN,
        actions::SUBMIT,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    // D-464: the submitter's optional note, its cap judged before any read.
    let (payload, note) = support::note_body(&body)?;
    caps::note(note.as_deref())?;
    let digest = preconditions::request_digest(&payload)?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::submit_revision(&state.db.db(), cmd, id, note).await
}
async fn unschedule_plan_revision(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // D-452: withdrawing an approved change is plan submit's (D-418), not plan author's.
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PLAN,
        actions::SUBMIT,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let digest = preconditions::request_digest(&support::empty_body(&body)?)?;
    plans::unschedule(state, scope, ctx, correlation, id, key, digest).await
}
async fn list_publish_changes(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let tenant = ctx.subject_tenant_id();
    let body = transaction(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move { approvals::publish_list(tx, &scope, tenant, id).await })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}
async fn publish_changes(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::SUBMIT,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let input: dto::PricingPublishChangesRequest = preconditions::parse_body(&body)?;
    // D-464: the submitter's optional note, its cap judged before any read.
    input.caps()?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::publish(&state.db.db(), cmd, id, input).await
}
async fn list_approval_units(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    uri: axum::http::Uri,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // The read grant and the two flag grants are independent PDP questions, asked at once: the
    // list and the card wait for one round trip. A denied read still answers 403, and the flag
    // grants never fail on a denial (`authz::grant_scope` reads it as no grant).
    let (scope, (approve_scope, submit_scope)) = tokio::try_join!(
        async {
            authz::access_scope(
                &enforcer,
                &ctx,
                &resource_types::APPROVAL_UNIT,
                actions::READ,
                None,
                None,
            )
            .await
            .map_err(authz_failure)
        },
        approval_flag_scopes(&enforcer, &ctx),
    )?;
    let axum::extract::Query(query) =
        axum::extract::Query::<dto::PricingApprovalUnitQuery>::try_from_uri(&uri)
            .map_err(|e| support::invalid_because("query", "QUERY_INVALID", &e.body_text()))?;
    let filter = unit_narrowing(
        query.state.as_deref(),
        query.kind.as_deref(),
        query.ref_id,
        query.book_id,
    )?;
    let page = unit_page(
        &filter,
        query.limit,
        query.cursor.as_deref(),
        query.orderby.as_deref(),
    )?;
    let request = approvals::UnitListRequest {
        filter,
        page,
        impact: query.impact.unwrap_or(true),
        approve_scope,
        submit_scope,
    };
    let caller = ctx.clone();
    let body = transaction(&state.db.db(), move |tx| {
        let (scope, ctx, request) = (scope.clone(), caller.clone(), request.clone());
        Box::pin(async move { approvals::read_unit_page(tx, &scope, &ctx, &request).await })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}
/// The unit list's narrowing, which the counts take too (D-458, D-470): a known state (else 400
/// `UNIT_STATE_INVALID`), a kind pricing records (else 400 `QUERY_INVALID` on `kind`), and the
/// referenced aggregate, `ref_id` or its alias `book_id` (400 `QUERY_INVALID` on `book_id` when
/// the two differ).
fn unit_narrowing(
    state: Option<&str>,
    kind: Option<&str>,
    ref_id: Option<Uuid>,
    book_id: Option<Uuid>,
) -> Result<crate::infra::storage::repo::approval_repo::UnitListFilter, CanonicalError> {
    let state = approvals::state_filter(state)?;
    let kind = kind
        .map(|k| {
            crate::infra::approval_kinds::Kind::parse(k)
                .ok_or_else(|| support::invalid("kind", "QUERY_INVALID"))
        })
        .transpose()?;
    let reference = match (ref_id, book_id) {
        (Some(a), Some(b)) if a != b => return Err(support::invalid("book_id", "QUERY_INVALID")),
        (a, b) => a.or(b),
    };
    Ok(crate::infra::storage::repo::approval_repo::UnitListFilter {
        state,
        kind,
        ref_id: reference,
    })
}
/// The one field the unit list's `$orderby` takes (D-470): `submitted_at`, ascending or descending.
/// The unit id breaks a tie in the same direction; it is the pager's tie-break, not a client key,
/// so it is not declared. `.with_odata_orderby` publishes it in the served contract
/// (`x-odata-orderby`), and [`unit_order`] accepts exactly it (the phase 9 review's theme I).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UnitOrderField {
    SubmittedAt,
}
impl UnitOrderField {
    const fn field(self) -> crate::infra::storage::repo::approval_repo::UnitListField {
        match self {
            Self::SubmittedAt => {
                crate::infra::storage::repo::approval_repo::UnitListField::SubmittedAt
            }
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
/// The unit list's order (D-470): `submitted_at` ascending (also when omitted, D-458) or
/// descending, the id breaking a tie in the same direction. Any other `$orderby` is 400
/// `INVALID_ORDERBY_FIELD`, the toolkit's refusal of an order a list does not take. The list parses
/// its own query rather than take the toolkit's `OData` extractor, which would refuse `limit=0`
/// (the house pager reads one unit) and drop a cursor's cause from its refusal (theme I).
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
/// The unit list's page (D-458): `limit`, and `cursor` from a page's `page_info`, which carries a
/// hash of the narrowing (`state`, `kind` and the referenced aggregate), so a cursor replayed
/// under another is 400 `FILTER_MISMATCH`, as the book list's is (D-442). The order is not part of
/// the hash (D-470): a cursor carries its own (`CursorV1.s`) and a continuation follows it, so
/// every cursor minted before the descending order still reads. `$orderby` beside a cursor is the
/// toolkit's 400 `ORDER_WITH_CURSOR`, judged first, as its `OData` extractor does. Without a
/// cursor, the query carries the order (`approval_repo::submission_order`), the one source the
/// repository reads (the phase 9 review's R36).
fn unit_page(
    filter: &crate::infra::storage::repo::approval_repo::UnitListFilter,
    limit: Option<u64>,
    cursor: Option<&str>,
    orderby: Option<&str>,
) -> Result<toolkit_odata::ODataQuery, CanonicalError> {
    if cursor.is_some() && orderby.is_some() {
        return Err(toolkit_odata::Error::OrderWithCursor.into());
    }
    let direction = unit_order(orderby)?;
    let digest = preconditions::request_digest(&serde_json::json!({
        "state": filter.state.map(bss_approval::UnitState::as_str),
        "kind": filter.kind.map(crate::infra::approval_kinds::Kind::as_str),
        "ref_id": filter.ref_id,
    }))
    .map_err(CanonicalError::from)?;
    let hash = digest
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut hex, b| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            hex.push(char::from(DIGITS[usize::from(b >> 4)]));
            hex.push(char::from(DIGITS[usize::from(b & 0x0f)]));
            hex
        });
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
        query = query
            .with_order(crate::infra::storage::repo::approval_repo::submission_order(direction));
    }
    Ok(query)
}
/// `GET /approval-units/counts` (D-470): under the list's grant, the list's narrowing, counted
/// outside any transaction.
async fn count_approval_units(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    uri: axum::http::Uri,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let axum::extract::Query(query) =
        // The refusal names the key it rejects (the phase 9 review's R43), as the list's does.
        axum::extract::Query::<dto::PricingApprovalUnitCountsQuery>::try_from_uri(&uri)
            .map_err(|e| support::invalid_because("query", "QUERY_INVALID", &e.body_text()))?;
    let filter = unit_narrowing(
        query.state.as_deref(),
        query.kind.as_deref(),
        query.ref_id,
        query.book_id,
    )?;
    // One grouped statement is its own snapshot: it runs on the plain connection, never in the
    // doors' serializable transaction, whose read locks over the scanned units would push
    // concurrent submits and votes into serialization failures (the phase 9 review's R32).
    let conn = state.db.conn().map_err(support::DoorError::from)?;
    Ok(approvals::count_units(&conn, &scope, ctx.subject_tenant_id(), &filter).await?)
}
async fn get_approval_unit(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // The read grant and the two flag grants are independent PDP questions, asked at once: the
    // list and the card wait for one round trip. A denied read still answers 403, and the flag
    // grants never fail on a denial (`authz::grant_scope` reads it as no grant).
    let (scope, (approve_scope, submit_scope)) = tokio::try_join!(
        async {
            authz::access_scope(
                &enforcer,
                &ctx,
                &resource_types::APPROVAL_UNIT,
                actions::READ,
                None,
                None,
            )
            .await
            .map_err(authz_failure)
        },
        approval_flag_scopes(&enforcer, &ctx),
    )?;
    let caller = ctx.clone();
    let body = transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), caller.clone());
        let (approve_scope, submit_scope) = (approve_scope.clone(), submit_scope.clone());
        Box::pin(async move {
            approvals::get_unit(tx, &scope, &ctx, id, &approve_scope, &submit_scope).await
        })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}
async fn approve_unit(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::APPROVE,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let input: dto::PricingVoteRequest = preconditions::parse_body(&body)?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::vote(
        &state.db.db(),
        cmd,
        id,
        approvals::Vote::Approve,
        Some(input),
    )
    .await
}
async fn reject_unit(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::APPROVE,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let input: dto::PricingVoteRequest = preconditions::parse_body(&body)?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::vote(
        &state.db.db(),
        cmd,
        id,
        approvals::Vote::Reject,
        Some(input),
    )
    .await
}
async fn withdraw_unit(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::SUBMIT,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let digest = preconditions::request_digest(&support::empty_body(&body)?)?;
    let (approve_scope, submit_scope) = approval_flag_scopes(&enforcer, &ctx).await?;
    let cmd = approvals::Command {
        scope,
        ctx,
        hub: state.hub.clone(),
        outbox: state.outbox.clone(),
        clock: state.clock.clone(),
        correlation,
        key,
        digest,
        approve_scope,
        submit_scope,
    };
    approvals::vote(&state.db.db(), cmd, id, approvals::Vote::Withdraw, None).await
}
async fn get_effective_policy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(kind): Path<String>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let kind = crate::infra::approval_kinds::Kind::parse(&kind)
        .ok_or_else(|| support::invalid("kind", "QUERY_INVALID"))?;
    let resource = match kind {
        crate::infra::approval_kinds::Kind::Prices => &resource_types::PRICE_BOOK_ENTRY,
        crate::infra::approval_kinds::Kind::PlanRevision => &resource_types::PLAN,
    };
    authz::access_scope(&enforcer, &ctx, resource, actions::READ, None, None)
        .await
        .map_err(authz_failure)?;
    let tenant = ctx.subject_tenant_id();
    transaction(&state.db.db(), move |tx| {
        Box::pin(async move { approvals::effective_quorum(tx, tenant, kind).await })
    })
    .await
}
async fn get_approval_policy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move { approvals::get_policy(tx, &scope, ctx.subject_tenant_id()).await })
    })
    .await
}
async fn put_approval_policy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let input: dto::PricingApprovalPolicyPut = preconditions::parse_body(&body)?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, input) = (scope.clone(), ctx.clone(), input.clone());
        Box::pin(async move {
            approvals::put_policy(tx, &scope, &ctx, correlation, version, input).await
        })
    })
    .await
}
async fn delete_approval_policy(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(kind): Path<String>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, kind) = (scope.clone(), ctx.clone(), kind.clone());
        Box::pin(async move {
            approvals::reset_policy(tx, &scope, &ctx, correlation, version, &kind).await
        })
    })
    .await
}
/// Draft price authoring: create, patch and delete.
fn price_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post("/bss-pricing/v1/price-book-entries/{id}/prices")
        .operation_id("bss_pricing.create_price")
        .summary("Create a draft price")
        .description(
            "Adds a draft price to an entry's chain, and a temporary price's return partner with \
             it; the Idempotency-Key replays the answer. The money is in the entry's model and \
             the price carries no model of its own (D-427). The note is at most 2000 characters \
             (D-457). Refusals: 400 for a rule the price breaks (for example PRICE_MISSING for \
             money of another model's shape, AMOUNT_INVALID, WINDOW_START_IN_PAST, \
             DIM_VALUE_UNKNOWN or PRICE_INSIDE_TEMPORARY), or NOTE_TOO_LONG; 409 \
             ENTRY_REFERENCE_LOST.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book entry id")
        .json_request::<dto::PricingPriceCreate>(openapi, "Request")
        .param(header("Idempotency-Key"))
        .handler(create_price)
        .json_response_with_schema::<dto::PricingPriceCreated>(
            openapi,
            StatusCode::CREATED,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-pricing/v1/prices/{id}")
        .operation_id("bss_pricing.patch_price")
        .summary("Change a draft price")
        .description(
            "Changes an unlocked draft price of its author at the version the author read \
             (If-Match). The temporary half of a draft takes effective_from and temporary_until: \
             its pair is built again over the new dates in the same transaction (D-443) - its \
             return re-derived in place, deleted when the chain's next price starts on the new \
             end or nothing of the chain is in force there, or created when a price is to be \
             returned to - and both halves are judged as the create judges them; the answer is \
             the edited price, its paired_price_id naming its partner now, or null. A return's \
             own dates, a temporary price's chain (dim_value), an end on any other price and a \
             null end are 400 TEMPORARY_PRICE_FIXED. Refusals: 400 for a rule the change breaks \
             (for example WINDOW_END_INVALID, WINDOW_START_IN_PAST or \
             TEMPORARY_SPANS_A_CHANGE), or NOTE_TOO_LONG on a note over 2000 characters \
             (D-457); 403 NOT_DRAFT_AUTHOR; 409 PRICE_NOT_DRAFT (the price or \
             its partner, or a cancel or end row, which is deleted instead, D-520) or \
             STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .json_request::<dto::PricingPricePatch>(openapi, "Request")
        .param(header("If-Match"))
        .handler(patch_price)
        .json_response_with_schema::<dto::PricingPriceDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::delete("/bss-pricing/v1/prices/{id}")
        .operation_id("bss_pricing.delete_price")
        .summary("Delete a draft price")
        .description(
            "Deletes an unlocked draft price of its author at the version the author read \
             (If-Match). Refusals: 403 NOT_DRAFT_AUTHOR; 409 PRICE_NOT_DRAFT or STALE_REVISION.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .param(header("If-Match"))
        .handler(delete_price)
        .no_content_response(StatusCode::NO_CONTENT, "Deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}
async fn create_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let input: dto::PricingPriceCreate = preconditions::parse_body(&body)?;
    input.caps()?;
    prices::create(
        &state.db.db(),
        scope,
        ctx,
        correlation,
        id,
        key,
        digest,
        input,
    )
    .await
}
async fn patch_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let input: dto::PricingPricePatch = preconditions::parse_body(&body)?;
    input.caps()?;
    prices::patch(&state.db.db(), scope, ctx, correlation, id, version, input).await
}
async fn delete_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move { prices::delete(tx, &scope, &ctx, correlation, id, version).await })
    })
    .await
}
async fn create_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    let body: PriceBookCreate = preconditions::parse_body(&body)?;
    body.caps()?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, body) = (scope.clone(), ctx.clone(), body.clone());
        let (key, digest) = (key.clone(), digest.clone());
        Box::pin(
            async move { books::create(tx, &scope, &ctx, correlation, &key, &digest, body).await },
        )
    })
    .await
}
async fn get_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let backend = state.db.db().backend();
    let today = time::OffsetDateTime::now_utc().date();
    let tx_ctx = ctx.clone();
    let read = transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), tx_ctx.clone());
        Box::pin(async move {
            let tenant = ctx.subject_tenant_id();
            let m = books::find(tx, &scope, tenant, id).await?;
            let version = preconditions::RowVersion::from_stored(m.version)
                .map_err(CanonicalError::from)?
                .get();
            // D-441: the book with its stats, as the list answers it.
            let body = books::with_stats(tx, tenant, backend, vec![m], today)
                .await?
                .pop()
                .ok_or_else(|| CanonicalError::internal("the read book is gone").create())?;
            Ok((body, version))
        })
    })
    .await?;
    // D-522: the archiving actor's name (D-519), after the transaction.
    let (body, version) = read;
    names::named(&state, &ctx, body, Some(version)).await
}
async fn patch_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let body: PriceBookPatch = preconditions::parse_body(&body)?;
    body.caps()?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, body) = (scope.clone(), ctx.clone(), body.clone());
        Box::pin(
            async move { books::patch(tx, &scope, &ctx, correlation, id, version, body).await },
        )
    })
    .await
}
async fn delete_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let backend = state.db.db().backend();
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(
            async move { books::delete(tx, &scope, &ctx, correlation, backend, id, version).await },
        )
    })
    .await
}
/// The archive mark of a finished book (D-522): `archive` and `unarchive`.
fn archive_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post("/bss-pricing/v1/price-books/{id}/archive")
        .operation_id("bss_pricing.archive_book")
        .summary("Archive a finished price book")
        .description(
            "Marks a finished book archived at the version the caller read (If-Match), with an \
             audit row (D-522): `archived_at` and `archived_by` are set and the version moves. In \
             the same transaction each entry whose reference is confirmed or lost becomes \
             `released`, with a `release` op (reason book_archived) that releases its SKU \
             reference in Products; the door drives those ops after the commit, at most 8 at once \
             and for at most 3 s in all, and the ticker finishes what it does not. A SKU that only this book named then stops being \
             referenced, so it can be retired. The entries and prices stay and are read-only: an \
             entry create or PATCH, a price create, cancel, end or submit, and a plan item naming \
             an entry of the book are 409 BOOK_ARCHIVED. `GET /price-books` leaves an archived \
             book out unless asked `archived eq true`; a read by id, the export and the consumer \
             reads ignore the mark. Archiving an archived book answers it unchanged. Refusals, in \
             order: 403 without the book write grant; 400 for a missing or malformed If-Match; 404 \
             for a book the tenant does not hold; 409 STALE_REVISION; 409 BOOK_IN_PLAN (a plan \
             with a draft, pending, scheduled or published revision on the book; superseded ones \
             do not refuse it); 409 BOOK_HAS_PENDING (a prices unit of the book in review: a \
             pending price, cancel or end); 409 \
             ENTRY_CONFIRMATION_PENDING (an entry's reference is being confirmed).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .param(header("If-Match"))
        .handler(archive_book)
        .json_response_with_schema::<PriceBookDto>(openapi, StatusCode::OK, "Response")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::post("/bss-pricing/v1/price-books/{id}/unarchive")
        .operation_id("bss_pricing.unarchive_book")
        .summary("Unarchive a price book")
        .description(
            "Clears a book's archive mark at the version the caller read (If-Match), with an audit \
             row (D-522). Each `released` entry gets a `rereserve` op, and the door drives them \
             after the commit (at most 8 at once, for at most 3 s in all; the ticker finishes the \
             rest). An entry whose SKU refuses the new reservation (retired, say) stays `released` \
             and read-only (409 ENTRY_REFERENCE_RELEASED), and the book is unarchived anyway: \
             `released_entries` lists the entries still released when the answer is built, or is \
             null when they could not be read after the unarchive committed. \
             Unarchiving a book that is not archived answers it unchanged. Refusals, in order: 403 \
             without the book write grant; 400 for a missing or malformed If-Match; 404; 409 \
             STALE_REVISION; 409 ENTRY_RELEASE_PENDING (an entry's release or re-reservation is \
             still open: retry once the reference work finishes). A refusal writes nothing.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price book id")
        .param(header("If-Match"))
        .handler(unarchive_book)
        .json_response_with_schema::<dto::PricingPriceBookUnarchiveDto>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}
/// How many reference ops an archive or an unarchive drives at once (D-522).
const DRIVE_CONCURRENCY: usize = 8;
/// How long an archive or an unarchive waits for all of its drives together (D-522).
const DRIVE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);
/// Drive the ops an archive or an unarchive wrote, as the door's caller, at most
/// [`DRIVE_CONCURRENCY`] at once and under one [`DRIVE_DEADLINE`] for the whole door. Each op is
/// durable: an op the door leaves unfinished, failed or cut off by the deadline, is the ticker's to
/// finish (D-522). Each drive runs on a task of its own: a drive opens its own transactions, and
/// toolkit-db refuses a connection to a task that is inside one.
async fn drive_all(state: &Arc<AuthoringState>, ctx: &SecurityContext, ops: &[Uuid]) {
    let deadline = tokio::time::Instant::now() + DRIVE_DEADLINE;
    let mut queue = ops.iter().copied();
    let mut running = tokio::task::JoinSet::new();
    let mut finished = 0_usize;
    loop {
        let room = DRIVE_CONCURRENCY.saturating_sub(running.len());
        for op in queue.by_ref().take(room) {
            running.spawn(drive_one(state.clone(), ctx.clone(), op));
        }
        match tokio::time::timeout_at(deadline, running.join_next()).await {
            Ok(Some(joined)) => {
                finished += 1;
                deferred(joined);
            }
            // Every op is driven, or the deadline passed.
            Ok(None) | Err(_) => break,
        }
    }
    let left = ops.len().saturating_sub(finished);
    if left > 0 {
        tracing::warn!(
            left,
            total = ops.len(),
            deadline_ms = DRIVE_DEADLINE.as_millis(),
            "pricing book archive reference work left to the ticker at the door's deadline"
        );
    }
    // Dropping the set aborts the drives still running; their ops stay durable.
}
/// One op of [`drive_all`], driven to its end as the door's caller.
async fn drive_one(
    state: Arc<AuthoringState>,
    ctx: SecurityContext,
    op: Uuid,
) -> (Uuid, Result<(), CanonicalError>) {
    use crate::infra::reference_work::{self, Caller, WallClock};
    let driven = reference_work::drive(&state, &ctx, op, Arc::new(WallClock), Caller::Door)
        .await
        .map(drop);
    (op, driven)
}
/// Log a drive of [`drive_all`] that did not finish its op: the ticker finishes it.
fn deferred(joined: Result<(Uuid, Result<(), CanonicalError>), tokio::task::JoinError>) {
    match joined {
        Ok((_, Ok(()))) => {}
        Ok((op, Err(error))) => {
            tracing::warn!(op_id=%op, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing book archive reference work deferred to the ticker");
        }
        Err(error) => {
            tracing::warn!(error=%error, "pricing book archive reference drive did not finish; the ticker finishes its op");
        }
    }
}
async fn archive_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // The book-write grant on the book, as its PATCH and DELETE ask it.
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let backend = state.db.db().backend();
    let tx_ctx = ctx.clone();
    let marked = transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), tx_ctx.clone());
        Box::pin(async move {
            books::archive(tx, &scope, &ctx, correlation, backend, id, version).await
        })
    })
    .await?;
    drive_all(&state, &ctx, &marked.ops).await;
    let version = preconditions::RowVersion::from_stored(marked.book.version)
        .map_err(CanonicalError::from)?
        .get();
    response(
        StatusCode::OK,
        &PriceBookDto::from(marked.book),
        Some(version),
    )
}
async fn unarchive_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // The book-write grant on the book, as its PATCH and DELETE ask it.
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let tx_ctx = ctx.clone();
    let marked = transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), tx_ctx.clone());
        Box::pin(async move { books::unarchive(tx, &scope, &ctx, correlation, id, version).await })
    })
    .await?;
    drive_all(&state, &ctx, &marked.ops).await;
    let tenant = ctx.subject_tenant_id();
    // The unarchive has committed: a failed read of the entries still released does not make it an
    // error. The answer says it does not know them (null) rather than invent a list.
    let released_entries = match transaction(&state.db.db(), move |tx| {
        Box::pin(async move { books::released_entries(tx, tenant, id).await })
    })
    .await
    {
        Ok(ids) => Some(ids),
        Err(error) => {
            tracing::warn!(book_id=%id, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing book unarchived; its released entries could not be read for the answer");
            None
        }
    };
    let version = preconditions::RowVersion::from_stored(marked.book.version)
        .map_err(CanonicalError::from)?
        .get();
    response(
        StatusCode::OK,
        &dto::PricingPriceBookUnarchiveDto {
            book: marked.book.into(),
            released_entries,
        },
        Some(version),
    )
}
async fn export_book(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let tenant = ctx.subject_tenant_id();
    let body = transaction(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move { books::export(tx, &scope, tenant, id).await })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}
async fn get_settings(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let tenant = ctx.subject_tenant_id();
    let (body, version) = transaction(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            let body = configuration::settings(tx, &scope, tenant).await?;
            let version = preconditions::RowVersion::from_stored(body.version)
                .map_err(CanonicalError::from)?
                .get();
            Ok((body, version))
        })
    })
    .await?;
    // D-519: the writer's name, after the transaction.
    let answer = names::named(&state, &ctx, body, Some(version)).await?;
    // D-518: the strong version tag stays; a matching If-None-Match is 304.
    Ok(support::revalidate_version(&headers, answer))
}
async fn put_settings(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    // D-457: the caps are judged against the stored settings (`configuration::put_settings`).
    let body: PricingSettingsPut = preconditions::parse_body(&body)?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, body) = (scope.clone(), ctx.clone(), body.clone());
        Box::pin(async move {
            configuration::put_settings(tx, &scope, &ctx, correlation, version, body).await
        })
    })
    .await
}
async fn get_dimensions(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move {
            let tenant = ctx.subject_tenant_id();
            let (body, tag) = configuration::dimensions(tx, &scope, tenant).await?;
            Ok(response(StatusCode::OK, &body, Some(tag))?)
        })
    })
    .await
}
async fn put_dimensions(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    // The caps are judged against the stored registry, in the transaction (D-457: only new text).
    let body: PricingDimensions = preconditions::parse_body(&body)?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, body) = (scope.clone(), ctx.clone(), body.clone());
        Box::pin(async move {
            configuration::put_dimensions(tx, &scope, &ctx, correlation, version, body).await
        })
    })
    .await
}
async fn patch_dimensions(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let body: PricingDimensionKeyPatch = preconditions::parse_body(&body)?;
    body.caps()?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, body) = (scope.clone(), ctx.clone(), body.clone());
        Box::pin(async move {
            configuration::patch_dimensions(tx, &scope, &ctx, correlation, version, body).await
        })
    })
    .await
}
async fn list_sku_entries(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    uri: axum::http::Uri,
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
    // D-486, D-517: the query, including $filter, $select and $count, is judged before any read.
    let query = price_book_entries::sku_entries_query(&uri)?;
    // D-434: the money is the export's — price_book read. Without it the entries still list, each
    // with a null current_price; only an unavailable policy fails the read. Status is not money.
    let books = money_scope(&enforcer, &ctx).await?;
    let today = time::OffsetDateTime::now_utc().date();
    let tenant = ctx.subject_tenant_id();
    let body = transaction(&state.db.db(), move |tx| {
        let (scope, books, query) = (scope.clone(), books.clone(), query.clone());
        Box::pin(async move {
            match &query {
                price_book_entries::EntriesRead::Sku(query) => {
                    price_book_entries::for_sku(tx, &scope, books.as_ref(), tenant, query, today)
                        .await
                }
                price_book_entries::EntriesRead::Ids(ids) => {
                    price_book_entries::for_ids(tx, &scope, books.as_ref(), tenant, ids, today)
                        .await
                }
            }
        })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}

async fn create_entry(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        None,
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let key = preconditions::idempotency_key(&headers)?;
    let payload: serde_json::Value = preconditions::parse_body(&body)?;
    let digest = preconditions::request_digest(&payload)?;
    // The generic parser above validates text/NULs and supplies the replay projection. Decode
    // policy-bearing input from the original bytes as well: a Value would discard duplicate
    // keys before the closed typed policy can refuse them (D-502 canonical profile).
    let input: dto::PricingPriceBookEntryCreate = serde_json::from_slice(&body).map_err(|e| {
        crate::infra::error_mapping::DomainError::InvalidRequest(format!(
            "the request body is not readable: {e}"
        ))
    })?;
    input.caps()?;
    price_book_entries::create(state, scope, ctx, id, correlation, key, digest, input).await
}

async fn get_entry(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::READ,
        None,
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    // D-440: the money is shown as D-434 shows it — price_book read, judged a second time.
    let books = money_scope(&enforcer, &ctx).await?;
    let today = time::OffsetDateTime::now_utc().date();
    let tenant = ctx.subject_tenant_id();
    let (body, version) = transaction(&state.db.db(), move |tx| {
        let (scope, books) = (scope.clone(), books.clone());
        Box::pin(async move {
            let m = price_book_entries::find(tx, &scope, tenant, id).await?;
            let version = preconditions::RowVersion::from_stored(m.version)
                .map_err(CanonicalError::from)?
                .get();
            let shown =
                price_book_entries::shows_money(tx, books.as_ref(), tenant, m.book_id).await?;
            let body = price_book_entries::read(tx, tenant, vec![m], shown, today)
                .await?
                .pop()
                .ok_or_else(|| CanonicalError::internal("the read entry is gone").create())?;
            Ok((body, version))
        })
    })
    .await?;
    names::named(&state, &ctx, body, Some(version)).await
}

/// D-434: the money's grant, `price_book` read, judged a second time in the request: its scope
/// when held, `None` when denied; a policy that cannot judge fails the read (503).
async fn money_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<Option<toolkit_db::secure::AccessScope>, CanonicalError> {
    match authz::access_scope(
        enforcer,
        ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        None,
    )
    .await
    {
        Ok(books) => Ok(Some(books)),
        Err(authz::AuthzError::Denied(_)) => Ok(None),
        Err(unavailable) => Err(authz_failure(unavailable)),
    }
}

/// The `status` of `GET /price-book-entries/{id}/prices` (D-440): absent, or one display status
/// or several, comma-separated. An unknown or empty status, a repeated `status` and any other key
/// are 400 `QUERY_INVALID`.
fn wanted_statuses(
    uri: &axum::http::Uri,
) -> Result<Option<Vec<crate::api::rest::closed_sets::PricingPriceStatus>>, CanonicalError> {
    let axum::extract::Query(query) =
        axum::extract::Query::<dto::PricingEntryPricesQuery>::try_from_uri(uri)
            .map_err(|_| support::invalid("query", "QUERY_INVALID"))?;
    query
        .status
        .map(|raw| {
            raw.split(',')
                .map(|token| {
                    token
                        .parse::<crate::domain::price::DisplayStatus>()
                        .map(Into::into)
                        .map_err(|_| support::invalid("status", "QUERY_INVALID"))
                })
                .collect()
        })
        .transpose()
}

async fn list_entry_prices(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    uri: axum::http::Uri,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::READ,
        None,
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    // D-440: every price is money — price_book read on the entry's book, judged a second time
    // (D-434); here a denial refuses the read (403, after the entry is found), since the whole
    // answer is money.
    let books = money_scope(&enforcer, &ctx).await?;
    let wanted = wanted_statuses(&uri)?;
    let today = time::OffsetDateTime::now_utc().date();
    let tenant = ctx.subject_tenant_id();
    let body = transaction(&state.db.db(), move |tx| {
        let (scope, books, wanted) = (scope.clone(), books.clone(), wanted.clone());
        Box::pin(async move {
            price_book_entries::prices(
                tx,
                &scope,
                books.as_ref(),
                tenant,
                id,
                wanted.as_deref(),
                today,
            )
            .await
        })
    })
    .await?;
    names::named(&state, &ctx, body, None).await
}

async fn patch_entry(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    let version = preconditions::if_match(&headers)?.get();
    let input: dto::PricingPriceBookEntryPatch = preconditions::parse_body(&body)?;
    input.caps()?;
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx, input) = (scope.clone(), ctx.clone(), input.clone());
        Box::pin(async move {
            price_book_entries::patch(tx, &scope, &ctx, correlation, id, version, input).await
        })
    })
    .await
}

async fn delete_entry(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    corr: Option<Extension<correlation::CorrelationId>>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK_ENTRY,
        actions::AUTHOR,
        Some(OwnerTenant(ctx.subject_tenant_id())),
        Some(ResourceRef(id)),
    )
    .await
    .map_err(authz_failure)?;
    let correlation = correlation::require_correlation(corr)?;
    price_book_entries::delete(state, scope, ctx, correlation, id).await
}

async fn list_reference_ops(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    uri: axum::http::Uri,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CONFIG,
        actions::SETTINGS,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let axum::extract::Query(query) =
        axum::extract::Query::<dto::PricingReferenceOpQuery>::try_from_uri(&uri)
            .map_err(|_| support::invalid("query", "QUERY_INVALID"))?;
    let filter = query
        .state
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|_| support::invalid("state", "REFERENCE_OP_STATE_INVALID"))?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(support::invalid("limit", "LIMIT_INVALID"));
    }
    transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move {
            let mut items = crate::infra::storage::repo::reference_op_repo::page(
                tx,
                &scope,
                ctx.subject_tenant_id(),
                filter,
                query.cursor,
                limit + 1,
            )
            .await?;
            let next_cursor = if u64::try_from(items.len()).unwrap_or(u64::MAX) > limit {
                items.pop();
                items.last().map(|op| op.op_id)
            } else {
                None
            };
            Ok(response(
                StatusCode::OK,
                &dto::PricingReferenceOpPage {
                    items: items
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                    next_cursor,
                },
                None,
            )?)
        })
    })
    .await
}

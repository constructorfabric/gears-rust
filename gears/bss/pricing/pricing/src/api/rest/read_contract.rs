//! The consumer read contract (D-419…D-422), mounted beside the authoring router: `GET /resolve`,
//! the chain matrix Rating and Subscriptions bind from, and `GET /prices/{id}`, the pinned price a
//! replay reads.
//!
//! A read writes nothing: no audit row, no idempotency key, no binding. `resolve` reads the stored
//! revision in ONE transaction (the revision, its plan and items, each entry with ALL its prices,
//! the dimension registry, the settings and the `keep_for_bound` ids); an unknown or another
//! tenant's revision is 404 there, before any Products read. The pure model then judges the pins
//! (`domain::resolve::matrix`), and only then, outside the transaction, is each SKU version read
//! as of the date through the detached registry (D-421), as pricing's system actor (D-424): the
//! caller has passed `plan:read` and its tenant holds the revision, the plan read already
//! discloses the items' SKU ids, and Products' registry refuses every other system subject — so a
//! consumer needs pricing's grants only.
//!
//!
//! Every refusal names the type of what it refused (phase 4 review F1): each `GET /resolve`
//! refusal — the query, the grant, the revision, its item, its state and the pins — is a
//! `plan.v1~` resource error, each `GET /prices/{id}` refusal — the id, the grant and the price —
//! a `price.v1~` one. The authoring doors still answer with the price book's type (owed).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-binding-sku-version:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-price-read-forever:p1
pub mod dto;
use super::authoring::{
    AuthoringState,
    support::{self, DoorError, require_authenticated},
};
use super::closed_sets::{
    PricingChargeKind, PricingEligibility, PricingModel, PricingPeriod, PricingPinnedPriceStatus,
    PricingPriceState, PricingResolvedRevisionState,
};
use crate::{
    authz::{self, ResourceRef, actions, resource_types},
    domain::{
        book, dimension,
        resolve::{ItemResolution, Pin, Resolved},
    },
    infra::{
        pricing_reads::{
            PriceResource, PriceSnapshot, ReadSnapshot, load_legacy_resolution, load_price,
            plan_denied, plan_invalid, price_conflict, price_denied, read_failure,
        },
        storage::RepoError,
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{Extension, Router, extract::Path, http::StatusCode, response::Response};
use bss_products_sdk::models::SkuVersion;
use dto::{
    PricingPinnedPriceDto, PricingResolveBindingDto, PricingResolveChainDto, PricingResolveDto,
    PricingResolveInputDto, PricingResolveItemDto, PricingResolveMeterDto, PricingResolveQuery,
    PricingResolveSkuVersionDto,
};
use std::{collections::BTreeMap, sync::Arc};
use time::Date;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Mount the consumer reads.
pub fn router(state: Arc<AuthoringState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::get("/bss-pricing/v1/resolve")
        .operation_id("bss_pricing.resolve")
        .summary("Resolve a plan revision on a date")
        .description(
            "Returns, per item of a published or superseded plan revision, or of a scheduled one \
             on or after its sale date, the chain matrix (the default chain and every dimension \
             value) with the price bound on the date, the SKU version in force and the resolved \
             invoice inputs; pins renew a subscription's bindings. A scheduled revision whose date \
             has come resolves as published, on every date, whether or not its switch is \
             persisted yet. No totals. Refusals: 400 QUERY_INVALID, DATE_INVALID, PIN_FOREIGN, \
             PIN_DUPLICATE or PINS_TOO_MANY; 404 for an unknown revision or item; 409 \
             REVISION_NOT_PUBLISHED for a draft or pending revision, REVISION_NOT_YET_AVAILABLE \
             for a scheduled one before its sale date; Products' own refusal of a SKU read; 503 \
             REGISTRY_UNAVAILABLE when Products cannot answer.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param(
            "plan_revision_id",
            true,
            "A published or superseded plan revision, or a scheduled one from its sale date",
        )
        .query_param("date", true, "The date resolved, YYYY-MM-DD")
        .query_param("item_id", false, "Resolve this one item of the revision")
        .query_param(
            "pins",
            false,
            "Comma-separated current bindings: price_id, or price_id:dim_value for a \
             default-chain price that value was bound to; at most 1000",
        )
        .handler(resolve)
        .json_response_with_schema::<PricingResolveDto>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-pricing/v1/prices/{id}")
        .operation_id("bss_pricing.get_price")
        .summary("Read a pinned price")
        .description(
            "Returns an approved price of the tenant, or a cancelled one (D-520), with its \
             original money whatever its window, with its entry's SKU, charge kind, period, book \
             and currency: stored facts only. A cancelled price carries status `cancelled`; an \
             approved one carries no status, since its display status depends on the day \
             (D-422). A draft, pending, rejected, unknown or foreign price is one and the same \
             404; an id that is not an id is 400 ID_INVALID.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .path_param("id", "Price id")
        .handler(get_price)
        .json_response_with_schema::<PricingPinnedPriceDto>(openapi, StatusCode::OK, "Response")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(state))
}

async fn resolve(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    uri: axum::http::Uri,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PLAN,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(plan_denied)?;
    let request = ResolveRequest::parse(&uri)?;
    resolution(&state, scope, &ctx, request).await
}

async fn get_price(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<String>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let id = Uuid::parse_str(&id).map_err(|_| {
        PriceResource::invalid_argument()
            .with_field_violation("id", "ID_INVALID", "ID_INVALID")
            .create()
    })?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE,
        actions::READ,
        None,
        Some(ResourceRef(id)),
    )
    .await
    .map_err(price_denied)?;
    let tenant = ctx.subject_tenant_id();
    let body = support::transaction_door(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            load_price(tx, &scope, tenant, id)
                .await
                .and_then(render_price)
        })
    })
    .await
    .map_err(|e| read_failure(e, price_conflict))?;
    support::response(StatusCode::OK, &body, None)
}
/// `GET /prices/{id}` below its door (D-422): an approved price of the tenant, as stored, or a
/// cancelled one, which says `cancelled` (D-520).
/// # Errors
/// One and the same 404 for a draft, pending or rejected price, an unknown id and another
/// tenant's id.
fn render_price(snapshot: PriceSnapshot) -> Result<PricingPinnedPriceDto, DoorError> {
    let PriceSnapshot { row, entry, book } = snapshot;
    let id = row.id;
    let e = entry.id;
    Ok(PricingPinnedPriceDto {
        price_id: row.id,
        price_book_entry_id: entry.id,
        sku_id: entry.sku_id,
        charge_kind: PricingChargeKind::stored(
            &entry.charge_kind,
            &format_args!("entry {e} charge_kind"),
        )?,
        period: entry
            .period
            .as_deref()
            .map(|p| PricingPeriod::stored(p, &format_args!("entry {e} period")))
            .transpose()?,
        book_id: book.id,
        currency: book.currency,
        version_no: row.version_no,
        dim_value: row.dim_value,
        model: PricingModel::stored(&entry.model, &format_args!("entry {e} model"))?,
        price: row.price_json,
        min_fee: row.min_fee,
        eligibility: PricingEligibility::stored(
            &row.eligibility,
            &format_args!("price {id} eligibility"),
        )?,
        effective_from: row.effective_from.to_string(),
        effective_to: row.effective_to.map(|d| d.to_string()),
        temporary_until: row.temporary_until.map(|d| d.to_string()),
        keep_for_bound: row.keep_for_bound,
        closed_explicitly: row.closed_explicitly,
        paired_price_id: row.paired_price_id,
        return_of_price_id: row.return_of_price_id,
        approved_by_unit_id: row.approved_by_unit_id,
        approved_at: row.approved_at,
        status: (PricingPriceState::stored(&row.state, &format_args!("price {id} state"))?
            == PricingPriceState::Cancelled)
            .then_some(PricingPinnedPriceStatus::Cancelled),
    })
}

/// A parsed `GET /resolve` query.
struct ResolveRequest {
    revision: Uuid,
    date: Date,
    item: Option<Uuid>,
    pins: Vec<Pin>,
}
impl ResolveRequest {
    /// # Errors
    /// 400 `QUERY_INVALID` for an unknown or repeated parameter and a malformed id; 400
    /// `DATE_INVALID` for a missing or malformed date; 400 `PIN_FOREIGN` for a pin that does not
    /// parse.
    fn parse(uri: &axum::http::Uri) -> Result<Self, CanonicalError> {
        // @cpt-begin:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-1
        let axum::extract::Query(query) =
            axum::extract::Query::<PricingResolveQuery>::try_from_uri(uri)
                .map_err(|_| plan_invalid("query", "QUERY_INVALID"))?;
        let id = |field: &str, text: Option<&str>| {
            text.map(|t| Uuid::parse_str(t).map_err(|_| plan_invalid(field, "QUERY_INVALID")))
                .transpose()
        };
        let revision = id("plan_revision_id", query.plan_revision_id.as_deref())?
            .ok_or_else(|| plan_invalid("plan_revision_id", "QUERY_INVALID"))?;
        let date = support::date(Some(query.date.unwrap_or_default()), "date")
            .ok()
            .flatten()
            .ok_or_else(|| plan_invalid("date", "DATE_INVALID"))?;
        let item = id("item_id", query.item_id.as_deref())?;
        let pins = match query.pins.as_deref() {
            None | Some("") => Vec::new(),
            Some(text) => text.split(',').map(pin).collect::<Result<_, _>>()?,
        };
        // @cpt-end:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-1
        Ok(Self {
            revision,
            date,
            item,
            pins,
        })
    }
}
/// One pin: `price_id`, or `price_id:dim_value` with a value spelled as a dimension value is.
fn pin(text: &str) -> Result<Pin, CanonicalError> {
    let foreign = || plan_invalid("pins", "PIN_FOREIGN");
    let (id, value) = match text.split_once(':') {
        Some((id, value)) if dimension::is_value(value) => (id, Some(value.to_owned())),
        Some(_) => return Err(foreign()),
        None => (text, None),
    };
    Ok(Pin {
        price_id: Uuid::parse_str(id).map_err(|_| foreign())?,
        dim_value: value,
    })
}

async fn resolution(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    request: ResolveRequest,
) -> Result<Response, CanonicalError> {
    let date = request.date;
    let query = bss_pricing_sdk::read::ResolveQuery {
        catalog: bss_pricing_sdk::read::CatalogRef {
            tenant_id: ctx.subject_tenant_id(),
        },
        revision_id: request.revision,
        date,
        item_id: request.item,
        pins: Vec::new(),
    };
    let stored = load_legacy_resolution(state, scope, ctx, query, &request.pins).await?;
    let body = render(&stored, date, stored.resolved.clone(), &stored.versions)?;
    support::response(StatusCode::OK, &body, None)
}

fn input(resolved: Resolved) -> PricingResolveInputDto {
    PricingResolveInputDto {
        value: resolved.value,
        source: resolved.source.map(Into::into),
    }
}
/// A stored token outside its closed set, in a door that answers `CanonicalError` (D-439).
fn stored_failure(error: RepoError) -> CanonicalError {
    DoorError::from(error).into()
}
/// The response, field by field as slice 07 §6 lists it.
fn render(
    stored: &ReadSnapshot,
    date: Date,
    resolved: Vec<ItemResolution>,
    versions: &BTreeMap<Uuid, SkuVersion>,
) -> Result<PricingResolveDto, CanonicalError> {
    let mut items = Vec::with_capacity(resolved.len());
    for r in resolved {
        let version = versions.get(&r.sku_id);
        let inputs = stored
            .inputs
            .get(&r.item_id)
            .ok_or_else(|| CanonicalError::internal("resolved inputs missing").create())?
            .clone();
        let mut chains = Vec::with_capacity(r.chains.len());
        for chain in r.chains {
            let uncovered = chain.uncovered();
            let binding = chain
                .binding
                .map(|b| {
                    let row = stored.rows.get(&b.price.id).ok_or_else(|| {
                        CanonicalError::internal("a bound price has no stored row").create()
                    })?;
                    Ok::<_, CanonicalError>(PricingResolveBindingDto {
                        price_id: row.id,
                        dim_used: b.dim_used().map(str::to_owned),
                        pinned_from: b.pinned_from,
                        price: row.price_json.clone(),
                        min_fee: row.min_fee.clone(),
                        eligibility: PricingEligibility::stored(
                            &row.eligibility,
                            &format_args!("price {} eligibility", row.id),
                        )
                        .map_err(stored_failure)?,
                        effective_from: row.effective_from.to_string(),
                        effective_to: row.effective_to.map(|d| d.to_string()),
                        temporary_until: row.temporary_until.map(|d| d.to_string()),
                        ends_on: b.ends_on().map(|d| d.to_string()),
                        keep_for_bound: b.keep_for_bound,
                    })
                })
                .transpose()?;
            chains.push(PricingResolveChainDto {
                dim_value: chain.dim_value,
                uncovered,
                binding,
            });
        }
        items.push(PricingResolveItemDto {
            usage_rating_policy: r
                .price_book_entry_id
                .and_then(|id| stored.policies.get(&id).cloned()),
            item_id: r.item_id,
            sku_id: r.sku_id,
            price_book_entry_id: r.price_book_entry_id,
            charge_kind: r.charge_kind.map(Into::into),
            period: r
                .period
                .as_deref()
                .map(|p| PricingPeriod::stored(p, &format_args!("plan item {} period", r.item_id)))
                .transpose()
                .map_err(stored_failure)?,
            model: r.model.map(Into::into),
            sku_version: version.map(|v| PricingResolveSkuVersionDto {
                published_version: v.published_version,
                effective_from: v.effective_from.to_string(),
            }),
            invoice_line_template: input(inputs.invoice_line_template),
            gl_code: input(inputs.gl_code),
            tax_category: input(inputs.tax_category),
            billing_timing: input(inputs.billing_timing),
            meter: PricingResolveMeterDto {
                usage_type_ref: version.and_then(|v| v.content.usage_type_ref.clone()),
                unit: version.and_then(|v| v.content.unit.clone()),
            },
            chains,
        });
    }
    Ok(PricingResolveDto {
        plan_revision_id: stored.revision.id,
        plan_id: stored.revision.plan_id,
        rev_no: stored.revision.rev_no,
        state: PricingResolvedRevisionState::stored(
            stored.state.as_str(),
            &format_args!("revision {} state", stored.revision.id),
        )
        .map_err(stored_failure)?,
        book_id: stored.revision.book_id,
        currency: stored.currency.clone(),
        currency_minor_digits: book::minor_digits(&stored.currency),
        rounding_policy: stored.rounding.clone(),
        date: date.to_string(),
        items,
    })
}

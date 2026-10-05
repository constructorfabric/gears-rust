//! Shared tenant-authorized local snapshots and detached dated Products reads.
use crate::api::rest::authoring::{
    AuthoringState, configuration,
    support::{self, DoorError, authz_failure},
};
use crate::{
    authz,
    domain::{
        dimension,
        plan::RevisionState,
        price::PriceState,
        price_book_entry::ChargeKind,
        resolve::{self, ItemResolution, Pin, ResolveContext, TenantDefaults},
    },
    infra::{
        reference_registry, reference_ticker, reference_work,
        storage::{
            RepoError,
            entity::{plan_revision, price, price_book, price_book_entry},
            repo::{
                book_repo, dimension_repo, plan_item_repo, plan_repo, plan_revision_repo,
                price_book_entry_repo, price_repo,
            },
        },
    },
};
use bss_pricing_sdk::read::ResolveQuery;
use bss_products_sdk::models::SkuVersion;
use std::collections::{BTreeMap, BTreeSet};
use time::Date;
use toolkit::api::canonical_prelude::resource_error;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub struct PriceSnapshot {
    pub row: price::Model,
    pub entry: price_book_entry::Model,
    pub book: price_book::Model,
}
/// What `GET /resolve` refuses: a plan revision, its items, the pins judged against them.
#[resource_error(gts_id!("cf.bss.pricing.plan.v1~"))]
pub struct PlanResource;
/// What `GET /prices/{id}` refuses: a price.
#[resource_error(gts_id!("cf.bss.pricing.price.v1~"))]
pub struct PriceResource;

/// A 400 of `GET /resolve`, with its code.
pub fn plan_invalid(field: &str, code: &str) -> CanonicalError {
    PlanResource::invalid_argument()
        .with_field_violation(field, code, code)
        .create()
}
/// A 404 of `GET /resolve`: the plan revision or item the caller's tenant does not hold.
pub fn plan_missing(what: &str) -> CanonicalError {
    PlanResource::not_found(format!("{what} not found"))
        .with_resource(what)
        .create()
}
/// A 409 of `GET /resolve`, with its code.
pub fn plan_conflict(code: &str) -> CanonicalError {
    PlanResource::aborted(code).with_reason(code).create()
}
/// `plan:read` denied; an unreachable PDP stays 503.
pub fn plan_denied(error: authz::AuthzError) -> CanonicalError {
    match error {
        authz::AuthzError::Denied(d) => PlanResource::permission_denied()
            .with_reason(d.reason)
            .create(),
        unavailable @ authz::AuthzError::Unavailable(_) => authz_failure(unavailable),
    }
}
/// `price:read` denied; an unreachable PDP stays 503.
pub fn price_denied(error: authz::AuthzError) -> CanonicalError {
    match error {
        authz::AuthzError::Denied(d) => PriceResource::permission_denied()
            .with_reason(d.reason)
            .create(),
        unavailable @ authz::AuthzError::Unavailable(_) => authz_failure(unavailable),
    }
}
/// A read transaction's failure: exhausted contention is a `plan` or `price` conflict like every
/// other refusal of the door; anything else as the doors render it.
pub fn read_failure(error: DoorError, conflict: fn(&str) -> CanonicalError) -> CanonicalError {
    match error {
        DoorError::Repo(RepoError::Conflict { code }) => conflict(code),
        other => other.into(),
    }
}
pub fn price_conflict(code: &str) -> CanonicalError {
    PriceResource::aborted(code).with_reason(code).create()
}

/// Everything the transaction reads, as the pure model and the renderer take it.
pub struct ReadSnapshot {
    pub(crate) generation: LocalGeneration,
    pub policies: BTreeMap<Uuid, crate::infra::usage_policy_wire::UsageRatingPolicy>,
    pub revision: plan_revision::Model,
    /// The revision's state as it reads today (D-447): published, superseded or scheduled.
    pub state: RevisionState,
    pub currency: String,
    pub context: ResolveContext,
    /// Every price of the entries the items name, as stored: a binding renders its row.
    pub rows: BTreeMap<Uuid, price::Model>,
    pub defaults: TenantDefaults,
    pub rounding: String,
    pub dimensions: BTreeMap<Uuid, Option<String>>,
    pub resolved: Vec<ItemResolution>,
    pub versions: BTreeMap<Uuid, SkuVersion>,
    pub inputs: BTreeMap<Uuid, resolve::InvoiceInputs>,
}

/// Exact local rows used by a resolve, captured in its serializable snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalGeneration {
    pub plan: crate::infra::storage::entity::plan::Model,
    pub revisions: Vec<plan_revision::Model>,
    items: Vec<crate::infra::storage::entity::plan_item::Model>,
    entries: Vec<price_book_entry::Model>,
    book: price_book::Model,
    dimensions: Vec<crate::infra::storage::entity::dimension_key::Model>,
    settings: serde_json::Value,
}

pub async fn load_price(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<PriceSnapshot, DoorError> {
    // @cpt-begin:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-5
    let row = price_repo::find(tx, scope, tenant, id)
        .await?
        // An approved price, or a cancelled one (D-520), never a `cancel` or `end` row.
        .filter(|p| {
            price_repo::is_price(p)
                && (p.state == PriceState::Approved.as_str()
                    || p.state == PriceState::Cancelled.as_str())
        })
        .ok_or_else(|| {
            PriceResource::not_found("price not found")
                .with_resource("price")
                .create()
        })?;
    let children = AccessScope::for_tenant(tenant);
    let entry = price_book_entry_repo::find(tx, &children, tenant, row.price_book_entry_id)
        .await?
        .ok_or_else(|| corrupt(format!("price {id} has no entry")))?;
    let book = book_repo::find(tx, &children, tenant, entry.book_id)
        .await?
        .ok_or_else(|| corrupt(format!("entry {} has no book", entry.id)))?;
    Ok(PriceSnapshot { row, entry, book })
    // @cpt-end:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-5
}
fn corrupt(what: String) -> DoorError {
    RepoError::CorruptRow(what).into()
}
/// The one read transaction of a resolve. The revision is judged by the state it reads today
/// among its plan's revisions (D-447): a published or superseded one resolves on every date
/// (D-419) — a scheduled one whose date has come reads published, so its answer does not change
/// when the switch is persisted — and a scheduled one still waiting resolves from its sale date
/// on (D-454).
pub async fn read_stored_at(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    item: Option<Uuid>,
    date: Date,
    today: Date,
) -> Result<ReadSnapshot, DoorError> {
    // @cpt-begin:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-2
    let children = AccessScope::for_tenant(tenant);
    let revision = plan_revision_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| plan_missing("plan_revision"))?;
    let plan = plan_repo::find(tx, &children, tenant, revision.plan_id)
        .await?
        .ok_or_else(|| corrupt(format!("revision {id} has no plan")))?;
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-5
    let siblings = plan_revision_repo::for_plan(tx, &children, tenant, revision.plan_id).await?;
    let state = crate::infra::plan_revisions::effective_revisions(&siblings, today)?
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.state)
        .ok_or_else(|| corrupt(format!("revision {id} is not among its plan's")))?;
    match state {
        RevisionState::Published | RevisionState::Superseded => {}
        RevisionState::Scheduled => {
            if revision.available_from.is_none_or(|from| date < from) {
                return Err(plan_conflict("REVISION_NOT_YET_AVAILABLE").into());
            }
        }
        RevisionState::Draft | RevisionState::Pending => {
            return Err(plan_conflict("REVISION_NOT_PUBLISHED").into());
        }
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-5
    let rows = plan_item_repo::for_revision(tx, &children, tenant, revision.id).await?;
    if item.is_some_and(|wanted| !rows.iter().any(|r| r.id == wanted)) {
        return Err(plan_missing("plan_item").into());
    }
    let book = book_repo::find(tx, &children, tenant, revision.book_id)
        .await?
        .ok_or_else(|| corrupt(format!("revision {id} has no book")))?;
    let mut registry = BTreeMap::new();
    let dimension_rows = dimension_repo::list(tx, &children, tenant).await?;
    for d in &dimension_rows {
        let values: Vec<String> = serde_json::from_value(d.values.clone())
            .map_err(|_| corrupt(format!("dimension {} values", d.key)))?;
        registry.insert(d.key.clone(), values);
    }
    let mut entries: BTreeMap<Uuid, resolve::Entry> = BTreeMap::new();
    let mut prices = BTreeMap::new();
    let mut keep_for_bound = BTreeSet::new();
    // The items' entries and all their prices in two statements, whatever the number of items
    // (PS-15).
    let wanted: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.price_book_entry_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let found = price_book_entry_repo::find_many(tx, &children, tenant, &wanted).await?;
    let policies =
        crate::infra::storage::repo::usage_policy_repo::for_entries(tx, tenant, &found).await?;
    let held: BTreeSet<Uuid> = found.iter().map(|e| e.id).collect();
    if let Some(lost) = wanted.iter().find(|w| !held.contains(w)) {
        return Err(corrupt(format!("entry {lost} of revision {id}")));
    }
    // The prices only: a `cancel` or `end` row is never resolved, pinned or bound (D-520, D-521).
    let mut grouped = price_repo::by_entry(
        price_repo::for_entries(tx, &children, tenant, &wanted)
            .await?
            .into_iter()
            .filter(price_repo::is_price)
            .collect(),
    );
    let mut dimensions = BTreeMap::new();
    let entry_rows = found.clone();
    for e in found {
        dimensions.insert(e.id, e.dimension_key.clone());
        let of_entry = grouped.remove(&e.id).unwrap_or_default();
        keep_for_bound.extend(of_entry.iter().filter(|p| p.keep_for_bound).map(|p| p.id));
        let model = price_book_entry_repo::model_of(&e)?;
        let domain = of_entry
            .iter()
            .map(|p| price_repo::to_domain(p, model))
            .collect::<Result<Vec<_>, _>>()?;
        prices.extend(of_entry.into_iter().map(|p| (p.id, p)));
        let values = e
            .dimension_key
            .as_ref()
            .and_then(|key| registry.get(key))
            .cloned()
            .unwrap_or_default();
        let charge_kind: ChargeKind = e
            .charge_kind
            .parse()
            .map_err(|_| corrupt(format!("entry {} charge_kind", e.id)))?;
        entries.insert(
            e.id,
            resolve::Entry {
                id: e.id,
                charge_kind,
                period: e.period,
                model,
                invoice_line_override: e.invoice_line_override,
                values,
                prices: domain,
            },
        );
    }
    let mut items = Vec::with_capacity(rows.len());
    let item_rows = rows.clone();
    for row in rows {
        items.push(resolve::Item {
            id: row.id,
            sku_id: row.sku_id,
            entry: row
                .price_book_entry_id
                .and_then(|entry| entries.get(&entry).cloned()),
        });
    }
    let settings = configuration::settings(tx, &children, tenant).await?;
    let invoice_line_templates = serde_json::from_value(settings.invoice_line_templates.clone())
        .map_err(|_| {
            corrupt(format!(
                "settings of tenant {tenant} invoice_line_templates"
            ))
        })?;
    Ok(ReadSnapshot {
        generation: LocalGeneration {
            plan,
            revisions: siblings,
            items: item_rows,
            entries: entry_rows,
            book: book.clone(),
            dimensions: dimension_rows,
            settings: serde_json::to_value(&settings).map_err(|e| corrupt(e.to_string()))?,
        },
        policies,
        revision,
        state,
        currency: book.currency,
        context: ResolveContext {
            items,
            keep_for_bound,
        },
        rows: prices,
        defaults: TenantDefaults {
            default_timing: settings.default_timing.as_str().to_owned(),
            default_gl: settings.default_gl,
            default_tax_category: settings.default_tax_category,
            invoice_line_templates,
        },
        rounding: settings.default_rounding,
        dimensions,
        resolved: Vec::new(),
        versions: BTreeMap::new(),
        inputs: BTreeMap::new(),
    })
    // @cpt-end:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-2
}

/// D-421: each SKU version as of `date`, one read per distinct SKU, through the detached
/// registry as pricing's system actor for `tenant` (D-424). REST resolve and
/// `PricingReadV1::resolve` both call this, and only after the caller passed `plan:read` and
/// the revision was found in the caller's tenant. The caller's context is not forwarded:
/// Products trusts this actor, not the consumer. A SKU Products does not know (404) has no version.
/// # Errors
/// Any other definite refusal as Products gave it; 503 `REGISTRY_UNAVAILABLE` when Products
/// cannot answer.
async fn versions_as_of(
    hub: &toolkit::ClientHub,
    tenant: Uuid,
    skus: impl IntoIterator<Item = Uuid>,
    date: Date,
) -> Result<BTreeMap<Uuid, SkuVersion>, CanonicalError> {
    let wanted: BTreeSet<Uuid> = skus.into_iter().collect();
    let mut found = BTreeMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    let registry =
        reference_registry::resolve(hub).map_err(|e| support::registry_unavailable(&e))?;
    let actor = reference_ticker::system_actor(tenant)?;
    for sku in wanted {
        match registry.sku_version_as_of(&actor, tenant, sku, date).await {
            Ok(Some(version)) => {
                found.insert(sku, version);
            }
            Ok(None) => {}
            Err(error) if error.status_code() == 404 => {}
            Err(error) if reference_work::definite_refusal(&error) => return Err(error),
            Err(error) => return Err(support::registry_unavailable(&error)),
        }
    }
    Ok(found)
}

/// Resolve SDK pins with explicit item and requested dimension identity.
pub async fn load_resolution(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    query: ResolveQuery,
) -> Result<ReadSnapshot, CanonicalError> {
    let mut stored = snapshot(
        state,
        scope,
        query.catalog.tenant_id,
        query.revision_id,
        query.item_id,
        query.date,
    )
    .await?;
    if query.pins.len() > resolve::MAX_PINS {
        return Err(plan_invalid("pins", "PINS_TOO_MANY"));
    }
    let mut pins = Vec::new();
    let mut seen = BTreeSet::new();
    for pin in query.pins {
        let foreign = || plan_invalid("pins", "PIN_FOREIGN");
        let item = stored
            .context
            .items
            .iter()
            .find(|i| i.id == pin.item_id)
            .ok_or_else(foreign)?;
        let row = stored.rows.get(&pin.price_id).ok_or_else(foreign)?;
        if item
            .entry
            .as_ref()
            .is_none_or(|e| e.id != row.price_book_entry_id)
            || row.state != crate::domain::price::PriceState::Approved.as_str()
            || pin
                .dimension_value
                .as_ref()
                .is_some_and(|v| !dimension::is_value(v))
        {
            return Err(foreign());
        }
        if !seen.insert((pin.item_id, pin.dimension_value.clone())) {
            return Err(plan_invalid("pins", "PIN_DUPLICATE"));
        }
        let dim_value = if row.dim_value == pin.dimension_value {
            None
        } else if row.dim_value.is_none() {
            pin.dimension_value
        } else {
            return Err(foreign());
        };
        pins.push(Pin {
            price_id: pin.price_id,
            dim_value,
        });
    }
    finish(
        state,
        &mut stored,
        ctx,
        query.catalog.tenant_id,
        query.date,
        query.item_id,
        &pins,
    )
    .await?;
    Ok(stored)
}
/// REST's historical pin syntax is interpreted by the existing domain resolver unchanged.
pub async fn load_legacy_resolution(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: &SecurityContext,
    query: ResolveQuery,
    pins: &[Pin],
) -> Result<ReadSnapshot, CanonicalError> {
    let mut stored = snapshot(
        state,
        scope,
        query.catalog.tenant_id,
        query.revision_id,
        query.item_id,
        query.date,
    )
    .await?;
    finish(
        state,
        &mut stored,
        ctx,
        query.catalog.tenant_id,
        query.date,
        query.item_id,
        pins,
    )
    .await?;
    Ok(stored)
}
async fn snapshot(
    state: &AuthoringState,
    scope: AccessScope,
    tenant: Uuid,
    revision: Uuid,
    item: Option<Uuid>,
    date: Date,
) -> Result<ReadSnapshot, CanonicalError> {
    support::transaction_door(&state.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            read_stored_at(
                tx,
                &scope,
                tenant,
                revision,
                item,
                date,
                time::OffsetDateTime::now_utc().date(),
            )
            .await
        })
    })
    .await
    .map_err(|e| read_failure(e, plan_conflict))
}
/// Finish a snapshot: the matrix, then each SKU version as pricing's system actor (D-424).
/// `ctx` is the caller who already passed `plan:read`. It is not the Products subject.
pub async fn finish(
    state: &AuthoringState,
    stored: &mut ReadSnapshot,
    _ctx: &SecurityContext,
    tenant: Uuid,
    date: Date,
    item: Option<Uuid>,
    pins: &[Pin],
) -> Result<(), CanonicalError> {
    stored.resolved = resolve::matrix(&stored.context, date, pins)
        .map_err(|e| plan_invalid("pins", e.code))?
        .into_iter()
        .filter(|r| item.is_none_or(|id| r.item_id == id))
        .collect();
    // @cpt-begin:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-4
    stored.versions = versions_as_of(
        &state.hub,
        tenant,
        stored.resolved.iter().map(|r| r.sku_id),
        date,
    )
    .await?;
    stored.inputs = stored
        .resolved
        .iter()
        .map(|item| {
            let entry_override = stored
                .context
                .items
                .iter()
                .find(|i| i.id == item.item_id)
                .and_then(|i| i.entry.as_ref())
                .and_then(|e| e.invoice_line_override.as_deref());
            (
                item.item_id,
                resolve::invoice_inputs(
                    entry_override,
                    stored.versions.get(&item.sku_id),
                    &stored.defaults,
                    item.charge_kind,
                ),
            )
        })
        .collect();
    // @cpt-end:cpt-cf-bss-pricing-flow-read-contract-events:p1:inst-read-contract-events-flow-4
    Ok(())
}

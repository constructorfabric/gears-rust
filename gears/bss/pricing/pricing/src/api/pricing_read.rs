//! Authorized in-process pricing reads over the same snapshots as REST.
use crate::{
    api::rest::authoring::{
        AuthoringState,
        support::{self, DoorError},
    },
    authz::{self, OwnerTenant, ResourceRef, actions, resource_types},
    domain::{
        book,
        money::{self, PriceData},
        price_book_entry::Model,
        resolve::Source,
    },
    infra::{
        plan_revisions,
        pricing_reads::{
            self, PriceSnapshot, ReadSnapshot, plan_conflict, plan_missing, read_failure,
        },
        storage::repo::{plan_repo, plan_revision_repo},
    },
};
use authz_resolver_sdk::{PolicyEnforcer, pep::ResourceType};
use bss_pricing_sdk::{
    digest::{money_digest, template_digest},
    read::{
        AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, IncompleteCommercialInputs,
        PlanQuery, PriceModel, PriceQuery, PriceState, PricingReadV1, ResolveQuery,
        ResolvedBindings, ResolvedCell, RevisionRef, Tier,
    },
    terms::{BillingCycle, BillingTiming, InputSource, InvoiceInputs, Rounding},
};
use rust_decimal::Decimal;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Real pricing read provider; authorization is evaluated for every invocation.
pub struct PricingReadProvider {
    state: Arc<AuthoringState>,
    enforcer: Arc<PolicyEnforcer>,
}
impl PricingReadProvider {
    /// Attach the shared runtime and policy enforcer.
    #[must_use]
    pub fn new(state: Arc<AuthoringState>, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { state, enforcer }
    }
    pub(crate) async fn scope(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        resource: &ResourceType,
        id: Uuid,
    ) -> Result<AccessScope, CanonicalError> {
        if !crate::api::rest::authoring::support::authenticated(ctx) {
            return Err(CanonicalError::unauthenticated()
                .with_reason("AUTHENTICATION_REQUIRED")
                .create());
        }
        // The explicit catalog tenant is a resource hint, never authority. The shared gate
        // verifies it is contained in the compiled PDP constraints before any storage access.
        authz::access_scope(
            &self.enforcer,
            ctx,
            resource,
            actions::READ,
            Some(OwnerTenant(tenant)),
            Some(ResourceRef(id)),
        )
        .await
        .map_err(|e| {
            if resource.name() == resource_types::PRICE.name() {
                pricing_reads::price_denied(e)
            } else {
                pricing_reads::plan_denied(e)
            }
        })
    }
}
#[async_trait::async_trait]
impl PricingReadV1 for PricingReadProvider {
    async fn price(
        &self,
        ctx: &SecurityContext,
        query: PriceQuery,
    ) -> Result<ImmutablePrice, CanonicalError> {
        let scope = self
            .scope(
                ctx,
                query.catalog.tenant_id,
                &resource_types::PRICE,
                query.price_id,
            )
            .await?;
        let snapshot = support::transaction_door(&self.state.db.db(), move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                pricing_reads::load_price(tx, &scope, query.catalog.tenant_id, query.price_id).await
            })
        })
        .await
        .map_err(|e| read_failure(e, pricing_reads::price_conflict))?;
        project_price(&snapshot)
    }
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        query: ResolveQuery,
    ) -> Result<ResolvedBindings, CanonicalError> {
        let scope = self
            .scope(
                ctx,
                query.catalog.tenant_id,
                &resource_types::PLAN,
                query.revision_id,
            )
            .await?;
        project_resolution(&pricing_reads::load_resolution(&self.state, scope, ctx, query).await?)
    }
    async fn current_revision(
        &self,
        ctx: &SecurityContext,
        query: PlanQuery,
    ) -> Result<RevisionRef, CanonicalError> {
        let tenant = query.catalog.tenant_id;
        let scope = self
            .scope(ctx, tenant, &resource_types::PLAN, query.plan_id)
            .await?;
        let now = crate::infra::storage::stored_now();
        let correlation = Uuid::now_v7();
        support::transaction_with_events(
            &self.state.db.db(),
            &self.state.outbox,
            move |tx, outbox| {
                let scope = scope.clone();
                Box::pin(async move {
                    plan_repo::find(tx, &scope, tenant, query.plan_id)
                        .await?
                        .ok_or_else(|| plan_missing("plan"))?;
                    plan_revisions::catch_up(tx, &outbox, tenant, query.plan_id, now, correlation)
                        .await?;
                    let plan = plan_repo::find(tx, &scope, tenant, query.plan_id)
                        .await?
                        .ok_or_else(|| plan_missing("plan"))?;
                    // Related revisions are read only after the plan passed the PDP-derived scope.
                    let children = AccessScope::for_tenant(tenant);
                    let revision = plan_revision_repo::for_plan(tx, &children, tenant, plan.id)
                        .await?
                        .into_iter()
                        .find(|r| Some(r.rev_no) == plan.published_rev && r.state == "published")
                        .ok_or_else(|| plan_conflict("PLAN_UNPUBLISHED"))?;
                    Ok::<_, DoorError>(RevisionRef {
                        plan_id: plan.id,
                        revision_id: revision.id,
                        revision_no: revision.rev_no,
                    })
                })
            },
        )
        .await
    }
}
fn corrupt(detail: impl Into<String>) -> CanonicalError {
    let detail = detail.into();
    tracing::error!(%detail, "corrupt stored pricing row");
    CanonicalError::internal(detail).create()
}
fn amount(price_id: Uuid, value: &str) -> Result<Decimal, CanonicalError> {
    Decimal::from_str_exact(value)
        .ok()
        .filter(|v| *v >= Decimal::ZERO)
        .ok_or_else(|| corrupt(format!("price {price_id}: invalid stored amount {value}")))
}
fn price_model(
    price_id: Uuid,
    model: Model,
    value: serde_json::Value,
) -> Result<PriceModel, CanonicalError> {
    let data = money::decode(model, value)
        .map_err(|e| corrupt(format!("price {price_id}: {}", e.code)))?;
    if let Some(e) = money::validate(model, &data).first() {
        return Err(corrupt(format!("price {price_id}: {}", e.code)));
    }
    Ok(match data {
        PriceData::Flat { amount } => PriceModel::Flat { amount },
        PriceData::PerUnit { rate } => PriceModel::PerUnit { unit_amount: rate },
        PriceData::Package {
            package_size,
            package_price,
        } => PriceModel::Package {
            package_size,
            package_price,
        },
        PriceData::Tiers { tiers } => {
            let tiers = tiers
                .into_iter()
                .map(|t| Tier {
                    up_to: t.up_to,
                    rate: t.rate,
                })
                .collect();
            if model == Model::Volume {
                PriceModel::Volume { tiers }
            } else {
                PriceModel::Graduated { tiers }
            }
        }
    })
}
fn immutable(
    row: &crate::infra::storage::entity::price::Model,
    model: Model,
    currency: &str,
) -> Result<ImmutablePrice, CanonicalError> {
    let mut p = ImmutablePrice {
        price_id: row.id,
        price_book_entry_id: row.price_book_entry_id,
        money_digest: [0; 32],
        currency: currency.into(),
        model: price_model(row.id, model, row.price_json.clone())?,
        minimum_fee: row
            .min_fee
            .as_deref()
            .map(|value| amount(row.id, value))
            .transpose()?,
        effective_from: row.effective_from,
        ends_on: row
            .temporary_until
            .or_else(|| row.closed_explicitly.then_some(row.effective_to).flatten()),
        state: price_state(row)?,
    };
    p.money_digest = money_digest(&p);
    Ok(p)
}
/// The stored state of a price the read serves: approved, or cancelled (D-520). The reads load no
/// other state, so another one here is a corrupt row.
fn price_state(
    row: &crate::infra::storage::entity::price::Model,
) -> Result<PriceState, CanonicalError> {
    use crate::domain::price::PriceState as Stored;
    match row.state.parse::<Stored>() {
        Ok(Stored::Approved) => Ok(PriceState::Approved),
        Ok(Stored::Cancelled) => Ok(PriceState::Cancelled),
        // Written out, so a state added to the domain is a compile error here.
        Ok(Stored::Draft | Stored::Pending | Stored::Rejected) | Err(_) => Err(corrupt(format!(
            "price {}: state {} is not served",
            row.id, row.state
        ))),
    }
}
fn project_price(s: &PriceSnapshot) -> Result<ImmutablePrice, CanonicalError> {
    let model = s.entry.model.parse().map_err(|_| {
        corrupt(format!(
            "entry {} price {}: invalid stored price model {}",
            s.entry.id, s.row.id, s.entry.model
        ))
    })?;
    immutable(&s.row, model, &s.book.currency)
}
fn incomplete(field: &'static str) -> CanonicalError {
    IncompleteCommercialInputs { field }.into()
}
fn required(value: Option<&str>, field: &'static str) -> Result<String, CanonicalError> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| incomplete(field))
}
pub(crate) fn project_resolution(s: &ReadSnapshot) -> Result<ResolvedBindings, CanonicalError> {
    let mut cells = Vec::new();
    for r in &s.resolved {
        for chain in &r.chains {
            let selection = BindingSelection {
                item_id: r.item_id,
                dimension_value: chain.dim_value.clone(),
            };
            let binding = chain
                .binding
                .as_ref()
                .map(|b| {
                    let version = s
                        .versions
                        .get(&r.sku_id)
                        .ok_or_else(|| incomplete("sku_version"))?;
                    if version.published_version <= 0 {
                        return Err(incomplete("sku_version"));
                    }
                    let entry = s
                        .context
                        .items
                        .iter()
                        .find(|i| i.id == r.item_id)
                        .and_then(|i| i.entry.as_ref())
                        .ok_or_else(|| incomplete("entry"))?;
                    let inputs = s.inputs.get(&r.item_id).ok_or_else(|| {
                        corrupt(format!("item {}: resolved inputs missing", r.item_id))
                    })?;
                    let template = required(
                        inputs.invoice_line_template.value.as_deref(),
                        "invoice_template",
                    )?;
                    let template_source = match inputs
                        .invoice_line_template
                        .source
                        .ok_or_else(|| incomplete("template_source"))?
                    {
                        Source::Entry => InputSource::Entry,
                        Source::Sku => InputSource::SkuVersion,
                        Source::Tenant => InputSource::SellerSettings,
                    };
                    let timing = match inputs.billing_timing.value.as_deref() {
                        Some("advance") => BillingTiming::Advance,
                        Some("arrears") => BillingTiming::Arrears,
                        _ => return Err(incomplete("billing_timing")),
                    };
                    if s.rounding != "half_even" {
                        return Err(incomplete("rounding"));
                    }
                    let kind = match entry.charge_kind {
                        crate::domain::price_book_entry::ChargeKind::Recurring => {
                            ChargeKind::Recurring
                        }
                        crate::domain::price_book_entry::ChargeKind::Usage => ChargeKind::Usage,
                        crate::domain::price_book_entry::ChargeKind::OneTime => ChargeKind::OneTime,
                    };
                    if kind == ChargeKind::Usage {
                        required(version.content.unit.as_deref(), "unit")?;
                    }
                    let recurring_period = match entry.period.as_deref() {
                        None => None,
                        Some("month") => Some(BillingCycle::Month),
                        Some("year") => Some(BillingCycle::Year),
                        _ => return Err(incomplete("recurring_period")),
                    };
                    let row = s.rows.get(&b.price.id).ok_or_else(|| {
                        corrupt(format!("price {}: bound price missing", b.price.id))
                    })?;
                    let price = immutable(row, entry.model, &s.currency)?;
                    if price.price_book_entry_id != entry.id {
                        return Err(corrupt(format!(
                            "price {} entry {}: bound price entry mismatch",
                            b.price.id, entry.id
                        )));
                    }
                    Ok(AcceptedBinding {
                        item_id: r.item_id,
                        price_book_entry_id: entry.id,
                        dimension_key: s.dimensions.get(&entry.id).cloned().flatten(),
                        dimension_value: chain.dim_value.clone(),
                        sku_id: r.sku_id,
                        sku_version: version.published_version,
                        sku_code: required(Some(&version.content.code), "sku_code")?,
                        sku_name: required(Some(&version.content.name), "sku_name")?,
                        unit: version.content.unit.clone(),
                        meter: match kind {
                            ChargeKind::Usage => version
                                .content
                                .usage_type_ref
                                .as_deref()
                                .map(crate::domain::usage_policy::meter_ref),
                            _ => None,
                        },
                        price,
                        kind,
                        recurring_period,
                        via_default: chain.dim_value.is_some() && b.dim_used().is_none(),
                        usage_rating_policy: s
                            .policies
                            .get(&entry.id)
                            .map(crate::infra::usage_policy_wire::UsageRatingPolicy::typed)
                            .transpose()
                            .map_err(DoorError::from)?,
                        invoice: InvoiceInputs {
                            template_digest: template_digest(&template),
                            template,
                            template_source,
                            gl_code: required(inputs.gl_code.value.as_deref(), "gl_code")?,
                            tax_category: required(
                                inputs.tax_category.value.as_deref(),
                                "tax_category",
                            )?,
                            timing,
                            currency_scale: book::minor_digits(&s.currency),
                            rounding: Rounding::HalfEven,
                        },
                    })
                })
                .transpose()?;
            cells.push(ResolvedCell { selection, binding });
        }
    }
    Ok(ResolvedBindings {
        plan_id: s.revision.plan_id,
        revision_id: s.revision.id,
        cells,
    })
}

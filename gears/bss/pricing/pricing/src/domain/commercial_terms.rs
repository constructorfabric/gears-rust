//! Pure supported-sale profile. The caller supplies independently resolved selected bindings.
use super::{RuleError, book, money, usage_policy};
use bss_pricing_sdk::{
    acceptance::{CommercialReason as R, NewSaleQuery, Term},
    digest::{billing_terms_digest, money_digest, policy_digest, template_digest},
    read::{AcceptedBinding, ChargeKind, PriceModel},
    terms::{BillingAnchor, BillingCycle, RatingWindow, Rounding, TermsSource},
};
use rust_decimal::Decimal;
use std::collections::BTreeSet;
use time::{Month, Time, UtcOffset};

/// Verified live facts, constructed inside Pricing, never accepted from an SDK caller.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each verified fact is an independent seam input"
)]
pub struct SaleObservation {
    pub revision_is_current: bool,
    pub revision_available: bool,
    pub sku_active: bool,
    pub sku_sellable: bool,
    pub covered: bool,
}
/// Validate live new-sale eligibility; provider errors never become these booleans.
/// # Errors
/// `NotSellable` for ineligible revision/SKU facts, `ResolutionChanged` for coverage loss.
pub fn validate_new_sale_observation(o: &SaleObservation) -> Result<(), RuleError> {
    if !o.revision_is_current || !o.revision_available || !o.sku_active || !o.sku_sellable {
        return Err(R::NotSellable.into());
    }
    if !o.covered {
        return Err(R::ResolutionChanged.into());
    }
    Ok(())
}
/// Validate shape, integrity and per-entry compatibility without choosing or rewriting terms.
/// `bindings` must contain one selected cell for every revision item; Task 5's live resolver
/// establishes that item universe and independently compares the caller's resolution digest.
/// # Errors
/// Stable typed commercial reasons identify the first violated invariant.
pub fn validate_commercial_terms(
    q: &NewSaleQuery,
    bindings: &[AcceptedBinding],
) -> Result<(), RuleError> {
    if q.quantity <= Decimal::ZERO {
        return Err(R::InvalidQuantity.into());
    }
    if matches!(q.term, Term::FixedPeriods { count: 0 }) {
        return Err(R::InvalidTermCount.into());
    }
    if q.order_version == 0
        || q.hold_policy_version == 0
        || [
            q.order_id,
            q.line_id,
            q.plan_id,
            q.plan_revision_id,
            q.tenant_axes.seller_tenant_id,
            q.tenant_axes.payer_tenant_id,
            q.tenant_axes.resource_tenant_id,
        ]
        .iter()
        .any(uuid::Uuid::is_nil)
    {
        return Err(R::UnsupportedTerms.into());
    }
    validate_billing_terms(q)?;
    validate_selections(q, bindings)?;
    if !book::currency_code(&q.market.currency) {
        return Err(R::CurrencyMismatch.into());
    }
    if q.market
        .region
        .as_ref()
        .is_some_and(|r| r.trim().is_empty())
    {
        return Err(R::MarketMismatch.into());
    }
    for b in bindings {
        validate_binding(q, b)?;
    }
    Ok(())
}
fn validate_billing_terms(q: &NewSaleQuery) -> Result<(), RuleError> {
    let t = &q.billing_terms;
    if t.schema_version != 1
        || matches!(t.source, TermsSource::SellerPolicy { id, version } if id.is_nil() || version == 0)
    {
        return Err(R::UnsupportedTerms.into());
    }
    let at = t.anchor_at.to_offset(UtcOffset::UTC);
    if t.anchor == BillingAnchor::Calendar
        && (at.time() != Time::MIDNIGHT
            || at.day() != 1
            || (t.cycle == BillingCycle::Year && at.month() != Month::January))
    {
        return Err(R::UnalignedBillingAnchor.into());
    }
    if billing_terms_digest(t) != t.digest {
        return Err(R::BillingTermsDigestMismatch.into());
    }
    Ok(())
}
fn validate_selections(q: &NewSaleQuery, bindings: &[AcceptedBinding]) -> Result<(), RuleError> {
    let mut selected = BTreeSet::new();
    if bindings.is_empty()
        || q.selections.len() != bindings.len()
        || q.selections
            .iter()
            .any(|s| s.item_id.is_nil() || !selected.insert(s.item_id))
    {
        return Err(R::IncompleteSelection.into());
    }
    let mut bound = BTreeSet::new();
    for b in bindings {
        if !bound.insert(b.item_id) || !selected.contains(&b.item_id) {
            return Err(R::IncompleteSelection.into());
        }
        let selection = q
            .selections
            .iter()
            .find(|s| s.item_id == b.item_id)
            .ok_or_else(|| RuleError::from(R::IncompleteSelection))?;
        if selection.dimension_value != b.dimension_value {
            return Err(R::DimensionMismatch.into());
        }
    }
    Ok(())
}
fn validate_binding(q: &NewSaleQuery, b: &AcceptedBinding) -> Result<(), RuleError> {
    if b.price_book_entry_id.is_nil() || b.price_book_entry_id != b.price.price_book_entry_id {
        return Err(R::BindingEntryMismatch.into());
    }
    if b.price.currency != q.market.currency {
        return Err(R::CurrencyMismatch.into());
    }
    if b.dimension_key
        .as_ref()
        .is_some_and(|v| v.trim().is_empty())
        || b.dimension_value
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        || matches!((&b.dimension_key, &b.dimension_value), (None, Some(_)))
        || (b.via_default && b.dimension_value.is_none())
    {
        return Err(R::DimensionMismatch.into());
    }
    if b.dimension_key.as_deref() == Some("region") && b.dimension_value != q.market.region {
        return Err(R::MarketMismatch.into());
    }
    validate_model_and_policy(q, b)?;
    validate_money(b)?;
    validate_invoice(b)?;
    Ok(())
}
fn validate_model_and_policy(q: &NewSaleQuery, b: &AcceptedBinding) -> Result<(), RuleError> {
    let scalar = matches!(
        b.price.model,
        PriceModel::Flat { .. } | PriceModel::PerUnit { .. }
    );
    match b.kind {
        ChargeKind::Recurring | ChargeKind::OneTime => {
            if !scalar {
                return Err(R::UnsupportedModel.into());
            }
            if b.usage_rating_policy.is_some() {
                return Err(R::UnsupportedTerms.into());
            }
            if b.kind == ChargeKind::Recurring {
                if b.recurring_period.as_ref() != Some(&q.billing_terms.cycle) {
                    return Err(R::BillingCycleMismatch.into());
                }
            } else if b.recurring_period.is_some() {
                return Err(R::UnsupportedTerms.into());
            }
        }
        ChargeKind::Usage => {
            if !matches!(
                b.price.model,
                PriceModel::PerUnit { .. }
                    | PriceModel::Volume { .. }
                    | PriceModel::Graduated { .. }
            ) {
                return Err(R::UnsupportedModel.into());
            }
            if b.recurring_period.is_some() {
                return Err(R::UnsupportedTerms.into());
            }
            let p = b
                .usage_rating_policy
                .as_ref()
                .ok_or_else(|| RuleError::from(R::MissingRatingPolicy))?;
            usage_policy::validate_policy_shape(&p.content)
                .map_err(|_| RuleError::from(R::MeterPolicyMismatch))?;
            if p.policy_id.is_nil()
                || p.version == 0
                || policy_digest(&p.content) != p.digest
                || b.unit.as_deref().is_none_or(|unit| unit.trim().is_empty())
            {
                return Err(R::MeterPolicyMismatch.into());
            }
            let hourly = matches!(p.content.rating_window, RatingWindow::CalendarHour { .. });
            if b.price.minimum_fee.is_some() && usage_policy::refuses_minimum_fee(&p.content) {
                return Err(R::UnsupportedTerms.into());
            }
            let at = q.billing_terms.anchor_at.to_offset(UtcOffset::UTC);
            if hourly
                && q.billing_terms.anchor == BillingAnchor::SubscriptionStart
                && (at.minute() != 0 || at.second() != 0 || at.nanosecond() != 0)
            {
                return Err(R::UnalignedBillingAnchor.into());
            }
        }
    }
    Ok(())
}
fn validate_money(b: &AcceptedBinding) -> Result<(), RuleError> {
    match &b.price.model {
        PriceModel::Flat { amount }
        | PriceModel::PerUnit {
            unit_amount: amount,
        } if *amount < Decimal::ZERO => return Err(R::InvalidMoney.into()),
        PriceModel::Volume { tiers } | PriceModel::Graduated { tiers } => {
            let tiers: Vec<_> = tiers
                .iter()
                .map(|t| money::Tier {
                    up_to: t.up_to,
                    rate: t.rate,
                })
                .collect();
            if !money::validate_tiers(&tiers).is_empty() {
                return Err(R::InvalidTiers.into());
            }
        }
        _ => {}
    }
    if b.price.minimum_fee.is_some_and(|v| v < Decimal::ZERO) {
        return Err(R::InvalidMoney.into());
    }
    if money_digest(&b.price) != b.price.money_digest {
        return Err(R::MoneyDigestMismatch.into());
    }
    Ok(())
}
fn validate_invoice(b: &AcceptedBinding) -> Result<(), RuleError> {
    if b.sku_id.is_nil()
        || b.price.price_id.is_nil()
        || b.sku_version <= 0
        || [
            &b.sku_code,
            &b.sku_name,
            &b.invoice.template,
            &b.invoice.gl_code,
            &b.invoice.tax_category,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        || (matches!(b.price.model, PriceModel::PerUnit { .. })
            && b.unit.as_ref().is_none_or(|u| u.trim().is_empty()))
    {
        return Err(R::IncompleteCommercialInputs.into());
    }
    if b.invoice.currency_scale != book::minor_digits(&b.price.currency)
        || b.invoice.rounding != Rounding::HalfEven
    {
        return Err(R::UnsupportedTerms.into());
    }
    if template_digest(&b.invoice.template) != b.invoice.template_digest {
        return Err(R::TemplateDigestMismatch.into());
    }
    Ok(())
}

/// Server-time expiry is half-open and independent of requested activation time.
/// # Errors
/// `HOLD_EXPIRED` at and after the persisted acceptance deadline.
pub fn validate_hold_time(
    now: time::OffsetDateTime,
    hold_until: time::OffsetDateTime,
) -> Result<(), RuleError> {
    if now >= hold_until {
        return Err(R::HoldExpired.into());
    }
    Ok(())
}
/// A first hold may choose any instant inside the accepted activation window.
/// # Errors
/// `ACTIVATION_OUTSIDE_ACCEPTED_WINDOW` before start or at/after the deadline.
pub fn validate_activation_window(
    start_at: time::OffsetDateTime,
    activation_at: time::OffsetDateTime,
    hold_until: time::OffsetDateTime,
) -> Result<(), RuleError> {
    if activation_at < start_at || activation_at >= hold_until {
        return Err(R::ActivationOutsideAcceptedWindow.into());
    }
    Ok(())
}

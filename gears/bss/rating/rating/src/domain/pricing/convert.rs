//! Pricing's `AcceptedBinding` and the core's `CoreBinding`, one into the other (DESIGN 11 §4.2).
//!
//! The core's types are the serde mirror Rating stores a binding as (`bss_rating__pricing_binding`)
//! and evaluates it with; Pricing's types are what `resolve` answers and what its digest
//! (`selected_bindings_digest`) is computed over. Every field maps one to one, so a binding that
//! goes through both conversions comes back equal, which the tests hold.

use bss_pricing_sdk::Digest;
use bss_pricing_sdk::meter_semantics::MeterRef as PricingMeterRef;
use bss_pricing_sdk::read::{
    AcceptedBinding, BindingSelection, ChargeKind as PricingChargeKind, ImmutablePrice, PriceModel,
    PriceState as PricingPriceState, ResolvedCell, Tier,
};
use bss_pricing_sdk::terms::{
    AggregationScope, BillingCycle, BillingTiming as PricingTiming, Fold,
    InputSource as PricingSource, InvoiceInputs, PartialWindow, RatingWindow, Reset, Rounding,
    Timezone, UsageRatingPolicy, UsageRatingPolicyInput,
};
use bss_rating_core::engine::current::exact::CoreCurrency;
use bss_rating_core::engine::current::input::{
    BillingTiming, CellSelection, ChargeKind, CoreBinding, CoreCell, CoreInvoiceInputs, CorePrice,
    CorePriceModel, CoreTier, CoreUsagePolicy, InputSource, MeterRef, PartialWindowPolicy,
    PolicyScope, PriceState, RatingWindowKind, RecurringPeriod, ResetPolicy, StoredRounding,
    WindowFold,
};

use toolkit_macros::domain_model;

use crate::domain::digest::Sha256Hex;

/// Why a binding does not convert: a digest that is not 64 lowercase hex characters, a currency
/// the money crate refuses, or a price state the core does not know.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {detail}")]
pub struct ConvertError {
    /// The field that does not convert.
    pub field: &'static str,
    /// What is wrong with it.
    pub detail: String,
}

impl ConvertError {
    fn new(field: &'static str, detail: impl Into<String>) -> Self {
        Self {
            field,
            detail: detail.into(),
        }
    }
}

/// The 32 bytes of a hex digest.
///
/// # Errors
///
/// [`ConvertError`] unless `hex` is 64 lowercase hex characters.
pub fn digest_bytes(field: &'static str, hex: &str) -> Result<Digest, ConvertError> {
    let parsed = Sha256Hex::parse(hex).map_err(|e| ConvertError::new(field, e.to_string()))?;
    let mut out = [0_u8; 32];
    for (slot, pair) in out.iter_mut().zip(parsed.as_str().as_bytes().chunks(2)) {
        let text =
            std::str::from_utf8(pair).map_err(|e| ConvertError::new(field, e.to_string()))?;
        *slot =
            u8::from_str_radix(text, 16).map_err(|e| ConvertError::new(field, e.to_string()))?;
    }
    Ok(out)
}

/// A digest as 64 lowercase hex characters.
#[must_use]
pub fn digest_hex(digest: &Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Pricing's binding as the core reads it.
#[must_use]
pub fn to_core(binding: &AcceptedBinding) -> CoreBinding {
    CoreBinding {
        item_id: binding.item_id,
        price_book_entry_id: binding.price_book_entry_id,
        dimension_key: binding.dimension_key.clone(),
        dimension_value: binding.dimension_value.clone(),
        sku_id: binding.sku_id,
        sku_version: binding.sku_version,
        sku_code: binding.sku_code.clone(),
        sku_name: binding.sku_name.clone(),
        unit: binding.unit.clone(),
        meter: binding.meter.as_ref().map(|meter| MeterRef {
            usage_type_id: meter.usage_type_id.clone(),
            version: meter.version.clone(),
        }),
        price: core_price(&binding.price),
        kind: match binding.kind {
            PricingChargeKind::Recurring => ChargeKind::Recurring,
            PricingChargeKind::Usage => ChargeKind::Usage,
            PricingChargeKind::OneTime => ChargeKind::OneTime,
        },
        recurring_period: binding.recurring_period.map(|cycle| match cycle {
            BillingCycle::Month => RecurringPeriod::Month,
            BillingCycle::Year => RecurringPeriod::Year,
        }),
        via_default: binding.via_default,
        usage_policy: binding.usage_rating_policy.as_ref().map(core_policy),
        invoice: core_invoice(&binding.invoice),
    }
}

fn core_price(price: &ImmutablePrice) -> CorePrice {
    CorePrice {
        price_id: price.price_id,
        price_book_entry_id: price.price_book_entry_id,
        money_digest: digest_hex(&price.money_digest),
        currency: CoreCurrency {
            code: price.currency.code().to_owned(),
            scale: price.currency.scale(),
        },
        model: match &price.model {
            PriceModel::Flat { amount } => CorePriceModel::Flat { amount: *amount },
            PriceModel::PerUnit { unit_amount } => CorePriceModel::PerUnit {
                unit_amount: *unit_amount,
            },
            PriceModel::Volume { tiers } => CorePriceModel::Volume {
                tiers: tiers.iter().map(core_tier).collect(),
            },
            PriceModel::Graduated { tiers } => CorePriceModel::Graduated {
                tiers: tiers.iter().map(core_tier).collect(),
            },
            PriceModel::Package {
                package_size,
                package_price,
            } => CorePriceModel::Package {
                package_size: *package_size,
                package_price: *package_price,
            },
        },
        minimum_fee: price.minimum_fee,
        effective_from: price.effective_from,
        ends_on: price.ends_on,
        state: match price.state {
            PricingPriceState::Cancelled => PriceState::Cancelled,
            // A binding's price is approved (pricing D-520); a state added later reads as the
            // approved price it was served as, and the core's checks decide.
            _ => PriceState::Approved,
        },
    }
}

const fn core_tier(tier: &Tier) -> CoreTier {
    CoreTier {
        up_to: tier.up_to,
        rate: tier.rate,
    }
}

fn core_policy(policy: &UsageRatingPolicy) -> CoreUsagePolicy {
    let content = &policy.content;
    CoreUsagePolicy {
        policy_id: policy.policy_id,
        version: policy.version,
        digest: digest_hex(&policy.digest),
        rating_window: match content.rating_window {
            RatingWindow::BillingCycle => RatingWindowKind::BillingCycle,
            RatingWindow::CalendarHour {
                timezone: Timezone::Utc,
            } => RatingWindowKind::CalendarHourUtc,
        },
        aggregation_scope: match content.aggregation_scope {
            AggregationScope::SubscriptionLine => PolicyScope::SubscriptionLine,
            AggregationScope::Resource => PolicyScope::Resource,
        },
        reset: match content.reset {
            Reset::RatingWindowStart => ResetPolicy::RatingWindowStart,
        },
        partial_window: match content.partial_window {
            PartialWindow::ActualQuantityFullThresholds => {
                PartialWindowPolicy::ActualQuantityFullThresholds
            }
        },
        fold: match content.fold {
            Fold::Sum => WindowFold::Sum,
        },
    }
}

fn core_invoice(invoice: &InvoiceInputs) -> CoreInvoiceInputs {
    CoreInvoiceInputs {
        template: invoice.template.clone(),
        template_digest: digest_hex(&invoice.template_digest),
        template_source: match invoice.template_source {
            PricingSource::Entry => InputSource::Entry,
            PricingSource::SkuVersion => InputSource::SkuVersion,
            PricingSource::SellerSettings => InputSource::SellerSettings,
        },
        gl_code: invoice.gl_code.clone(),
        tax_category: invoice.tax_category.clone(),
        timing: match invoice.timing {
            PricingTiming::Advance => BillingTiming::Advance,
            PricingTiming::Arrears => BillingTiming::Arrears,
        },
        rounding: match invoice.rounding {
            Rounding::HalfEven => StoredRounding::HalfEven,
        },
    }
}

/// The core's binding as Pricing's type, for Pricing's digest.
///
/// # Errors
///
/// [`ConvertError`] for a digest that is not hex or a currency the money crate refuses.
pub fn to_pricing(binding: &CoreBinding) -> Result<AcceptedBinding, ConvertError> {
    Ok(AcceptedBinding {
        item_id: binding.item_id,
        price_book_entry_id: binding.price_book_entry_id,
        dimension_key: binding.dimension_key.clone(),
        dimension_value: binding.dimension_value.clone(),
        sku_id: binding.sku_id,
        sku_version: binding.sku_version,
        sku_code: binding.sku_code.clone(),
        sku_name: binding.sku_name.clone(),
        unit: binding.unit.clone(),
        meter: binding.meter.as_ref().map(|meter| PricingMeterRef {
            usage_type_id: meter.usage_type_id.clone(),
            version: meter.version.clone(),
        }),
        price: pricing_price(&binding.price)?,
        kind: match binding.kind {
            ChargeKind::Recurring => PricingChargeKind::Recurring,
            ChargeKind::Usage => PricingChargeKind::Usage,
            ChargeKind::OneTime => PricingChargeKind::OneTime,
        },
        recurring_period: binding.recurring_period.map(|period| match period {
            RecurringPeriod::Month => BillingCycle::Month,
            RecurringPeriod::Year => BillingCycle::Year,
        }),
        via_default: binding.via_default,
        usage_rating_policy: binding
            .usage_policy
            .as_ref()
            .map(pricing_policy)
            .transpose()?,
        invoice: pricing_invoice(&binding.invoice)?,
    })
}

fn pricing_price(price: &CorePrice) -> Result<ImmutablePrice, ConvertError> {
    Ok(ImmutablePrice {
        price_id: price.price_id,
        price_book_entry_id: price.price_book_entry_id,
        money_digest: digest_bytes("price.money_digest", &price.money_digest)?,
        currency: bss_money::CurrencySpec::try_new(
            price.currency.code.clone(),
            price.currency.scale,
        )
        .map_err(|e| ConvertError::new("price.currency", e.to_string()))?,
        model: match &price.model {
            CorePriceModel::Flat { amount } => PriceModel::Flat { amount: *amount },
            CorePriceModel::PerUnit { unit_amount } => PriceModel::PerUnit {
                unit_amount: *unit_amount,
            },
            CorePriceModel::Volume { tiers } => PriceModel::Volume {
                tiers: tiers.iter().map(pricing_tier).collect(),
            },
            CorePriceModel::Graduated { tiers } => PriceModel::Graduated {
                tiers: tiers.iter().map(pricing_tier).collect(),
            },
            CorePriceModel::Package {
                package_size,
                package_price,
            } => PriceModel::Package {
                package_size: *package_size,
                package_price: *package_price,
            },
        },
        minimum_fee: price.minimum_fee,
        effective_from: price.effective_from,
        ends_on: price.ends_on,
        state: match price.state {
            PriceState::Approved => PricingPriceState::Approved,
            PriceState::Cancelled => PricingPriceState::Cancelled,
        },
    })
}

const fn pricing_tier(tier: &CoreTier) -> Tier {
    Tier {
        up_to: tier.up_to,
        rate: tier.rate,
    }
}

fn pricing_policy(policy: &CoreUsagePolicy) -> Result<UsageRatingPolicy, ConvertError> {
    Ok(UsageRatingPolicy {
        policy_id: policy.policy_id,
        version: policy.version,
        digest: digest_bytes("usage_policy.digest", &policy.digest)?,
        content: UsageRatingPolicyInput {
            rating_window: match policy.rating_window {
                RatingWindowKind::BillingCycle => RatingWindow::BillingCycle,
                RatingWindowKind::CalendarHourUtc => RatingWindow::CalendarHour {
                    timezone: Timezone::Utc,
                },
            },
            aggregation_scope: match policy.aggregation_scope {
                PolicyScope::SubscriptionLine => AggregationScope::SubscriptionLine,
                PolicyScope::Resource => AggregationScope::Resource,
            },
            reset: match policy.reset {
                ResetPolicy::RatingWindowStart => Reset::RatingWindowStart,
            },
            partial_window: match policy.partial_window {
                PartialWindowPolicy::ActualQuantityFullThresholds => {
                    PartialWindow::ActualQuantityFullThresholds
                }
            },
            fold: match policy.fold {
                WindowFold::Sum => Fold::Sum,
            },
        },
    })
}

fn pricing_invoice(invoice: &CoreInvoiceInputs) -> Result<InvoiceInputs, ConvertError> {
    Ok(InvoiceInputs {
        template: invoice.template.clone(),
        template_digest: digest_bytes("invoice.template_digest", &invoice.template_digest)?,
        template_source: match invoice.template_source {
            InputSource::Entry => PricingSource::Entry,
            InputSource::SkuVersion => PricingSource::SkuVersion,
            InputSource::SellerSettings => PricingSource::SellerSettings,
        },
        gl_code: invoice.gl_code.clone(),
        tax_category: invoice.tax_category.clone(),
        timing: match invoice.timing {
            BillingTiming::Advance => PricingTiming::Advance,
            BillingTiming::Arrears => PricingTiming::Arrears,
        },
        rounding: match invoice.rounding {
            StoredRounding::HalfEven => Rounding::HalfEven,
        },
    })
}

/// Pricing's cell as the core reads it.
#[must_use]
pub fn cell_to_core(cell: &ResolvedCell) -> CoreCell {
    CoreCell {
        selection: CellSelection {
            item_id: cell.selection.item_id,
            dimension_value: cell.selection.dimension_value.clone(),
        },
        binding: cell.binding.as_ref().map(to_core),
    }
}

/// The core's cell as Pricing's type.
///
/// # Errors
///
/// As [`to_pricing`].
pub fn cell_to_pricing(cell: &CoreCell) -> Result<ResolvedCell, ConvertError> {
    Ok(ResolvedCell {
        selection: BindingSelection {
            item_id: cell.selection.item_id,
            dimension_value: cell.selection.dimension_value.clone(),
        },
        binding: cell.binding.as_ref().map(to_pricing).transpose()?,
    })
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod tests;

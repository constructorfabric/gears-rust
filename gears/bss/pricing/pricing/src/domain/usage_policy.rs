//! Shape and semantic identity of immutable entry policies.
use super::RuleError;
use bss_pricing_sdk::{
    Digest,
    terms::{AggregationScope, RatingWindow, UsageRatingPolicyInput},
};

/// Validate shape only. The five rating rules have no free text; the meter is the SKU's.
/// # Errors
/// The rules-only content has no shape refusal. The meter gate is [`validate_meter_policy`].
#[allow(
    clippy::unnecessary_wraps,
    reason = "callers share this Result with the meter gate"
)]
pub fn validate_policy_shape(_policy: &UsageRatingPolicyInput) -> Result<(), RuleError> {
    Ok(())
}

/// The meter a provider is asked for the SKU's `usage_type_ref`.
///
/// A derived id `products.derived/<code>@<n>` is asked at version `<n>`. Any other ref is asked at
/// `v1`: a raw meter is unconfigured before that version is read, and the pricing fixtures declare `v1`.
#[must_use]
pub fn meter_ref(usage_type_ref: &str) -> bss_pricing_sdk::terms::MeterRef {
    let version = bss_products_sdk::derived::MeterId::parse(usage_type_ref)
        .map_or_else(|_| "v1".to_owned(), |id| id.version().to_string());
    bss_pricing_sdk::terms::MeterRef {
        usage_type_id: usage_type_ref.to_owned(),
        version,
    }
}

/// Content identity for the full entry key; absence is reserved for legacy/non-usage entries.
#[must_use]
pub fn entry_policy_key(policy: &UsageRatingPolicyInput) -> Digest {
    bss_pricing_sdk::digest::policy_digest(policy)
}

/// A minimum fee is unsupported on an hourly window and on any resource-scoped policy (D-504).
#[must_use]
pub fn refuses_minimum_fee(content: &UsageRatingPolicyInput) -> bool {
    matches!(content.rating_window, RatingWindow::CalendarHour { .. })
        || content.aggregation_scope == AggregationScope::Resource
}

/// Compare the SKU's meter and unit with the provider's answer and the policy fold.
/// # Errors
/// `METER_POLICY_MISMATCH` means the answer does not certify this SKU's quantities.
pub fn validate_meter_policy(
    policy: &UsageRatingPolicyInput,
    sku_ref: &str,
    sku_unit: &str,
    semantics: &bss_pricing_sdk::meter_semantics::MeterSemantics,
) -> Result<(), RuleError> {
    validate_policy_shape(policy)?;
    let asked = meter_ref(sku_ref);
    if semantics.meter != asked
        || semantics.canonical_unit != sku_unit
        || semantics.fold != policy.fold
        || policy.fold != bss_pricing_sdk::terms::Fold::Sum
        || !semantics.source_integrated
    {
        return Err(RuleError::new("METER_POLICY_MISMATCH"));
    }
    Ok(())
}

/// A deploy-3 `quantity_semantics` copy, checked against the SKU and the provider, then dropped.
pub struct LegacyQuantity<'a> {
    /// Meter id the client sent.
    pub meter_id: &'a str,
    /// Meter version the client sent.
    pub meter_version: &'a str,
    /// Unit the client sent.
    pub unit: &'a str,
    /// Accrual version the client sent.
    pub accrual: &'a str,
    /// Fold the client sent.
    pub fold: bss_pricing_sdk::terms::Fold,
}

/// A deploy-3 `quantity_semantics` object, verified against the SKU and the provider, then dropped.
/// # Errors
/// `METER_POLICY_MISMATCH` when the copy does not equal the SKU and the provider's answer.
pub fn legacy_quantity_matches(
    legacy: &LegacyQuantity<'_>,
    sku_ref: &str,
    sku_unit: &str,
    semantics: &bss_pricing_sdk::meter_semantics::MeterSemantics,
) -> Result<(), RuleError> {
    let asked = meter_ref(sku_ref);
    if legacy.meter_id.trim().is_empty()
        || legacy.meter_version.trim().is_empty()
        || legacy.unit.trim().is_empty()
        || legacy.accrual.trim().is_empty()
        || legacy.meter_id != asked.usage_type_id
        || legacy.meter_version != asked.version
        || legacy.unit != sku_unit
        || legacy.unit != semantics.canonical_unit
        || legacy.fold != semantics.fold
        || legacy.accrual != semantics.accrual_policy_version
    {
        return Err(RuleError::new("METER_POLICY_MISMATCH"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "the assertion is the refusal")]
    use bss_pricing_sdk::terms::*;
    #[test]
    fn rules_only_shape_has_no_meter_text_to_refuse() {
        let policy = UsageRatingPolicyInput {
            rating_window: RatingWindow::CalendarHour {
                timezone: Timezone::Utc,
            },
            aggregation_scope: AggregationScope::SubscriptionLine,
            reset: Reset::RatingWindowStart,
            partial_window: PartialWindow::ActualQuantityFullThresholds,
            fold: Fold::Sum,
        };
        assert!(super::validate_policy_shape(&policy).is_ok());
    }

    #[test]
    fn a_resource_scoped_floor_and_an_hourly_fee_are_the_same_refusal() {
        use bss_pricing_sdk::terms::*;
        let mut policy = UsageRatingPolicyInput {
            rating_window: RatingWindow::BillingCycle,
            aggregation_scope: AggregationScope::SubscriptionLine,
            reset: Reset::RatingWindowStart,
            partial_window: PartialWindow::ActualQuantityFullThresholds,
            fold: Fold::Sum,
        };
        assert!(!super::refuses_minimum_fee(&policy));
        policy.aggregation_scope = AggregationScope::Resource;
        assert!(super::refuses_minimum_fee(&policy));
        policy.aggregation_scope = AggregationScope::SubscriptionLine;
        policy.rating_window = RatingWindow::CalendarHour {
            timezone: Timezone::Utc,
        };
        assert!(super::refuses_minimum_fee(&policy));
    }
}

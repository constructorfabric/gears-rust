//! Typed pricing invoice and entry policy projections required by reads.

use uuid::Uuid;

use crate::Digest;

/// `BillingCycle` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BillingCycle {
    /// A calendar month.
    Month,
    /// A calendar year.
    Year,
}

/// `Timezone` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timezone {
    /// UTC. The only zone an hourly window may name.
    Utc,
}

/// `RatingWindow` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatingWindow {
    /// The subscription billing cycle.
    BillingCycle,
    /// One clock hour in `timezone`.
    CalendarHour {
        /// The zone the hour is aligned to.
        timezone: Timezone,
    },
}

/// `AggregationScope` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggregationScope {
    /// Quantities fold on the subscription line.
    SubscriptionLine,
    /// Quantities fold on one resource.
    Resource,
}

/// `Reset` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reset {
    /// The accumulator resets when the rating window starts.
    RatingWindowStart,
}

/// `Fold` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fold {
    /// Sum the quantities in the window.
    Sum,
}

/// `PartialWindow` value in the versioned pricing read contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartialWindow {
    /// Rate the quantity that occurred; thresholds stay whole.
    ActualQuantityFullThresholds,
}

/// `MeterRef` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterRef {
    /// Usage type id.
    pub usage_type_id: String,
    /// Version.
    pub version: String,
}

/// `UsageRatingPolicyInput` value in the versioned pricing read contract.
///
/// The five rating rules only. The meter, the unit and the accrual version live on the SKU revision, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRatingPolicyInput {
    /// Rating window.
    pub rating_window: RatingWindow,

    /// Aggregation scope.
    pub aggregation_scope: AggregationScope,

    /// Reset.
    pub reset: Reset,

    /// Partial window.
    pub partial_window: PartialWindow,

    /// Fold of quantities inside the window.
    pub fold: Fold,
}

/// `UsageRatingPolicy` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRatingPolicy {
    /// Policy id.
    pub policy_id: Uuid,

    /// Version.
    pub version: u64,

    /// Digest.
    pub digest: Digest,

    /// Content.
    pub content: UsageRatingPolicyInput,
}

/// `BillingTiming` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingTiming {
    Advance,
    Arrears,
}

/// `InputSource` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSource {
    Entry,
    SkuVersion,
    SellerSettings,
}

/// `Rounding` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rounding {
    HalfEven,
}

/// `InvoiceInputs` value in the versioned pricing read contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceInputs {
    /// Template.
    pub template: String,

    /// Template digest.
    pub template_digest: Digest,

    /// Template source.
    pub template_source: InputSource,

    /// Gl code.
    pub gl_code: String,

    /// Tax category.
    pub tax_category: String,

    /// Timing.
    pub timing: BillingTiming,

    /// Currency scale.
    pub currency_scale: u32,

    /// Rounding.
    pub rounding: Rounding,
}

/// Invoice-period origin, resolved before Pricing is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingAnchor {
    /// First day of the calendar month or year at midnight UTC.
    Calendar,
    /// Explicit subscription anniversary; Pricing never rounds it.
    SubscriptionStart,
}
/// Provenance of the resolved invoice terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermsSource {
    /// Explicit order intent.
    ExplicitOrder,
    /// Immutable seller policy version.
    SellerPolicy {
        /// Policy identity.
        id: Uuid,
        /// Positive policy version.
        version: u64,
    },
}
/// Versioned consumer projection of Subscriptions-owned invoice terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingTerms {
    /// Supported new-sale snapshot version is 1.
    pub schema_version: u32,
    /// Invoice frequency, independent of usage rating windows.
    pub cycle: BillingCycle,
    /// Resolved anchor convention.
    pub anchor: BillingAnchor,
    /// Resolved anchor instant, preserved without normalization.
    pub anchor_at: time::OffsetDateTime,
    /// Commercial timezone.
    pub timezone: Timezone,
    /// Explicit provenance.
    pub source: TermsSource,
    /// Canonical snapshot digest, excluding this field itself.
    pub digest: Digest,
}

impl std::str::FromStr for BillingCycle {
    type Err = crate::acceptance::UnsupportedCommercialValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            _ => Err(crate::acceptance::UnsupportedCommercialValue {
                field: "cycle",
                value: value.into(),
                reason: crate::acceptance::CommercialReason::UnsupportedTerms,
            }),
        }
    }
}

impl std::str::FromStr for BillingAnchor {
    type Err = crate::acceptance::UnsupportedCommercialValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "calendar" => Ok(Self::Calendar),
            "subscription_start" => Ok(Self::SubscriptionStart),
            _ => Err(crate::acceptance::UnsupportedCommercialValue {
                field: "anchor",
                value: value.into(),
                reason: crate::acceptance::CommercialReason::UnsupportedTerms,
            }),
        }
    }
}

impl std::str::FromStr for Timezone {
    type Err = crate::acceptance::UnsupportedCommercialValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "UTC" => Ok(Self::Utc),
            _ => Err(crate::acceptance::UnsupportedCommercialValue {
                field: "timezone",
                value: value.into(),
                reason: crate::acceptance::CommercialReason::UnsupportedWindow,
            }),
        }
    }
}

impl std::str::FromStr for Rounding {
    type Err = crate::acceptance::UnsupportedCommercialValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "half_even" => Ok(Self::HalfEven),
            _ => Err(crate::acceptance::UnsupportedCommercialValue {
                field: "rounding",
                value: value.into(),
                reason: crate::acceptance::CommercialReason::UnsupportedTerms,
            }),
        }
    }
}

impl std::str::FromStr for AggregationScope {
    type Err = crate::acceptance::UnsupportedCommercialValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "subscription_line" => Ok(Self::SubscriptionLine),
            "resource" => Ok(Self::Resource),
            _ => Err(crate::acceptance::UnsupportedCommercialValue {
                field: "aggregation_scope",
                value: value.into(),
                reason: crate::acceptance::CommercialReason::UnsupportedScope,
            }),
        }
    }
}

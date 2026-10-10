//! Explicit Orders schema-2 projection of public Pricing SDK values only.
// Reviewed with S1-04; every field is required, including nullable fields.
#![allow(dead_code)] // Serde remote definition types are never constructed.
use bss_pricing_sdk::{
    Digest,
    acceptance::{Market, NewSaleQuery, TenantAxes, Term},
    read::{
        AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, PriceModel, PriceState, Tier,
    },
    terms::{
        AggregationScope, BillingAnchor, BillingCycle, BillingTerms, BillingTiming, Fold,
        InputSource, InvoiceInputs, MeterRef, PartialWindow, RatingWindow, Reset, Rounding,
        TermsSource, Timezone, UsageRatingPolicy, UsageRatingPolicyInput,
    },
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::acceptance::TenantAxes",
    deny_unknown_fields
)]
#[allow(clippy::struct_field_names)] // Exact native tenant-axis names.
pub(super) struct TenantAxesDef {
    seller_tenant_id: Uuid,
    payer_tenant_id: Uuid,
    resource_tenant_id: Uuid,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::acceptance::Market", deny_unknown_fields)]
pub(super) struct MarketDef {
    currency: String,
    #[serde(deserialize_with = "crate::commercial::scalars::required_option")]
    region: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::acceptance::NewSaleQuery",
    deny_unknown_fields
)]
pub(super) struct NewSaleQueryDef {
    #[serde(with = "TenantAxesDef")]
    tenant_axes: TenantAxes,
    order_id: Uuid,
    #[serde(with = "crate::commercial::scalars::unsigned")]
    order_version: u64,
    line_id: Uuid,
    plan_id: Uuid,
    plan_revision_id: Uuid,
    #[serde(with = "vec_binding_selection")]
    selections: Vec<BindingSelection>,
    #[serde(with = "crate::commercial::scalars::decimal")]
    quantity: Decimal,
    #[serde(with = "MarketDef")]
    market: Market,
    #[serde(with = "crate::commercial::scalars::instant")]
    start_at: OffsetDateTime,
    #[serde(with = "TermDef")]
    term: Term,
    #[serde(with = "BillingTermsDef")]
    billing_terms: BillingTerms,
    #[serde(with = "crate::commercial::scalars::digest")]
    resolved_bindings_digest: Digest,
    #[serde(with = "crate::commercial::scalars::unsigned")]
    hold_policy_version: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::acceptance::AcceptanceReceipt",
    deny_unknown_fields
)]
pub(super) struct AcceptanceReceiptDef {
    acceptance_id: Uuid,
    #[serde(with = "crate::commercial::scalars::digest")]
    request_digest: Digest,
    #[serde(with = "crate::commercial::scalars::digest")]
    terms_digest: Digest,
    #[serde(with = "NewSaleQueryDef")]
    query: NewSaleQuery,
    #[serde(with = "crate::commercial::scalars::instant")]
    accepted_at: OffsetDateTime,
    #[serde(with = "crate::commercial::scalars::instant")]
    hold_until: OffsetDateTime,
    #[serde(with = "vec_accepted_binding")]
    bindings: Vec<AcceptedBinding>,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::read::Tier", deny_unknown_fields)]
pub(super) struct TierDef {
    #[serde(with = "option_decimal")]
    up_to: Option<Decimal>,
    #[serde(with = "crate::commercial::scalars::decimal")]
    rate: Decimal,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::read::ImmutablePrice", deny_unknown_fields)]
pub(super) struct ImmutablePriceDef {
    price_id: Uuid,
    price_book_entry_id: Uuid,
    #[serde(with = "crate::commercial::scalars::digest")]
    money_digest: Digest,
    currency: String,
    #[serde(with = "PriceModelDef")]
    model: PriceModel,
    #[serde(with = "option_decimal")]
    minimum_fee: Option<Decimal>,
    #[serde(with = "crate::commercial::scalars::date")]
    effective_from: Date,
    #[serde(with = "option_date")]
    ends_on: Option<Date>,
    #[serde(with = "crate::commercial::scalars::price_state")]
    state: PriceState,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::read::AcceptedBinding", deny_unknown_fields)]
pub(super) struct AcceptedBindingDef {
    item_id: Uuid,
    price_book_entry_id: Uuid,
    #[serde(deserialize_with = "crate::commercial::scalars::required_option")]
    dimension_key: Option<String>,
    #[serde(deserialize_with = "crate::commercial::scalars::required_option")]
    dimension_value: Option<String>,
    sku_id: Uuid,
    #[serde(with = "crate::commercial::scalars::signed")]
    sku_version: i64,
    sku_code: String,
    sku_name: String,
    #[serde(deserialize_with = "crate::commercial::scalars::required_option")]
    unit: Option<String>,
    #[serde(with = "option_meter_ref")]
    meter: Option<MeterRef>,
    #[serde(with = "ImmutablePriceDef")]
    price: ImmutablePrice,
    #[serde(with = "ChargeKindDef")]
    kind: ChargeKind,
    #[serde(with = "option_billing_cycle")]
    recurring_period: Option<BillingCycle>,
    via_default: bool,
    #[serde(with = "option_usage_rating_policy")]
    usage_rating_policy: Option<UsageRatingPolicy>,
    #[serde(with = "InvoiceInputsDef")]
    invoice: InvoiceInputs,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::read::BindingSelection",
    deny_unknown_fields
)]
pub(super) struct BindingSelectionDef {
    item_id: Uuid,
    #[serde(deserialize_with = "crate::commercial::scalars::required_option")]
    dimension_value: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::terms::MeterRef", deny_unknown_fields)]
pub(super) struct MeterRefDef {
    usage_type_id: String,
    version: String,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::UsageRatingPolicyInput",
    deny_unknown_fields
)]
pub(super) struct UsageRatingPolicyInputDef {
    #[serde(with = "RatingWindowDef")]
    rating_window: RatingWindow,
    #[serde(with = "AggregationScopeDef")]
    aggregation_scope: AggregationScope,
    #[serde(with = "ResetDef")]
    reset: Reset,
    #[serde(with = "PartialWindowDef")]
    partial_window: PartialWindow,
    #[serde(with = "FoldDef")]
    fold: Fold,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::UsageRatingPolicy",
    deny_unknown_fields
)]
pub(super) struct UsageRatingPolicyDef {
    policy_id: Uuid,
    #[serde(with = "crate::commercial::scalars::unsigned")]
    version: u64,
    #[serde(with = "crate::commercial::scalars::digest")]
    digest: Digest,
    #[serde(with = "UsageRatingPolicyInputDef")]
    content: UsageRatingPolicyInput,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::terms::InvoiceInputs", deny_unknown_fields)]
pub(super) struct InvoiceInputsDef {
    template: String,
    #[serde(with = "crate::commercial::scalars::digest")]
    template_digest: Digest,
    #[serde(with = "InputSourceDef")]
    template_source: InputSource,
    gl_code: String,
    tax_category: String,
    #[serde(with = "BillingTimingDef")]
    timing: BillingTiming,
    currency_scale: u32,
    #[serde(with = "RoundingDef")]
    rounding: Rounding,
}
#[derive(Serialize, Deserialize)]
#[serde(remote = "bss_pricing_sdk::terms::BillingTerms", deny_unknown_fields)]
pub(super) struct BillingTermsDef {
    schema_version: u32,
    #[serde(with = "BillingCycleDef")]
    cycle: BillingCycle,
    #[serde(with = "BillingAnchorDef")]
    anchor: BillingAnchor,
    #[serde(with = "crate::commercial::scalars::instant")]
    anchor_at: OffsetDateTime,
    #[serde(with = "TimezoneDef")]
    timezone: Timezone,
    #[serde(with = "TermsSourceDef")]
    source: TermsSource,
    #[serde(with = "crate::commercial::scalars::digest")]
    digest: Digest,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::acceptance::Term",
    rename_all = "snake_case",
    deny_unknown_fields,
    tag = "kind"
)]
pub(super) enum TermDef {
    Rolling,
    FixedPeriods { count: u32 },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::read::PriceModel",
    rename_all = "snake_case",
    deny_unknown_fields,
    tag = "kind"
)]
pub(super) enum PriceModelDef {
    Flat {
        #[serde(with = "crate::commercial::scalars::decimal")]
        amount: Decimal,
    },
    PerUnit {
        #[serde(with = "crate::commercial::scalars::decimal")]
        unit_amount: Decimal,
    },
    Volume {
        #[serde(with = "vec_tier")]
        tiers: Vec<Tier>,
    },
    Graduated {
        #[serde(with = "vec_tier")]
        tiers: Vec<Tier>,
    },
    Package {
        #[serde(with = "crate::commercial::scalars::decimal")]
        package_size: Decimal,
        #[serde(with = "crate::commercial::scalars::decimal")]
        package_price: Decimal,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::read::ChargeKind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum ChargeKindDef {
    Recurring,
    Usage,
    OneTime,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::BillingCycle",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum BillingCycleDef {
    Month,
    Year,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::Timezone",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum TimezoneDef {
    Utc,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::RatingWindow",
    rename_all = "snake_case",
    deny_unknown_fields,
    tag = "kind"
)]
pub(super) enum RatingWindowDef {
    BillingCycle,
    CalendarHour {
        #[serde(with = "TimezoneDef")]
        timezone: Timezone,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::AggregationScope",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum AggregationScopeDef {
    SubscriptionLine,
    Resource,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::Reset",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum ResetDef {
    RatingWindowStart,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::Fold",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum FoldDef {
    Sum,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::PartialWindow",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum PartialWindowDef {
    ActualQuantityFullThresholds,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::BillingTiming",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum BillingTimingDef {
    Advance,
    Arrears,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::InputSource",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum InputSourceDef {
    Entry,
    SkuVersion,
    SellerSettings,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::Rounding",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum RoundingDef {
    HalfEven,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::BillingAnchor",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum BillingAnchorDef {
    Calendar,
    SubscriptionStart,
}
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "bss_pricing_sdk::terms::TermsSource",
    rename_all = "snake_case",
    deny_unknown_fields,
    tag = "kind"
)]
pub(super) enum TermsSourceDef {
    ExplicitOrder,
    SellerPolicy {
        id: Uuid,
        #[serde(with = "crate::commercial::scalars::unsigned")]
        version: u64,
    },
}
mod vec_binding_selection {
    use super::{BindingSelection, BindingSelectionDef, Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "BindingSelectionDef")] BindingSelection);
    pub fn serialize<S: serde::Serializer>(
        value: &[BindingSelection],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .cloned()
            .map(Value)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<BindingSelection>, D::Error> {
        Ok(Vec::<Value>::deserialize(deserializer)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
}
mod vec_accepted_binding {
    use super::{AcceptedBinding, AcceptedBindingDef, Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "AcceptedBindingDef")] AcceptedBinding);
    pub fn serialize<S: serde::Serializer>(
        value: &[AcceptedBinding],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .cloned()
            .map(Value)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<AcceptedBinding>, D::Error> {
        Ok(Vec::<Value>::deserialize(deserializer)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
}
mod option_decimal {
    use super::{Decimal, Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "crate::commercial::scalars::decimal")] Decimal);
    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: serde::Serializer>(
        value: &Option<Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (*value).map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Decimal>, D::Error> {
        Ok(Option::<Value>::deserialize(deserializer)?.map(|v| v.0))
    }
}
mod option_date {
    use super::{Date, Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "crate::commercial::scalars::date")] Date);
    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: serde::Serializer>(
        value: &Option<Date>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (*value).map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Date>, D::Error> {
        Ok(Option::<Value>::deserialize(deserializer)?.map(|v| v.0))
    }
}
mod option_meter_ref {
    use super::{Deserialize, MeterRef, MeterRefDef, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "MeterRefDef")] MeterRef);
    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: serde::Serializer>(
        value: &Option<MeterRef>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.clone().map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<MeterRef>, D::Error> {
        Ok(Option::<Value>::deserialize(deserializer)?.map(|v| v.0))
    }
}
mod option_billing_cycle {
    use super::{BillingCycle, BillingCycleDef, Deserialize, Serialize};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "BillingCycleDef")] BillingCycle);
    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: serde::Serializer>(
        value: &Option<BillingCycle>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (*value).map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<BillingCycle>, D::Error> {
        Ok(Option::<Value>::deserialize(deserializer)?.map(|v| v.0))
    }
}
mod option_usage_rating_policy {
    use super::{Deserialize, Serialize, UsageRatingPolicy, UsageRatingPolicyDef};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "UsageRatingPolicyDef")] UsageRatingPolicy);
    #[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // Serde with-hook signature.
    pub fn serialize<S: serde::Serializer>(
        value: &Option<UsageRatingPolicy>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.clone().map(Value).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<UsageRatingPolicy>, D::Error> {
        Ok(Option::<Value>::deserialize(deserializer)?.map(|v| v.0))
    }
}
mod vec_tier {
    use super::{Deserialize, Serialize, Tier, TierDef};
    #[derive(Serialize, Deserialize)]
    struct Value(#[serde(with = "TierDef")] Tier);
    pub fn serialize<S: serde::Serializer>(
        value: &[Tier],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .cloned()
            .map(Value)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Tier>, D::Error> {
        Ok(Vec::<Value>::deserialize(deserializer)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
}

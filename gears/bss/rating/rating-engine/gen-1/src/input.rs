//! Generation 1's frozen input types (rating DESIGN 01 §3.1).
//!
//! `EvaluationInput` and the `Core*` types mirror what the pipeline assembles: the fact version,
//! the bindings Pricing resolved (pricing-sdk `ResolvedBindings`, `AcceptedBinding`), the derived
//! declaration, the subscription version, the per-slice quantities and the engine identity. They
//! are this generation's own types: `rating-core` maps the pipeline's values into them, numbers as
//! canonical text, and nothing in the workspace depends on them otherwise (ADR-0006).
//!
//! Every number is canonical text on the wire: a Pricing decimal and a quantity both read with
//! fractional trailing zeros dropped and are written canonically (`decimal_text`,
//! [`ExactQuantity`]). Every instant is a whole UTC second written in RFC 3339
//! (`2026-09-15T00:00:00Z`); every interval is half-open.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::budget::ArithmeticBudget;
use crate::exact::ExactQuantity;

time::serde::format_description!(pub(crate) iso_date, Date, "[year]-[month]-[day]");

/// Serde for an optional calendar date as `YYYY-MM-DD` or null.
mod iso_date_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use time::Date;

    #[derive(Serialize, Deserialize)]
    struct Text(#[serde(with = "super::iso_date")] Date);

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "`#[serde(with)]` requires the `&Option<T>` signature"
    )]
    pub(super) fn serialize<S: Serializer>(
        value: &Option<Date>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(Text).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Date>, D::Error> {
        Ok(Option::<Text>::deserialize(deserializer)?.map(|text| text.0))
    }
}

/// An engine generation and the digest of its frozen tree; results record it as their
/// engine-of-record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineId {
    pub engine_generation: u32,
    /// Lower-case hex SHA-256.
    pub engine_digest: String,
}

/// Everything one evaluation of one child reads. The only definition (DESIGN 01 §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInput {
    pub child: ChildSpec,
    pub fact: FactVersion,
    pub pricing: PricingInput,
    /// Products' declaration at the binding's meter, for a usage child (T-D-76).
    pub derived: Option<FrozenDerivedDeclaration>,
    pub subscription: SubscriptionVersion,
    pub quantities: ChildQuantities,
    /// The other members of a window group: context only, never billed here (T-D-67).
    pub group_context: Option<GroupContext>,
    /// The arithmetic budget of the generation the pipeline dispatches to.
    pub budget: ArithmeticBudget,
    /// The engine-of-record this input is evaluated under.
    pub engine: EngineId,
}

/// What a child rates (DESIGN §3.1). There is no per-event child: Pricing has no such window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildKind {
    /// One rating window of a usage binding's `usage_rating_policy`.
    UsageWindow,
    /// One billing period of a recurring line.
    PeriodLine,
    /// One one-time charge (rated only once R-19 is accepted).
    OneTime,
}

/// The child being evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildSpec {
    pub child_id: Uuid,
    pub kind: ChildKind,
    pub window: RatingWindow,
    pub aggregation_key: AggregationKey,
}

/// A child's window: the canonical window and the part of it the fact serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatingWindow {
    #[serde(with = "time::serde::rfc3339")]
    pub window_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub window_end: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub served_from: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub served_to: OffsetDateTime,
}

/// A half-open UTC interval `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
}

/// The three tenants of a subscription line.
#[expect(
    clippy::struct_field_names,
    reason = "the tenant axes as DESIGN §3.1 and the delivery name them"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantAxes {
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Uuid,
}

/// Where usage quantities fold (the policy's `aggregation_scope`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AggregationScope {
    SubscriptionLine,
    Resource { resource_id: String },
}

/// The key a child is evaluated under (DESIGN §3.1, `AggregationKey`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregationKey {
    pub tenant_axes: TenantAxes,
    pub subscription_id: Uuid,
    /// Subscriptions' component-interval coordinate (`plan#n`, `addon:{id}#n`).
    pub sub_line_key: String,
    pub item_id: Uuid,
    /// The derived usage type `products.derived/<code>@<n>` of a usage child.
    pub meter: Option<String>,
    pub dimension_key: Option<String>,
    pub scope: AggregationScope,
}

/// What a fact bills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Recurring,
    Usage,
    OneTime,
}

/// How a suspension affects recurring lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionPosture {
    /// Prorate over the un-suspended part of the period.
    PauseRecurring,
    /// Bill the full period.
    Continue,
}

/// A price adjustment an input can name. Every one is deferred (DECOMPOSITION §2.3): naming one
/// fails `adjustment_unsupported`, and nothing is applied by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentKind {
    ContractOverride,
    CommitmentPool,
    Reservation,
    Coupon,
    Promotion,
}

/// A price adjustment named by the fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjustmentRef {
    pub kind: AdjustmentKind,
    pub reference: String,
}

/// One accepted price binding of an item and dimension value (pricing-sdk `PricePin`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricePin {
    pub item_id: Uuid,
    pub dimension_value: Option<String>,
    pub price_id: Uuid,
}

/// The fact version the child rates (DESIGN 14 §3.1, `FactVersion`).
#[expect(
    clippy::struct_field_names,
    reason = "`fact_version` is the field name of DESIGN 14 §3.1"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactVersion {
    pub fact_id: Uuid,
    pub fact_version: u64,
    pub kind: FactKind,
    pub subscription_id: Uuid,
    pub sub_line_key: String,
    /// The billing currency code; a binding in another currency fails `fx_not_supported`.
    pub currency: String,
    /// The billing period `[start, end)`.
    pub period: Interval,
    /// The part of the period the fact serves.
    pub served: Interval,
    pub plan_revision_id: Uuid,
    /// The accepted price binding: the period's pins the customer accepted (R-29).
    pub accepted: Vec<PricePin>,
    /// The committed quantity of a recurring `per_unit` line.
    pub quantity: Option<ExactQuantity>,
    pub suspensions: Vec<Interval>,
    pub suspension_posture: SuspensionPosture,
    /// Price adjustments the fact names; each fails `adjustment_unsupported`.
    pub adjustments: Vec<AdjustmentRef>,
    /// A sold-as bundle the line names; bundles are dormant and fail closed (pricing D-411).
    pub bundle: Option<String>,
}

/// What the pipeline passes for Pricing (T-D-73): the binding of record and one resolve per
/// resolve date (the period start, then each `ends_on` inside the period up to the window end).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingInput {
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    pub pins: Vec<PricePin>,
    /// Hex digest of the ordered per-resolve `binding_digest`s.
    pub binding_of_record: String,
    pub resolves: Vec<CoreResolve>,
}

/// One answer of Pricing's `resolve` at one date (pricing-sdk `ResolvedBindings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreResolve {
    #[serde(with = "iso_date")]
    pub resolve_date: Date,
    /// Hex digest of this answer.
    pub binding_digest: String,
    pub cells: Vec<CoreCell>,
}

/// The item and dimension value a cell answers for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSelection {
    pub item_id: Uuid,
    pub dimension_value: Option<String>,
}

/// One cell of a resolve; no binding means uncovered, never free (pricing-sdk `ResolvedCell`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreCell {
    pub selection: CellSelection,
    pub binding: Option<CoreBinding>,
}

/// A derived usage meter reference (pricing-sdk `MeterRef`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterRef {
    pub usage_type_id: String,
    pub version: String,
}

/// What a binding charges for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    Usage,
    Recurring,
    OneTime,
}

/// The billing cycle of a recurring price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurringPeriod {
    Month,
    Year,
}

/// The price a cell is bound to (pricing-sdk `AcceptedBinding`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreBinding {
    pub item_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub dimension_key: Option<String>,
    pub dimension_value: Option<String>,
    pub sku_id: Uuid,
    pub sku_version: i64,
    pub sku_code: String,
    pub sku_name: String,
    pub unit: Option<String>,
    pub meter: Option<MeterRef>,
    pub price: CorePrice,
    pub kind: ChargeKind,
    pub recurring_period: Option<RecurringPeriod>,
    /// The requested value was covered by the default chain (pricing D-420).
    pub via_default: bool,
    pub usage_policy: Option<CoreUsagePolicy>,
    pub invoice: CoreInvoiceInputs,
}

/// A currency and the posting scale stored with the price (pricing D-531).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreCurrency {
    pub code: String,
    pub scale: u8,
}

/// Where a price stands; a binding's price is always approved (pricing D-520).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceState {
    Approved,
    Cancelled,
}

/// The immutable price of a binding (pricing-sdk `ImmutablePrice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorePrice {
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
    /// Hex digest of the price's money.
    pub money_digest: String,
    pub currency: CoreCurrency,
    pub model: CorePriceModel,
    #[serde(with = "crate::decimal_text::option")]
    pub minimum_fee: Option<Decimal>,
    #[serde(with = "iso_date")]
    pub effective_from: Date,
    /// The binding's own end (`temporary_until` or an explicit close), never `effective_to`.
    #[serde(with = "iso_date_option")]
    pub ends_on: Option<Date>,
    pub state: PriceState,
}

/// One band of a tiered price; `up_to` is exclusive and absent on the open band.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreTier {
    #[serde(with = "crate::decimal_text::option")]
    pub up_to: Option<Decimal>,
    #[serde(with = "crate::decimal_text")]
    pub rate: Decimal,
}

/// A price model in major units (pricing-sdk `PriceModel`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorePriceModel {
    Flat {
        #[serde(with = "crate::decimal_text")]
        amount: Decimal,
    },
    PerUnit {
        #[serde(with = "crate::decimal_text")]
        unit_amount: Decimal,
    },
    Volume {
        tiers: Vec<CoreTier>,
    },
    Graduated {
        tiers: Vec<CoreTier>,
    },
    Package {
        #[serde(with = "crate::decimal_text")]
        package_size: Decimal,
        #[serde(with = "crate::decimal_text")]
        package_price: Decimal,
    },
}

/// The window a usage price rates over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RatingWindowKind {
    BillingCycle,
    CalendarHourUtc,
}

/// Where a usage price folds its quantities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    SubscriptionLine,
    Resource,
}

/// When the band accumulator resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetPolicy {
    RatingWindowStart,
}

/// How a partial window is rated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialWindowPolicy {
    ActualQuantityFullThresholds,
}

/// How quantities fold inside a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowFold {
    Sum,
}

/// A usage price's rating policy (pricing-sdk `UsageRatingPolicy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreUsagePolicy {
    pub policy_id: Uuid,
    pub version: u64,
    /// Hex digest of the policy content.
    pub digest: String,
    pub rating_window: RatingWindowKind,
    pub aggregation_scope: PolicyScope,
    pub reset: ResetPolicy,
    pub partial_window: PartialWindowPolicy,
    pub fold: WindowFold,
}

/// Where an invoice input came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Entry,
    SkuVersion,
    SellerSettings,
}

/// When the line is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingTiming {
    Advance,
    Arrears,
}

/// The tenant rounding Pricing stores; Billing rounds, so evaluation never uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredRounding {
    HalfEven,
}

/// The binding's invoice inputs (pricing-sdk `InvoiceInputs`); no scale: the price carries it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreInvoiceInputs {
    pub template: String,
    /// Hex digest of the template.
    pub template_digest: String,
    pub template_source: InputSource,
    pub gl_code: String,
    pub tax_category: String,
    pub timing: BillingTiming,
    pub rounding: StoredRounding,
}

/// Products' derived usage declaration at the binding's meter, frozen with the result (T-D-76).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenDerivedDeclaration {
    pub code: String,
    pub version: u32,
    /// Hex digest of Products' canonical declaration bytes.
    pub digest: String,
    pub declaration: DerivedDeclaration,
}

/// Products' `DerivedUsageDeclaration`, as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedDeclaration {
    pub output_unit: String,
    pub granularity: Granularity,
    pub inputs: Vec<DerivedInput>,
    pub formula: DerivedExpr,
    pub output_scale: u32,
    pub output_round: RoundMode,
}

/// The span a derived formula applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Hour,
}

/// How one derived input folds over a granule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GranuleFold {
    Sum,
    Peak,
    TimeWeighted,
}

/// One input of a derived declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedInput {
    pub name: String,
    /// The raw GTS usage type, with its exact version.
    pub usage_type_ref: String,
    pub granule_fold: GranuleFold,
    pub max_hold_seconds: Option<u32>,
    pub unit: String,
}

/// How a derived value is rounded to a scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundMode {
    HalfEven,
    HalfUp,
    Up,
    Down,
}

/// A derived formula over the inputs' folded quantities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DerivedExpr {
    Input {
        name: String,
    },
    Const {
        #[serde(with = "crate::decimal_text")]
        value: Decimal,
    },
    Add {
        left: Box<DerivedExpr>,
        right: Box<DerivedExpr>,
    },
    Sub {
        left: Box<DerivedExpr>,
        right: Box<DerivedExpr>,
    },
    Mul {
        left: Box<DerivedExpr>,
        right: Box<DerivedExpr>,
    },
    DivConst {
        operand: Box<DerivedExpr>,
        #[serde(with = "crate::decimal_text")]
        divisor: Decimal,
    },
    Max {
        operands: Vec<DerivedExpr>,
    },
    Min {
        operands: Vec<DerivedExpr>,
    },
    Ceil {
        operand: Box<DerivedExpr>,
    },
    Floor {
        operand: Box<DerivedExpr>,
    },
    Round {
        operand: Box<DerivedExpr>,
        scale: u32,
        mode: RoundMode,
    },
}

/// The subscription version the fact belongs to.
#[expect(
    clippy::struct_field_names,
    reason = "`subscription_version` is the field name of DESIGN §4.1"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionVersion {
    pub subscription_id: Uuid,
    pub subscription_version: u64,
    pub tenant_axes: TenantAxes,
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub quantity_intervals: Vec<QuantityInterval>,
    pub brand: Option<String>,
}

/// A committed quantity in force over an interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuantityInterval {
    pub interval: Interval,
    pub quantity: ExactQuantity,
}

/// The child's quantities under one immutable counter layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildQuantities {
    pub layout_version: u64,
    pub slices: Vec<SliceQuantity>,
}

/// One slice of the child: its bounds, its quantity and, for a usage child, its granules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceQuantity {
    #[serde(with = "time::serde::rfc3339")]
    pub slice_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub slice_end: OffsetDateTime,
    /// The slice quantity: the committed quantity of a recurring slice; for a usage slice the
    /// pipeline's count, which the meter step recomputes from the granules.
    pub q: ExactQuantity,
    pub q_version: u64,
    /// Per UTC-hour granule and raw input type, for a usage slice.
    pub granules: Vec<Granule>,
}

/// One UTC hour of a usage slice, folded per raw input type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Granule {
    #[serde(with = "time::serde::rfc3339")]
    pub granule_start: OffsetDateTime,
    pub inputs: Vec<InputQuantity>,
}

/// One raw input type's folded quantity in a granule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputQuantity {
    pub usage_type_ref: String,
    pub q: ExactQuantity,
}

/// The other members of a window group, captured once by the pipeline (T-D-67).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupContext {
    pub window_group_id: Uuid,
    pub members: Vec<GroupMember>,
    pub prefix_before_this_member: ExactQuantity,
    pub group_total: ExactQuantity,
    /// Hex digest of the stored context.
    pub context_digest: String,
}

/// One member of a window group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMember {
    pub child_id: Uuid,
    pub fact_id: Uuid,
    pub fact_version: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub served_from: OffsetDateTime,
    pub layout_version: u64,
    pub slices: Vec<MemberSlice>,
}

/// One slice quantity of a window-group member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSlice {
    #[serde(with = "time::serde::rfc3339")]
    pub slice_start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub slice_end: OffsetDateTime,
    pub q: ExactQuantity,
    pub q_version: u64,
}

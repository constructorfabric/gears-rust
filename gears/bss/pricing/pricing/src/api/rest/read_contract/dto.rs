//! The consumer read contract's wire shapes (D-419…D-422): `snake_case`, money and quantities as
//! exact decimal text, dates as `YYYY-MM-DD`. The consumer goldens freeze them. A closed set is
//! its `enum` (D-439), with the tokens the goldens carry.
use crate::api::rest::closed_sets::{
    PricingChargeKind, PricingEligibility, PricingModel, PricingPeriod, PricingPinnedPriceStatus,
    PricingResolveSource, PricingResolvedRevisionState,
};
use uuid::Uuid;

/// `GET /resolve`: one plan revision resolved on one date with the caller's pins (D-419). It
/// carries no totals and no promotion (D-409, D-415).
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveDto {
    pub plan_revision_id: Uuid,
    pub plan_id: Uuid,
    pub rev_no: i32,
    /// The revision's state as it reads today (D-447): `published`, `superseded`, or `scheduled`
    /// for a revision asked on or after its sale date while that date has not come yet (D-454). No
    /// other revision resolves.
    pub state: PricingResolvedRevisionState,
    /// The revision's book.
    pub book_id: Uuid,
    /// The book's currency.
    pub currency: String,
    /// The currency's scale: the number of minor digits money carries.
    pub currency_minor_digits: u32,
    /// The tenant's `default_rounding`, as stored: a string, since no CHECK guards the column
    /// (D-437, D-439).
    pub rounding_policy: String,
    /// The date resolved, `YYYY-MM-DD`.
    pub date: String,
    /// Every item of the revision, or the one `item_id` names.
    pub items: Vec<PricingResolveItemDto>,
}
/// One item of the revision on the date: its SKU and entry (D-467), its SKU version and resolved
/// invoice inputs (D-421), and its chain matrix (D-420).
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveItemDto {
    /// Immutable policy projected from the selected entry; null for legacy entries.
    pub usage_rating_policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicy>,
    pub item_id: Uuid,
    pub sku_id: Uuid,
    /// Null for a legacy item stored without an entry (D-467), which has no chains.
    pub price_book_entry_id: Option<Uuid>,
    /// Null without an entry.
    pub charge_kind: Option<PricingChargeKind>,
    pub period: Option<PricingPeriod>,
    /// The entry's model (D-427), the model of every binding's money; null without an entry.
    pub model: Option<PricingModel>,
    /// The SKU version in force on the date; null when Products has no version on that date or
    /// does not know the SKU.
    pub sku_version: Option<PricingResolveSkuVersionDto>,
    /// The entry's override, else the SKU version's template, else the tenant template for the
    /// charge kind.
    pub invoice_line_template: PricingResolveInputDto,
    /// The SKU version's, else the tenant `default_gl`.
    pub gl_code: PricingResolveInputDto,
    /// The SKU version's, else the tenant `default_tax_category`.
    pub tax_category: PricingResolveInputDto,
    /// The SKU version's, else the tenant `default_timing` (PRD AC #13).
    pub billing_timing: PricingResolveInputDto,
    /// The meter of that SKU version; both null without a version.
    pub meter: PricingResolveMeterDto,
    /// The default chain, then each value registered today in the registry's order, then any
    /// other value a pin names.
    pub chains: Vec<PricingResolveChainDto>,
}
/// The SKU version an item reads on the date.
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveSkuVersionDto {
    pub published_version: i64,
    /// `YYYY-MM-DD`.
    pub effective_from: String,
}
/// A resolved invoice input and where it came from; both null when no source holds it.
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveInputDto {
    pub value: Option<String>,
    pub source: Option<PricingResolveSource>,
}
/// The meter of the SKU version in force on the date.
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveMeterDto {
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
}
/// One row of an item's matrix: the default chain (`dim_value` null) or one value.
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveChainDto {
    pub dim_value: Option<String>,
    /// Neither the value's own chain nor the default binds on the date: `binding` is null. Never
    /// a refusal and never an invented price.
    pub uncovered: bool,
    pub binding: Option<PricingResolveBindingDto>,
}
/// The price bound for the period, as stored.
#[toolkit_macros::api_dto(response)]
pub struct PricingResolveBindingDto {
    pub price_id: Uuid,
    /// The chain the bound price belongs to: the value, or null for the default chain.
    pub dim_used: Option<String>,
    /// The pin the renewal walk started from; null for a signup.
    pub pinned_from: Option<Uuid>,
    /// The price's money as stored, in the item's model (D-427), amounts as exact decimal text.
    pub price: serde_json::Value,
    /// Exact decimal text.
    pub min_fee: Option<String>,
    pub eligibility: PricingEligibility,
    pub effective_from: String,
    /// The stored window's end, for information: a successor's start sets it, including a `new`
    /// successor a pinned subscription does not take, so it is not an end for the binding.
    pub effective_to: Option<String>,
    pub temporary_until: Option<String>,
    /// Where the binding ends for its holder (D-425): `temporary_until` for a temporary price, the
    /// end of an explicitly closed price, null when it has none. A consumer slices a period here,
    /// never at `effective_to`.
    pub ends_on: Option<String>,
    /// The price is kept for pinned subscriptions (the predecessor of a `new` price).
    pub keep_for_bound: bool,
}
/// `GET /prices/{id}`: one approved price as stored, served forever whatever its window (D-422),
/// with its entry's SKU, charge kind and period and its book's currency, or a cancelled one
/// (D-520). Stored facts only: no value computed from today, and no authoring internals.
#[toolkit_macros::api_dto(response)]
pub struct PricingPinnedPriceDto {
    pub price_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub sku_id: Uuid,
    pub charge_kind: PricingChargeKind,
    pub period: Option<PricingPeriod>,
    pub book_id: Uuid,
    pub currency: String,
    /// The price's place in its entry's order of prices.
    pub version_no: i32,
    /// The value's chain; null for the default chain.
    pub dim_value: Option<String>,
    /// Its entry's model (D-427).
    pub model: PricingModel,
    /// The price's money as stored, amounts as exact decimal text.
    pub price: serde_json::Value,
    /// Exact decimal text.
    pub min_fee: Option<String>,
    pub eligibility: PricingEligibility,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub temporary_until: Option<String>,
    /// The price is kept for pinned subscriptions (the predecessor of a `new` price).
    pub keep_for_bound: bool,
    /// The window was ended explicitly rather than by a successor's start.
    pub closed_explicitly: bool,
    /// A temporary price's return partner, or the price a return partner restores.
    pub paired_price_id: Option<Uuid>,
    pub return_of_price_id: Option<Uuid>,
    /// The approval unit that applied the price.
    pub approved_by_unit_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub approved_at: Option<time::OffsetDateTime>,
    /// `cancelled` for a price cancelled before it started, the entry list's token (D-520): a
    /// stored fact, the same on every day. Absent on an approved price, whose display status
    /// depends on the day, which this read never computes (D-422). Its own one-value set, so the
    /// schema offers nothing the read never serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PricingPinnedPriceStatus>,
}
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PricingResolveQuery {
    pub plan_revision_id: Option<String>,
    pub date: Option<String>,
    pub item_id: Option<String>,
    pub pins: Option<String>,
}

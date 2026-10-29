//! SDK-shaped port request and answer types for the gate's outbound reads (Task 6a, Amendment A).
//!
//! Names come from the open PRs: Rating #5390/#5175 (evaluation, `ExactMoney`) and Subscriptions
//! #5131 (overlap key, occupancy). Items marked "(proposed)" exist in no PR yet and are renamed
//! when the real SDK lands; one adapter file per port is the swap seam. Types only: no logic
//! beyond serde validation and small constructors.
use super::pricing_map::SelectedItem;
use super::vector::ProducerResult;
use bss_money::exact::ExactAmount;
use bss_orders_sdk::gate_report::{CatalogSelection, SelectionError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, OffsetDateTime};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// `YYYY-MM-DD` spelling of a [`Date`] on the wire.
pub(crate) mod iso_date {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _, ser::Error as _};
    use time::{Date, format_description::well_known::Iso8601};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` signature takes `&T`"
    )]
    pub fn serialize<S: Serializer>(d: &Date, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.format(&Iso8601::DATE).map_err(S::Error::custom)?)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Date, D::Error> {
        let text = String::deserialize(d)?;
        Date::parse(&text, &Iso8601::DATE).map_err(D::Error::custom)
    }
}

// ---- identity-profile (D-199, D-215; no PR) ----

/// Query of the payer's identity profile (D-199, D-215; no PR, proposed).
#[domain_model]
#[expect(
    clippy::struct_field_names,
    reason = "the three axes are named by their tenant role"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerProfileQuery {
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
    /// The tenant that owns the resource.
    pub resource_tenant_id: Uuid,
}

/// The payer's profile facts (D-199, D-215; no PR, proposed).
#[domain_model]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent profile facts, as the identity answer states them"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerProfile {
    /// The seller, payer and resource axes are valid together.
    pub axes_valid: bool,
    /// The payer's currency.
    pub currency: String,
    /// The payer's region.
    pub region: String,
    /// The payer is in a relationship with the seller.
    pub in_relationship_with_seller: bool,
    /// The payer is in a relationship with the resource tenant.
    pub in_relationship_with_resource: bool,
    /// The profile version the answer reflects.
    pub profile_version: String,
}

// ---- contract-resolution (D-132, D-199; no PR) ----

/// Query of one contract for a payer and seller (D-132, D-199; no PR, proposed).
#[domain_model]
#[expect(clippy::struct_field_names, reason = "SDK-shaped identity fields")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractQuery {
    /// The contract asked about.
    pub contract_id: Uuid,
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
}

/// A contract's status (D-132; no PR, proposed).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    /// In force.
    Active,
    /// Not in force.
    Inactive,
}

/// Facts of one resolved contract (D-132, D-199; no PR, proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractFacts {
    /// Whether the contract is in force.
    pub status: ContractStatus,
    /// The payer is eligible under the contract.
    pub payer_eligible: bool,
    /// When the contract takes effect, if stated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub effective_at: Option<OffsetDateTime>,
    /// Whether the contract requires acceptance, if stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_required: Option<bool>,
    /// The contract version the answer reflects.
    pub version: String,
}

// ---- overlap-key (SUB-G1 names from #5131; operation proposed) ----

/// One line asked about for its overlap key (#5131 SUB-G1 names; operation proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlapKeyLine {
    /// The order line.
    pub line_id: Uuid,
    /// The plan revision the line sells.
    pub plan_revision_id: Uuid,
    /// The paid recurring SKU of the line.
    pub paid_recurring_sku: Uuid,
    /// Exact requested Pricing selection, including fallback requests.
    pub item_id: Uuid,
    /// Requested dimension, not the resolved default binding dimension.
    pub dimension_value: Option<String>,
    /// Accepted SKU version.
    pub sku_version: i64,
}

/// The overlap key of one line (#5131 SUB-G1 names; operation proposed). Unknown members
/// refuse (`SUBSCRIPTIONS_CONTRACT.md`): a misspelled `item_key` never decodes as a legacy echo.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlapKey {
    /// The order line.
    pub line_id: Uuid,
    /// The catalog subscription product key (#5131).
    pub catalog_subscription_product_key: String,
    /// The overlap scope key (#5131).
    pub overlap_scope_key: String,
    /// Complete proposed profile; absence is explicitly legacy evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_key: Option<super::item_keys::ItemOverlapKey>,
}

// ---- occupancy (#5131 getOverlapOccupancy) ----

/// Query of one overlap scope's occupancy (#5131 `getOverlapOccupancy`; the resource tenant
/// follows the D-198 product direction).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccupancyQuery {
    /// Exact reusable reservation context; absent for Submit and Preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_context: Option<super::protected::OwnContext>,
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The resource tenant, when the scope is resource-bound (D-198).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_tenant_id: Option<Uuid>,
    /// The overlap scope key.
    pub overlap_scope_key: String,
}

/// Where a concurrency limit came from (#5131).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitProvenance {
    /// The catalog set it.
    Catalog,
    /// The contract set it.
    Contract,
    /// The platform default applies.
    PlatformDefault,
}

/// The maximum concurrent active subscriptions in a scope (#5131).
///
/// Wire form (proposed encoding, #5131 leaves it unspecified): `Limit(n)` is the number `n`,
/// `Unlimited` is `null`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxConcurrentActive {
    /// At most this many.
    Limit(u32),
    /// No limit.
    Unlimited,
}

impl Serialize for MaxConcurrentActive {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Limit(n) => Some(*n),
            Self::Unlimited => None,
        }
        .serialize(s)
    }
}

/// Reads `null` as [`MaxConcurrentActive::Unlimited`] and an unsigned number as the limit.
struct LimitVisitor;

impl serde::de::Visitor<'_> for LimitVisitor {
    type Value = MaxConcurrentActive;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a non-negative integer limit, or null for unlimited")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(MaxConcurrentActive::Unlimited)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(MaxConcurrentActive::Unlimited)
    }

    fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Self::Value, E> {
        u32::try_from(n)
            .map(MaxConcurrentActive::Limit)
            .map_err(|_| E::invalid_value(serde::de::Unexpected::Unsigned(n), &self))
    }
}

/// `deserialize_any`, not `deserialize_option`: a serde derive treats a missing member of an
/// `Option` as `None`, but a missing member reaching `deserialize_any` is an error. So the type
/// itself refuses an absent limit, with no per-field attribute; only an explicit `null` is
/// unlimited.
impl<'de> Deserialize<'de> for MaxConcurrentActive {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(LimitVisitor)
    }
}

/// Scope of `active_count` (proposed development mirror; producer agreement pending).
/// Independent of the authority that supplied the concurrency limit.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccupancyCountScope {
    /// Count across this payer's resource tenants for the key.
    PerPayer,
    /// Count for the complete payer/resource tenant/key tuple.
    FullTuple,
}

/// The occupancy of one overlap scope (#5131 `getOverlapOccupancy`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occupancy {
    /// Active subscriptions counted in the scope.
    pub active_count: u32,
    /// Explicit count scope; missing or unknown scope is malformed.
    pub count_scope: OccupancyCountScope,
    /// The scope's limit, required even when explicitly unlimited (`null`).
    pub max_concurrent_active: MaxConcurrentActive,
    /// Where the limit came from.
    pub provenance: LimitProvenance,
}

// ---- billing-terms (D-193; absent from #5131, ask raised; proposed) ----

/// The caller's term intent for a line (D-193; proposed).
///
/// Wire form: externally tagged, `snake_case` (`{"fixed":{"months":12}}`, `"rolling"`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermIntentInput {
    /// A fixed number of months.
    Fixed {
        /// The months.
        months: u32,
    },
    /// Continues until ended.
    Rolling,
    /// The seller's policy decides.
    SellerPolicy,
}

/// Query to resolve one line's billing terms (D-193; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingTermsQuery {
    /// The order line.
    pub line_id: Uuid,
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
    /// The plan revision the line sells.
    pub plan_revision_id: Uuid,
    /// The requested term intent.
    pub intent: TermIntentInput,
    /// The contract in play, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<Uuid>,
}

/// Where resolved terms came from (D-193; proposed).
///
/// Wire form: externally tagged, `snake_case` (`"explicit_order"`, `{"contract":{..}}`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermsProvenance {
    /// The order stated them.
    ExplicitOrder,
    /// A contract stated them.
    Contract {
        /// The contract.
        contract_id: Uuid,
        /// The contract version.
        version: String,
    },
    /// The seller's policy stated them.
    SellerPolicy {
        /// The policy version.
        version: String,
    },
    /// The platform default applies.
    PlatformDefault,
}

/// Reference to the resolved billing terms snapshot by its canonical digest.
///
/// `bss_pricing_sdk::terms::BillingTerms` is public but carries no serde, so the gate's answer
/// holds the snapshot's `digest` here and its content in [`ResolvedTerms::snapshot`]
/// (D-193; proposed). The digest is the snapshot's `bss.billing-terms.v1` digest as 64 lowercase
/// hexadecimal digits, the spelling the schema-2 receipt codec writes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingTermsRef {
    /// The snapshot's canonical digest.
    pub digest: String,
}

/// The anchor convention of a resolved terms snapshot; mirrors
/// `bss_pricing_sdk::terms::BillingAnchor` (D-193; proposed).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermsAnchor {
    /// First day of the calendar month or year at midnight UTC.
    Calendar,
    /// The explicit subscription anniversary.
    SubscriptionStart,
}

/// The provenance a resolved terms snapshot states; mirrors
/// `bss_pricing_sdk::terms::TermsSource` (D-193; proposed). Wire form: internally tagged by
/// `kind`, as the schema-2 receipt codec writes it (`{"kind":"explicit_order"}`,
/// `{"kind":"seller_policy","id":..,"version":3}`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TermsSnapshotSource {
    /// The order's explicit intent.
    ExplicitOrder,
    /// An immutable seller policy version.
    SellerPolicy {
        /// The policy identity.
        id: Uuid,
        /// The positive policy version.
        version: u64,
    },
}

/// The content of a resolved billing terms snapshot, as Subscriptions states it (D-193;
/// proposed): every member of `bss_pricing_sdk::terms::BillingTerms` except its digest, which
/// [`BillingTermsRef`] carries. Submit freezes it into each line's Pricing request; nothing here
/// is defaulted by Orders.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingTermsSnapshot {
    /// The snapshot schema version (the new-sale snapshot is version 1).
    pub schema_version: u32,
    /// The invoice cycle.
    pub cycle: EvaluationCycle,
    /// The anchor convention.
    pub anchor: TermsAnchor,
    /// The resolved anchor instant, preserved without normalization.
    #[serde(with = "time::serde::rfc3339")]
    pub anchor_at: OffsetDateTime,
    /// The commercial timezone, as its IANA spelling (Pricing accepts `UTC` only).
    pub timezone: String,
    /// The snapshot's provenance.
    pub source: TermsSnapshotSource,
}

/// One line's resolved billing terms (D-193; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTerms {
    /// The order line.
    pub line_id: Uuid,
    /// The resolved billing terms snapshot.
    pub billing_terms: BillingTermsRef,
    /// The resolved term (same shape as `bss_pricing_sdk::acceptance::Term`, which has no serde).
    pub term: EvaluationTerm,
    /// The earliest start the terms allow, if any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub earliest_start: Option<OffsetDateTime>,
    /// Where the terms came from.
    pub provenance: TermsProvenance,
    /// The snapshot's content, when the port states it. The gate checks its cycle against
    /// evaluation and recurring bindings, and refuses `billing-terms-unsupported` for answered
    /// terms without a snapshot or with one Pricing cannot express (D-236 item 2 as amended by
    /// D-242); submit freezes the line's Pricing request from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<BillingTermsSnapshot>,
}

// ---- acceptance-policy (D-191; no PR; proposed) ----

/// Query of a seller's acceptance policy (D-191; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptancePolicyQuery {
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
}

/// A seller's acceptance policy (D-191; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptancePolicy {
    /// The policy version.
    pub version: String,
    /// How long an acceptance hold lasts, in seconds.
    pub hold_seconds: u32,
}

// ---- assessment (D-189; no PR; proposed) ----

/// Query of the catalog assessment (D-189; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentQuery {
    /// The assessment identity.
    pub assessment_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
    /// The date the catalog resolves on.
    #[serde(with = "iso_date")]
    pub resolve_date: Date,
    /// The producer profile asked for.
    pub profile: String,
    /// The lines assessed.
    pub lines: Vec<AssessmentLine>,
}

/// One line of an assessment query (D-189; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentLine {
    /// The order line.
    pub line_id: Uuid,
    /// The pricing plan.
    pub plan_id: Uuid,
    /// The pricing revision.
    pub revision_id: Uuid,
    /// The selected slots.
    pub selections: Vec<SelectedItem>,
    /// The market, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<Market>,
    /// The term, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<EvaluationTerm>,
    /// The billing cycle, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<EvaluationCycle>,
}

/// The assessment's answer, in the `ProducerResult` vocabulary of D-195 (D-189; proposed).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentAnswer {
    /// The profile that answered.
    pub profile: String,
    /// When the producer observed its inputs.
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// The per-line, per-item results.
    pub results: Vec<ProducerResultWithKey>,
}

/// A producer result with the identity needed to key it into the outcome vector (D-189, D-195).
/// Its serde form is [`ProducerResultWithKeyWire`]: the selection as the two flat members, so a
/// result keyed without a selection but with a scope key never decodes (malformed coverage).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "ProducerResultWithKeyWire",
    into = "ProducerResultWithKeyWire"
)]
pub struct ProducerResultWithKey {
    /// The order line.
    pub line_id: Uuid,
    /// The plan item, for an item-scoped result.
    pub item_id: Option<Uuid>,
    /// The catalog selection the result keys (a selected NULL counts as selected).
    pub selection: CatalogSelection,
    /// The producer's result.
    pub result: ProducerResult,
}

/// Wire form of [`ProducerResultWithKey`]: today's members, the selection as two.
#[derive(Serialize, Deserialize)]
struct ProducerResultWithKeyWire {
    line_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_id: Option<Uuid>,
    has_catalog_selection: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    catalog_scope_key: Option<String>,
    result: ProducerResult,
}
impl TryFrom<ProducerResultWithKeyWire> for ProducerResultWithKey {
    type Error = SelectionError;
    fn try_from(w: ProducerResultWithKeyWire) -> Result<Self, Self::Error> {
        Ok(Self {
            line_id: w.line_id,
            item_id: w.item_id,
            selection: CatalogSelection::from_parts(w.has_catalog_selection, w.catalog_scope_key)?,
            result: w.result,
        })
    }
}
impl From<ProducerResultWithKey> for ProducerResultWithKeyWire {
    fn from(r: ProducerResultWithKey) -> Self {
        let (has_catalog_selection, catalog_scope_key) = r.selection.into_parts();
        Self {
            line_id: r.line_id,
            item_id: r.item_id,
            has_catalog_selection,
            catalog_scope_key,
            result: r.result,
        }
    }
}

// ---- evaluation (#5175 names; #5390 ExactMoney shape) ----

/// A market: currency and region (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Market {
    /// The currency code.
    pub currency: String,
    /// The region.
    pub region: String,
}

/// A term in invoice periods (#5175; same shape as `bss_pricing_sdk::acceptance::Term`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationTerm {
    /// Continues until ended.
    Rolling,
    /// A positive number of invoice periods.
    FixedPeriods {
        /// The period count.
        count: u32,
    },
}

/// The invoice cycle an evaluation prices (#5175).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationCycle {
    /// Monthly.
    Month,
    /// Yearly.
    Year,
}

/// Why an [`ExactMoney`] value is rejected.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExactMoneyError {
    /// The numerator is not an optionally negative ASCII integer without leading zeros.
    #[error("numerator is not a canonical integer")]
    Numerator,
    /// The denominator is not a positive ASCII integer without leading zeros.
    #[error("denominator is not a canonical positive integer")]
    Denominator,
    /// Both parts are canonical, but the fraction is not in lowest terms.
    #[error("fraction is not reduced")]
    NotReduced,
}

/// An exact fraction of a currency's major unit; mirrors `bss_rating_sdk::ExactMoney` (#5390).
///
/// The fields are private: [`ExactMoney::new`] and deserialization are the only constructors and
/// both validate the fraction with `bss_money::exact::ExactAmount::parse_fraction`, the money
/// ADR's canonical fraction text (rule 6) that Rating writes (Rating ADR-0005, T-D-80).
/// `currency` and `currency_scale` are not validated here. Both parts are decimal text.
/// Deserialization rejects exponents, JSON numbers, signs other than a leading `-` on the
/// numerator, leading zeros, empty text, a zero or negative denominator and a fraction that is
/// not in lowest terms. This mirror is replaced by the SDK type when Rating lands.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ExactMoneyWire")]
pub struct ExactMoney {
    numerator: String,
    denominator: String,
    currency: String,
    currency_scale: u8,
}

/// Unvalidated wire form of [`ExactMoney`].
#[derive(Deserialize)]
struct ExactMoneyWire {
    numerator: String,
    denominator: String,
    currency: String,
    currency_scale: u8,
}

impl ExactMoney {
    /// Build a validated value (same rules as deserialization).
    ///
    /// # Errors
    /// [`ExactMoneyError`] when a part is not canonical integer text, the denominator is zero or
    /// negative, or the fraction is not reduced.
    pub fn new(
        numerator: &str,
        denominator: &str,
        currency: &str,
        currency_scale: u8,
    ) -> Result<Self, ExactMoneyError> {
        // Each part alone over a unit fraction names the bad part; together they are only
        // refused for a common factor.
        if ExactAmount::parse_fraction(numerator, "1").is_err() {
            return Err(ExactMoneyError::Numerator);
        }
        if ExactAmount::parse_fraction("1", denominator).is_err() {
            return Err(ExactMoneyError::Denominator);
        }
        if ExactAmount::parse_fraction(numerator, denominator).is_err() {
            return Err(ExactMoneyError::NotReduced);
        }
        Ok(Self {
            numerator: numerator.to_owned(),
            denominator: denominator.to_owned(),
            currency: currency.to_owned(),
            currency_scale,
        })
    }

    /// Canonical integer text, optional leading `-`.
    #[must_use]
    pub fn numerator(&self) -> &str {
        &self.numerator
    }

    /// Canonical positive integer text.
    #[must_use]
    pub fn denominator(&self) -> &str {
        &self.denominator
    }

    /// The currency code.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// The currency's minor-unit scale.
    #[must_use]
    pub fn currency_scale(&self) -> u8 {
        self.currency_scale
    }
}

impl TryFrom<ExactMoneyWire> for ExactMoney {
    type Error = ExactMoneyError;
    fn try_from(w: ExactMoneyWire) -> Result<Self, Self::Error> {
        Self::new(&w.numerator, &w.denominator, &w.currency, w.currency_scale)
    }
}

/// An exact amount with its display text (#5390).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    /// The exact value.
    pub exact: ExactMoney,
    /// The display text.
    pub display: String,
}

/// An amount that may be unavailable (#5175). Wire form (picked here): internally tagged by
/// `status`, `{"status":"available","exact":..,"display":..}` or `{"status":"unavailable"}`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Availability {
    /// The amount is known.
    Available(Amount),
    /// The amount is not available.
    Unavailable,
}

/// The exact accepted native binding plus the legacy projected identity (#5175; proposed).
///
/// Rating evaluates `native_binding` without resolving successors or evaluating money in
/// Orders (D-197). The existing Orders SDK receipt codec supplies its complete lossless shape;
/// legacy projected fields remain readable for stored attempts. Producer digests use lowercase
/// hexadecimal text, copied without recomputation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationBinding {
    /// Complete native binding in the Orders SDK schema-2 receipt spelling. Absent only in
    /// legacy snapshots; freshly built requests carry every model, meter, invoice and policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_binding: Option<serde_json::Value>,
    /// The plan item.
    pub item_id: Uuid,
    /// The price book entry.
    pub price_book_entry_id: Uuid,
    /// The accepted price (`ImmutablePrice.price_id`).
    pub price_id: Uuid,
    /// The price's financial-content SHA-256 (`ImmutablePrice.money_digest`) as lowercase hex.
    pub money_digest: String,
    /// The price's currency code (`ImmutablePrice.currency.code`).
    pub currency: String,
    /// The price's currency scale (`ImmutablePrice.currency.scale`).
    pub currency_scale: u8,
    /// The SKU.
    pub sku_id: Uuid,
    /// The SKU version.
    pub sku_version: i64,
    /// The dimension key, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension_key: Option<String>,
    /// The dimension value, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension_value: Option<String>,
    /// The charge kind (`AcceptedBinding.kind`).
    pub kind: EvaluationChargeKind,
    /// The recurring period, for a recurring binding (`AcceptedBinding.recurring_period`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_period: Option<EvaluationCycle>,
    /// The requested value was covered by the default chain (evidence only).
    pub via_default: bool,
}

/// The kind of charge a binding prices; mirrors `bss_pricing_sdk::read::ChargeKind`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationChargeKind {
    /// Charged every period.
    Recurring,
    /// Charged on metered usage.
    Usage,
    /// Charged once.
    OneTime,
}
impl EvaluationChargeKind {
    /// The stored and wire token (`charge_kind`; the serde spelling).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Recurring => "recurring",
            Self::Usage => "usage",
            Self::OneTime => "one_time",
        }
    }
    /// The kind a stored token names, if any.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::Recurring, Self::Usage, Self::OneTime]
            .into_iter()
            .find(|k| k.token() == token)
    }
}

/// One accepted pricing chain of an item (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationChain {
    /// The dimension value of the chain; `None` is the default chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim_value: Option<String>,
    /// The accepted binding.
    pub binding: EvaluationBinding,
}

/// One item of an evaluation line (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationItem {
    /// The plan item.
    pub item_id: Uuid,
    /// Quantity as `bss_money` canonical decimal text (D-216).
    pub quantity: String,
    /// The accepted chains.
    pub chains: Vec<EvaluationChain>,
}

/// One line of an evaluation request (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationLine {
    /// Exact plan identity; absent in legacy request snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    /// Actual resolved service activation start, independent of earliest-start constraints.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub start_at: Option<OffsetDateTime>,
    /// Complete resolver answer, including `BillingTerms` snapshot, original digest and provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_terms: Option<ResolvedTerms>,
    /// The order line.
    pub line_id: Uuid,
    /// The plan revision priced.
    pub plan_revision_id: Uuid,
    /// The term priced.
    pub term: EvaluationTerm,
    /// The cycle priced.
    pub cycle: EvaluationCycle,
    /// The items priced.
    pub items: Vec<EvaluationItem>,
}

/// The order evaluation request (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderEvaluationRequest {
    /// Independently authorized order axes; absent in legacy request snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_axes: Option<super::orchestrate::TenantAxes>,
    /// Assessment instant captured before the run's external calls.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub assessed_at: Option<OffsetDateTime>,
    /// Actual seller acceptance policy when answered, including its authoritative version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_policy: Option<AcceptancePolicy>,
    /// The assessment this evaluation follows (D-189).
    pub assessment_id: Uuid,
    /// The date prices resolve on.
    #[serde(with = "iso_date")]
    pub resolve_date: Date,
    /// The market.
    pub market: Market,
    /// The contract in play, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<Uuid>,
    /// The lines.
    pub lines: Vec<EvaluationLine>,
}

/// Gross, net and discount totals (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Gross total.
    pub gross: Amount,
    /// Net total.
    pub net: Amount,
    /// Discount, when evaluable.
    pub discount: Availability,
}

/// The producer's storage declaration of one committed totals component (DESIGN §3.7
/// `orders_resolved_total`, D-197, D-216): its charge kind, monetary horizon and evidence, item
/// breakdown, labelled recurring cycles and exclusions, copied verbatim into the row.
///
/// Proposed addition to the #5175 mirror (D-231 note): Orders neither derives a horizon nor
/// converts cycles, so an evaluation without these declarations is unusable for submit
/// (`evaluation-unavailable`), never defaulted. Preview copies declared exclusions when available.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TotalsDeclaration {
    /// The charge kind of the component: `recurring` or `one_time` for a committed component,
    /// `usage` for a usage declaration.
    pub charge_kind: EvaluationChargeKind,
    /// The producer-declared monetary horizon (`amount_basis`).
    pub amount_basis: String,
    /// Line/term/cycle evidence of that horizon (`period_evidence`).
    pub period_evidence: serde_json::Value,
    /// Producer item amounts (`item_breakdown`).
    pub item_breakdown: serde_json::Value,
    /// Labelled recurring cycles (`recurring_by_cycle`).
    pub recurring_by_cycle: serde_json::Value,
    /// Explicit usage/tax/unsupported-overlay exclusions (`exclusions`).
    pub exclusions: serde_json::Value,
}

/// Producer-declared committed money and its storage facts (proposed D-197 mirror).
/// Orders copies each component; aggregate Preview/tax totals remain independent.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedComponent {
    /// Producer-calculated component amounts.
    pub totals: Totals,
    /// Required component kind, horizon, evidence and exclusions.
    pub declaration: TotalsDeclaration,
}

/// One line's evaluation (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineEvaluation {
    /// The order line.
    pub line_id: Uuid,
    /// The line totals.
    pub totals: Totals,
    /// Explicit committed components: absent uses the legacy single declaration; an empty
    /// list states no committed charges (usage-only). Never derived from aggregate totals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_components: Option<Vec<CommittedComponent>>,
    /// Usage charges exist and are not in the totals.
    pub usage_flagged: bool,
    /// The storage declaration of the line's committed component (proposed; submit needs it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<TotalsDeclaration>,
    /// The storage declaration of the line's usage component (charge kind `usage`): present
    /// exactly when `usage_flagged` (proposed; submit writes its uncommitted usage row from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_declaration: Option<TotalsDeclaration>,
}

/// The valuation horizon a TCV declares (`tcv_basis`; DESIGN §3.7).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcvBasis {
    /// Finite accepted terms.
    FiniteTerm,
    /// Annualized rolling terms.
    RollingAnnualized,
    /// The explicitly labelled mixed approval basis.
    Mixed,
}
impl TcvBasis {
    /// The stored and wire token (`tcv_basis`; the serde spelling).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::FiniteTerm => "finite_term",
            Self::RollingAnnualized => "rolling_annualized",
            Self::Mixed => "mixed",
        }
    }
}

/// Total contract value (#5175, #5390).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tcv {
    /// The exact value.
    pub exact: ExactMoney,
    /// The display text.
    pub display: String,
    /// The value as posting would round it, decimal text.
    pub posting_estimate: String,
    /// The declared valuation horizon (proposed; submit needs it, D-231 note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<TcvBasis>,
    /// Per-line basis and producer policy version (`tcv_evidence`; proposed, as `basis`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
}

/// The order evaluation answer (#5175).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderEvaluation {
    /// The assessment this evaluation followed.
    pub assessment_id: Uuid,
    /// The figures are an estimate.
    pub estimate: bool,
    /// The evaluation currency.
    pub currency: String,
    /// The currency's minor-unit scale.
    pub currency_scale: u8,
    /// Per-line evaluations.
    pub lines: Vec<LineEvaluation>,
    /// Order totals.
    pub order_totals: Totals,
    /// Explicit order components; absent preserves the legacy single-component contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_committed_components: Option<Vec<CommittedComponent>>,
    /// Producer-supplied order/recurring carrier when no recurring order component exists.
    /// Both its amounts and declaration are required; Orders never fabricates zero money.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_tcv_carrier: Option<CommittedComponent>,
    /// The storage declaration of the order roll-up (proposed; submit needs it, D-231 note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_declaration: Option<TotalsDeclaration>,
    /// The storage declaration of the order roll-up's usage component: present exactly when a
    /// line carries usage charges (proposed, as `order_declaration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_usage_declaration: Option<TotalsDeclaration>,
    /// Total contract value, when computable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcv: Option<Tcv>,
    /// Whether promotions were evaluated.
    pub promotion_status: Availability,
    /// Digest of the request this answers.
    pub request_digest: String,
}

// ---- indicative-tax (unowned; proposed minimal) ----

/// An amount stated for one line: a named object, not a `(line, amount)` pair, so the wire
/// reads `{"line_id": .., "amount": {..}}` (the shape of the other per-line answers).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineAmount {
    /// The line.
    pub line_id: Uuid,
    /// Its amount.
    pub amount: Amount,
}

/// Wire form of [`LineAmount`], a map-only decode: a derived `Deserialize` would also accept
/// the legacy `[line, amount]` sequence, which must stay refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineAmountWire {
    line_id: Uuid,
    amount: Amount,
}

impl<'de> Deserialize<'de> for LineAmount {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = LineAmount;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object {line_id, amount}")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<LineAmount, A::Error> {
                let w =
                    LineAmountWire::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(LineAmount {
                    line_id: w.line_id,
                    amount: w.amount,
                })
            }
        }
        d.deserialize_map(V)
    }
}

/// Query of an indicative tax estimate (unowned; proposed minimal).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxQuery {
    /// The paying tenant.
    pub payer_tenant_id: Uuid,
    /// The selling tenant.
    pub seller_tenant_id: Uuid,
    /// The taxable amount per line.
    pub lines: Vec<LineAmount>,
}

/// An indicative tax estimate (unowned; proposed minimal).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndicativeTax {
    /// Tax per line.
    pub per_line: Vec<LineAmount>,
    /// Total tax.
    pub total: Amount,
    /// The provider stated explicitly that no tax applies.
    pub explicit_no_tax: bool,
}

/// Explicit legacy and proposed protected occupancy evidence.
pub use super::protected::OccupancyObservation;

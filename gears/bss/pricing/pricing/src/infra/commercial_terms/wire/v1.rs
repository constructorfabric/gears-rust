//! Schema-1 receipt and hold DTOs. A shape changes in place and keeps schema version 1 (D-532);
//! a stored field is never reinterpreted.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance:p1
use super::scalars;
use bss_pricing_sdk::{Digest, acceptance as a, plan_quota as q, read as r, terms as t};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "frozen contract tenant axis names"
)]
pub(super) struct TenantAxes {
    #[serde(with = "scalars::uuid")]
    pub seller_tenant_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub payer_tenant_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub resource_tenant_id: Uuid,
}
impl From<a::TenantAxes> for TenantAxes {
    fn from(v: a::TenantAxes) -> Self {
        Self {
            seller_tenant_id: v.seller_tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            resource_tenant_id: v.resource_tenant_id,
        }
    }
}
impl From<TenantAxes> for a::TenantAxes {
    fn from(v: TenantAxes) -> Self {
        Self {
            seller_tenant_id: v.seller_tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            resource_tenant_id: v.resource_tenant_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Market {
    pub currency: String,

    #[serde(deserialize_with = "scalars::required_option")]
    pub region: Option<String>,
}
impl From<a::Market> for Market {
    fn from(v: a::Market) -> Self {
        Self {
            currency: v.currency,
            region: v.region,
        }
    }
}
impl From<Market> for a::Market {
    fn from(v: Market) -> Self {
        Self {
            currency: v.currency,
            region: v.region,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, tag = "kind")]
pub(super) enum Term {
    Rolling,

    FixedPeriods {
        #[serde(with = "scalars::integer")]
        count: u32,
    },
}
impl From<a::Term> for Term {
    fn from(v: a::Term) -> Self {
        match v {
            a::Term::Rolling => Self::Rolling,
            a::Term::FixedPeriods { count } => Self::FixedPeriods { count },
        }
    }
}
impl From<Term> for a::Term {
    fn from(v: Term) -> Self {
        match v {
            Term::Rolling => Self::Rolling,
            Term::FixedPeriods { count } => Self::FixedPeriods { count },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewSaleQuery {
    pub tenant_axes: TenantAxes,
    #[serde(with = "scalars::uuid")]
    pub order_id: Uuid,

    #[serde(with = "scalars::integer")]
    pub order_version: u64,
    #[serde(with = "scalars::uuid")]
    pub line_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub plan_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub plan_revision_id: Uuid,

    pub selections: Vec<BindingSelection>,

    #[serde(with = "scalars::decimal")]
    pub quantity: Decimal,

    pub market: Market,

    #[serde(with = "scalars::instant")]
    pub start_at: OffsetDateTime,

    pub term: Term,

    pub billing_terms: BillingTerms,

    #[serde(with = "scalars::digest")]
    pub resolved_bindings_digest: Digest,

    #[serde(with = "scalars::integer")]
    pub hold_policy_version: u64,

    /// The preview's selected quota digest (D-548). Absent on a request without one, and omitted
    /// when absent, so a quota-free receipt keeps its bytes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "scalars::digest_present"
    )]
    pub expected_quota_digest: Option<Digest>,
}
impl From<a::NewSaleQuery> for NewSaleQuery {
    fn from(v: a::NewSaleQuery) -> Self {
        Self {
            tenant_axes: v.tenant_axes.into(),
            order_id: v.order_id,
            order_version: v.order_version,
            line_id: v.line_id,
            plan_id: v.plan_id,
            plan_revision_id: v.plan_revision_id,
            selections: v.selections.into_iter().map(Into::into).collect(),
            quantity: v.quantity,
            market: v.market.into(),
            start_at: v.start_at,
            term: v.term.into(),
            billing_terms: v.billing_terms.into(),
            resolved_bindings_digest: v.resolved_bindings_digest,
            hold_policy_version: v.hold_policy_version,
            expected_quota_digest: v.expected_quota_digest,
        }
    }
}
impl From<NewSaleQuery> for a::NewSaleQuery {
    fn from(v: NewSaleQuery) -> Self {
        Self {
            tenant_axes: v.tenant_axes.into(),
            order_id: v.order_id,
            order_version: v.order_version,
            line_id: v.line_id,
            plan_id: v.plan_id,
            plan_revision_id: v.plan_revision_id,
            selections: v.selections.into_iter().map(Into::into).collect(),
            quantity: v.quantity,
            market: v.market.into(),
            start_at: v.start_at,
            term: v.term.into(),
            billing_terms: v.billing_terms.into(),
            resolved_bindings_digest: v.resolved_bindings_digest,
            hold_policy_version: v.hold_policy_version,
            expected_quota_digest: v.expected_quota_digest,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcceptanceReceipt {
    #[serde(with = "scalars::uuid")]
    pub acceptance_id: Uuid,

    #[serde(with = "scalars::digest")]
    pub request_digest: Digest,

    #[serde(with = "scalars::digest")]
    pub terms_digest: Digest,

    pub query: NewSaleQuery,

    #[serde(with = "scalars::instant")]
    pub accepted_at: OffsetDateTime,

    #[serde(with = "scalars::instant")]
    pub hold_until: OffsetDateTime,

    pub bindings: Vec<AcceptedBinding>,

    /// The frozen quota evidence (D-548). Absent on a receipt without rules, and omitted when
    /// absent, so a quota-free receipt keeps its bytes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "scalars::present"
    )]
    pub quota: Option<AcceptedQuotaSnapshot>,
}
impl From<a::AcceptanceReceipt> for AcceptanceReceipt {
    fn from(v: a::AcceptanceReceipt) -> Self {
        Self {
            acceptance_id: v.acceptance_id,
            request_digest: v.request_digest,
            terms_digest: v.terms_digest,
            query: v.query.into(),
            accepted_at: v.accepted_at,
            hold_until: v.hold_until,
            bindings: v.bindings.into_iter().map(Into::into).collect(),
            quota: v.quota.map(Into::into),
        }
    }
}
impl TryFrom<AcceptanceReceipt> for a::AcceptanceReceipt {
    type Error = PriceSpecError;
    fn try_from(v: AcceptanceReceipt) -> Result<Self, Self::Error> {
        Ok(Self {
            acceptance_id: v.acceptance_id,
            request_digest: v.request_digest,
            terms_digest: v.terms_digest,
            query: v.query.into(),
            accepted_at: v.accepted_at,
            hold_until: v.hold_until,
            bindings: v
                .bindings
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            quota: v.quota.map(Into::into),
        })
    }
}

/// The frozen quota snapshot (D-548): digests as lowercase hex, versions and revisions as
/// canonical decimal text from 1, the quantity as exact text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcceptedQuotaSnapshot {
    #[serde(with = "scalars::digest")]
    pub selected_quota_digest: Digest,

    #[serde(with = "scalars::digest")]
    pub quota_snapshot_digest: Digest,

    #[serde(with = "scalars::decimal")]
    pub original_quantity: Decimal,

    pub rules: Vec<AcceptedQuotaRule>,
}
impl From<a::AcceptedQuotaSnapshot> for AcceptedQuotaSnapshot {
    fn from(v: a::AcceptedQuotaSnapshot) -> Self {
        Self {
            selected_quota_digest: v.selected_quota_digest,
            quota_snapshot_digest: v.quota_snapshot_digest,
            original_quantity: v.original_quantity,
            rules: v.rules.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<AcceptedQuotaSnapshot> for a::AcceptedQuotaSnapshot {
    fn from(v: AcceptedQuotaSnapshot) -> Self {
        Self {
            selected_quota_digest: v.selected_quota_digest,
            quota_snapshot_digest: v.quota_snapshot_digest,
            original_quantity: v.original_quantity,
            rules: v.rules.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcceptedQuotaRule {
    pub rule: ResolvedQuotaRule,

    pub limit: String,

    #[serde(with = "scalars::integer")]
    pub availability_revision: NonZeroU64,
}
impl From<a::AcceptedQuotaRule> for AcceptedQuotaRule {
    fn from(v: a::AcceptedQuotaRule) -> Self {
        Self {
            rule: v.rule.into(),
            limit: v.limit,
            availability_revision: v.availability_revision,
        }
    }
}
impl From<AcceptedQuotaRule> for a::AcceptedQuotaRule {
    fn from(v: AcceptedQuotaRule) -> Self {
        Self {
            rule: v.rule.into(),
            limit: v.limit,
            availability_revision: v.availability_revision,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResolvedQuotaRule {
    pub rule: PlanQuotaRule,

    pub evidence: PinnedQuotaEvidence,
}
impl From<r::ResolvedQuotaRule> for ResolvedQuotaRule {
    fn from(v: r::ResolvedQuotaRule) -> Self {
        Self {
            rule: v.rule.into(),
            evidence: v.evidence.into(),
        }
    }
}
impl From<ResolvedQuotaRule> for r::ResolvedQuotaRule {
    fn from(v: ResolvedQuotaRule) -> Self {
        Self {
            rule: v.rule.into(),
            evidence: v.evidence.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PlanQuotaBasis {
    Fixed,
    PerPlanUnit,
}
impl From<q::PlanQuotaBasis> for PlanQuotaBasis {
    fn from(v: q::PlanQuotaBasis) -> Self {
        match v {
            q::PlanQuotaBasis::Fixed => Self::Fixed,
            q::PlanQuotaBasis::PerPlanUnit => Self::PerPlanUnit,
        }
    }
}
impl From<PlanQuotaBasis> for q::PlanQuotaBasis {
    fn from(v: PlanQuotaBasis) -> Self {
        match v {
            PlanQuotaBasis::Fixed => Self::Fixed,
            PlanQuotaBasis::PerPlanUnit => Self::PerPlanUnit,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum QuotaSubjectSelector {
    ResourceOwner,
}
impl From<q::QuotaSubjectSelector> for QuotaSubjectSelector {
    fn from(v: q::QuotaSubjectSelector) -> Self {
        match v {
            q::QuotaSubjectSelector::ResourceOwner => Self::ResourceOwner,
        }
    }
}
impl From<QuotaSubjectSelector> for q::QuotaSubjectSelector {
    fn from(v: QuotaSubjectSelector) -> Self {
        match v {
            QuotaSubjectSelector::ResourceOwner => Self::ResourceOwner,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PlanQuotaKind {
    Allocation,
}
impl From<q::PlanQuotaKind> for PlanQuotaKind {
    fn from(v: q::PlanQuotaKind) -> Self {
        match v {
            q::PlanQuotaKind::Allocation => Self::Allocation,
        }
    }
}
impl From<PlanQuotaKind> for q::PlanQuotaKind {
    fn from(v: PlanQuotaKind) -> Self {
        match v {
            PlanQuotaKind::Allocation => Self::Allocation,
        }
    }
}

/// An exact version reference: a definition, a mapping or a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VersionRef {
    #[serde(with = "scalars::uuid")]
    pub id: Uuid,

    #[serde(with = "scalars::integer")]
    pub version: NonZeroU64,

    #[serde(with = "scalars::digest")]
    pub digest: Digest,
}
impl From<q::QuotaDefinitionRef> for VersionRef {
    fn from(v: q::QuotaDefinitionRef) -> Self {
        Self {
            id: v.id,
            version: v.version,
            digest: v.digest,
        }
    }
}
impl From<VersionRef> for q::QuotaDefinitionRef {
    fn from(v: VersionRef) -> Self {
        Self {
            id: v.id,
            version: v.version,
            digest: v.digest,
        }
    }
}
impl From<r::QuotaOwnerRef> for VersionRef {
    fn from(v: r::QuotaOwnerRef) -> Self {
        Self {
            id: v.id,
            version: v.version,
            digest: v.digest,
        }
    }
}
impl From<VersionRef> for r::QuotaOwnerRef {
    fn from(v: VersionRef) -> Self {
        Self {
            id: v.id,
            version: v.version,
            digest: v.digest,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanQuotaRule {
    #[serde(with = "scalars::uuid")]
    pub rule_id: Uuid,

    pub definition_ref: VersionRef,

    #[serde(with = "scalars::uuid")]
    pub item_id: Uuid,

    #[serde(deserialize_with = "scalars::required_option")]
    pub dimension_value: Option<String>,

    pub logical_binding_key: String,

    pub subject_selector: QuotaSubjectSelector,

    pub scope_selection: BTreeMap<String, String>,

    pub amount: String,

    pub unit_ref: String,

    pub basis: PlanQuotaBasis,
}
impl From<q::PlanQuotaRuleV1> for PlanQuotaRule {
    fn from(v: q::PlanQuotaRuleV1) -> Self {
        Self {
            rule_id: v.rule_id,
            definition_ref: v.definition_ref.into(),
            item_id: v.item_id,
            dimension_value: v.dimension_value,
            logical_binding_key: v.logical_binding_key,
            subject_selector: v.subject_selector.into(),
            scope_selection: v.scope_selection,
            amount: v.amount,
            unit_ref: v.unit_ref,
            basis: v.basis.into(),
        }
    }
}
impl From<PlanQuotaRule> for q::PlanQuotaRuleV1 {
    fn from(v: PlanQuotaRule) -> Self {
        Self {
            rule_id: v.rule_id,
            definition_ref: v.definition_ref.into(),
            item_id: v.item_id,
            dimension_value: v.dimension_value,
            logical_binding_key: v.logical_binding_key,
            subject_selector: v.subject_selector.into(),
            scope_selection: v.scope_selection,
            amount: v.amount,
            unit_ref: v.unit_ref,
            basis: v.basis.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PinnedQuotaEvidence {
    #[serde(with = "scalars::uuid")]
    pub sku_id: Uuid,

    #[serde(with = "scalars::integer")]
    pub sku_version: NonZeroU64,

    pub metric_ref: String,

    #[serde(with = "scalars::digest")]
    pub metric_digest: Digest,

    pub quota_kind: PlanQuotaKind,

    pub canonical_unit_ref: String,

    #[serde(with = "scalars::digest")]
    pub unit_digest: Digest,

    pub unit_numerator: String,

    pub unit_denominator: String,

    pub unit_min: String,

    pub unit_step: String,

    #[serde(deserialize_with = "scalars::required_option")]
    pub unit_max: Option<String>,

    pub mapping: VersionRef,

    pub profile: VersionRef,

    pub accounting_policy_ref: String,

    #[serde(with = "scalars::digest")]
    pub accounting_policy_digest: Digest,

    pub scope_policy_ref: String,

    #[serde(with = "scalars::digest")]
    pub scope_policy_digest: Digest,

    pub initial_set_supported: bool,

    pub increase_supported: bool,
}
impl From<r::PinnedQuotaEvidence> for PinnedQuotaEvidence {
    fn from(v: r::PinnedQuotaEvidence) -> Self {
        Self {
            sku_id: v.sku_id,
            sku_version: v.sku_version,
            metric_ref: v.metric_ref,
            metric_digest: v.metric_digest,
            quota_kind: v.quota_kind.into(),
            canonical_unit_ref: v.canonical_unit_ref,
            unit_digest: v.unit_digest,
            unit_numerator: v.unit_numerator,
            unit_denominator: v.unit_denominator,
            unit_min: v.unit_min,
            unit_step: v.unit_step,
            unit_max: v.unit_max,
            mapping: v.mapping.into(),
            profile: v.profile.into(),
            accounting_policy_ref: v.accounting_policy_ref,
            accounting_policy_digest: v.accounting_policy_digest,
            scope_policy_ref: v.scope_policy_ref,
            scope_policy_digest: v.scope_policy_digest,
            initial_set_supported: v.initial_set_supported,
            increase_supported: v.increase_supported,
        }
    }
}
impl From<PinnedQuotaEvidence> for r::PinnedQuotaEvidence {
    fn from(v: PinnedQuotaEvidence) -> Self {
        Self {
            sku_id: v.sku_id,
            sku_version: v.sku_version,
            metric_ref: v.metric_ref,
            metric_digest: v.metric_digest,
            quota_kind: v.quota_kind.into(),
            canonical_unit_ref: v.canonical_unit_ref,
            unit_digest: v.unit_digest,
            unit_numerator: v.unit_numerator,
            unit_denominator: v.unit_denominator,
            unit_min: v.unit_min,
            unit_step: v.unit_step,
            unit_max: v.unit_max,
            mapping: v.mapping.into(),
            profile: v.profile.into(),
            accounting_policy_ref: v.accounting_policy_ref,
            accounting_policy_digest: v.accounting_policy_digest,
            scope_policy_ref: v.scope_policy_ref,
            scope_policy_digest: v.scope_policy_digest,
            initial_set_supported: v.initial_set_supported,
            increase_supported: v.increase_supported,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeldBindings {
    #[serde(with = "scalars::uuid")]
    pub hold_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub acceptance_id: Uuid,

    #[serde(with = "scalars::digest")]
    pub terms_digest: Digest,

    #[serde(with = "scalars::instant")]
    pub activation_at: OffsetDateTime,

    pub bindings: Vec<AcceptedBinding>,
}
impl From<a::HeldBindings> for HeldBindings {
    fn from(v: a::HeldBindings) -> Self {
        Self {
            hold_id: v.hold_id,
            acceptance_id: v.acceptance_id,
            terms_digest: v.terms_digest,
            activation_at: v.activation_at,
            bindings: v.bindings.into_iter().map(Into::into).collect(),
        }
    }
}
impl TryFrom<HeldBindings> for a::HeldBindings {
    type Error = PriceSpecError;
    fn try_from(v: HeldBindings) -> Result<Self, Self::Error> {
        Ok(Self {
            hold_id: v.hold_id,
            acceptance_id: v.acceptance_id,
            terms_digest: v.terms_digest,
            activation_at: v.activation_at,
            bindings: v
                .bindings
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tier {
    #[serde(with = "scalars::decimal_option")]
    pub up_to: Option<Decimal>,

    #[serde(with = "scalars::decimal")]
    pub rate: Decimal,
}
impl From<r::Tier> for Tier {
    fn from(v: r::Tier) -> Self {
        Self {
            up_to: v.up_to,
            rate: v.rate,
        }
    }
}
impl From<Tier> for r::Tier {
    fn from(v: Tier) -> Self {
        Self {
            up_to: v.up_to,
            rate: v.rate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, tag = "kind")]
pub(super) enum PriceModel {
    Flat {
        #[serde(with = "scalars::decimal")]
        amount: Decimal,
    },

    PerUnit {
        #[serde(with = "scalars::decimal")]
        unit_amount: Decimal,
    },

    Volume {
        tiers: Vec<Tier>,
    },

    Graduated {
        tiers: Vec<Tier>,
    },

    Package {
        #[serde(with = "scalars::decimal")]
        package_size: Decimal,

        #[serde(with = "scalars::decimal")]
        package_price: Decimal,
    },
}
impl From<r::PriceModel> for PriceModel {
    fn from(v: r::PriceModel) -> Self {
        match v {
            r::PriceModel::Flat { amount } => Self::Flat { amount },
            r::PriceModel::PerUnit { unit_amount } => Self::PerUnit { unit_amount },
            r::PriceModel::Volume { tiers } => Self::Volume {
                tiers: tiers.into_iter().map(Into::into).collect(),
            },
            r::PriceModel::Graduated { tiers } => Self::Graduated {
                tiers: tiers.into_iter().map(Into::into).collect(),
            },
            r::PriceModel::Package {
                package_size,
                package_price,
            } => Self::Package {
                package_size,
                package_price,
            },
        }
    }
}
impl From<PriceModel> for r::PriceModel {
    fn from(v: PriceModel) -> Self {
        match v {
            PriceModel::Flat { amount } => Self::Flat { amount },
            PriceModel::PerUnit { unit_amount } => Self::PerUnit { unit_amount },
            PriceModel::Volume { tiers } => Self::Volume {
                tiers: tiers.into_iter().map(Into::into).collect(),
            },
            PriceModel::Graduated { tiers } => Self::Graduated {
                tiers: tiers.into_iter().map(Into::into).collect(),
            },
            PriceModel::Package {
                package_size,
                package_price,
            } => Self::Package {
                package_size,
                package_price,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImmutablePrice {
    #[serde(with = "scalars::uuid")]
    pub price_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub price_book_entry_id: Uuid,

    #[serde(with = "scalars::digest")]
    pub money_digest: Digest,

    pub currency: String,

    /// The scale stored with the price when it was written; never the current table's.
    #[serde(with = "scalars::integer")]
    pub currency_scale: u8,

    pub model: PriceModel,

    #[serde(with = "scalars::decimal_option")]
    pub minimum_fee: Option<Decimal>,

    #[serde(with = "scalars::date")]
    pub effective_from: Date,

    #[serde(with = "scalars::date_option")]
    pub ends_on: Option<Date>,
}
/// The frozen v1 price has no state: a binding's price is approved, because resolve never binds a
/// cancelled price and a price a binding names is never cancelled (D-520).
impl From<r::ImmutablePrice> for ImmutablePrice {
    fn from(v: r::ImmutablePrice) -> Self {
        Self {
            price_id: v.price_id,
            price_book_entry_id: v.price_book_entry_id,
            money_digest: v.money_digest,
            currency: v.currency.code().into(),
            currency_scale: v.currency.scale(),
            model: v.model.into(),
            minimum_fee: v.minimum_fee,
            effective_from: v.effective_from,
            ends_on: v.ends_on,
        }
    }
}
/// A stored receipt price whose currency and scale make no valid spec. It names the price, the code
/// and the scale, so the corrupt row can be found from the log.
#[derive(Debug, thiserror::Error)]
#[error("price {price_id}: currency {currency:?} at scale {scale}: {source}")]
pub(super) struct PriceSpecError {
    price_id: Uuid,
    currency: String,
    scale: u8,
    source: bss_money::MoneyError,
}
/// The stored currency and scale must make a valid spec; the current currency table is never read.
impl TryFrom<ImmutablePrice> for r::ImmutablePrice {
    type Error = PriceSpecError;
    fn try_from(v: ImmutablePrice) -> Result<Self, Self::Error> {
        let currency = bss_money::CurrencySpec::try_new(v.currency.clone(), v.currency_scale)
            .map_err(|source| PriceSpecError {
                price_id: v.price_id,
                currency: v.currency,
                scale: v.currency_scale,
                source,
            })?;
        Ok(Self {
            price_id: v.price_id,
            price_book_entry_id: v.price_book_entry_id,
            money_digest: v.money_digest,
            currency,
            model: v.model.into(),
            minimum_fee: v.minimum_fee,
            effective_from: v.effective_from,
            ends_on: v.ends_on,
            state: r::PriceState::Approved,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ChargeKind {
    Recurring,
    Usage,
    OneTime,
}
impl From<r::ChargeKind> for ChargeKind {
    fn from(v: r::ChargeKind) -> Self {
        match v {
            r::ChargeKind::Recurring => Self::Recurring,
            r::ChargeKind::Usage => Self::Usage,
            r::ChargeKind::OneTime => Self::OneTime,
        }
    }
}
impl From<ChargeKind> for r::ChargeKind {
    fn from(v: ChargeKind) -> Self {
        match v {
            ChargeKind::Recurring => Self::Recurring,
            ChargeKind::Usage => Self::Usage,
            ChargeKind::OneTime => Self::OneTime,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcceptedBinding {
    #[serde(with = "scalars::uuid")]
    pub item_id: Uuid,
    #[serde(with = "scalars::uuid")]
    pub price_book_entry_id: Uuid,

    #[serde(deserialize_with = "scalars::required_option")]
    pub dimension_key: Option<String>,

    #[serde(deserialize_with = "scalars::required_option")]
    pub dimension_value: Option<String>,
    #[serde(with = "scalars::uuid")]
    pub sku_id: Uuid,

    #[serde(with = "scalars::integer")]
    pub sku_version: i64,

    pub sku_code: String,

    pub sku_name: String,

    #[serde(deserialize_with = "scalars::required_option")]
    pub unit: Option<String>,

    /// Dated SKU usage type. Absent on a receipt written before D-514, and omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<MeterRef>,

    pub price: ImmutablePrice,

    pub kind: ChargeKind,

    #[serde(deserialize_with = "scalars::required_option")]
    pub recurring_period: Option<BillingCycle>,

    pub via_default: bool,

    #[serde(deserialize_with = "scalars::required_option")]
    pub usage_rating_policy: Option<UsageRatingPolicy>,

    pub invoice: InvoiceInputs,
}
impl From<r::AcceptedBinding> for AcceptedBinding {
    fn from(v: r::AcceptedBinding) -> Self {
        Self {
            item_id: v.item_id,
            price_book_entry_id: v.price_book_entry_id,
            dimension_key: v.dimension_key,
            dimension_value: v.dimension_value,
            sku_id: v.sku_id,
            sku_version: v.sku_version,
            sku_code: v.sku_code,
            sku_name: v.sku_name,
            unit: v.unit,
            meter: v.meter.map(Into::into),
            price: v.price.into(),
            kind: v.kind.into(),
            recurring_period: v.recurring_period.map(Into::into),
            via_default: v.via_default,
            usage_rating_policy: v.usage_rating_policy.map(Into::into),
            invoice: v.invoice.into(),
        }
    }
}
impl TryFrom<AcceptedBinding> for r::AcceptedBinding {
    type Error = PriceSpecError;
    fn try_from(v: AcceptedBinding) -> Result<Self, Self::Error> {
        Ok(Self {
            item_id: v.item_id,
            price_book_entry_id: v.price_book_entry_id,
            dimension_key: v.dimension_key,
            dimension_value: v.dimension_value,
            sku_id: v.sku_id,
            sku_version: v.sku_version,
            sku_code: v.sku_code,
            sku_name: v.sku_name,
            unit: v.unit,
            meter: v.meter.map(Into::into),
            price: v.price.try_into()?,
            kind: v.kind.into(),
            recurring_period: v.recurring_period.map(Into::into),
            via_default: v.via_default,
            usage_rating_policy: v.usage_rating_policy.map(Into::into),
            invoice: v.invoice.into(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BindingSelection {
    #[serde(with = "scalars::uuid")]
    pub item_id: Uuid,

    #[serde(deserialize_with = "scalars::required_option")]
    pub dimension_value: Option<String>,
}
impl From<r::BindingSelection> for BindingSelection {
    fn from(v: r::BindingSelection) -> Self {
        Self {
            item_id: v.item_id,
            dimension_value: v.dimension_value,
        }
    }
}
impl From<BindingSelection> for r::BindingSelection {
    fn from(v: BindingSelection) -> Self {
        Self {
            item_id: v.item_id,
            dimension_value: v.dimension_value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum BillingCycle {
    Month,
    Year,
}
impl From<t::BillingCycle> for BillingCycle {
    fn from(v: t::BillingCycle) -> Self {
        match v {
            t::BillingCycle::Month => Self::Month,
            t::BillingCycle::Year => Self::Year,
        }
    }
}
impl From<BillingCycle> for t::BillingCycle {
    fn from(v: BillingCycle) -> Self {
        match v {
            BillingCycle::Month => Self::Month,
            BillingCycle::Year => Self::Year,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Timezone {
    #[serde(rename = "UTC")]
    Utc,
}
impl From<t::Timezone> for Timezone {
    fn from(v: t::Timezone) -> Self {
        match v {
            t::Timezone::Utc => Self::Utc,
        }
    }
}
impl From<Timezone> for t::Timezone {
    fn from(v: Timezone) -> Self {
        match v {
            Timezone::Utc => Self::Utc,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, tag = "kind")]
pub(super) enum RatingWindow {
    BillingCycle,
    CalendarHour { timezone: Timezone },
}
impl From<t::RatingWindow> for RatingWindow {
    fn from(v: t::RatingWindow) -> Self {
        match v {
            t::RatingWindow::BillingCycle => Self::BillingCycle,
            t::RatingWindow::CalendarHour { timezone } => Self::CalendarHour {
                timezone: timezone.into(),
            },
        }
    }
}
impl From<RatingWindow> for t::RatingWindow {
    fn from(v: RatingWindow) -> Self {
        match v {
            RatingWindow::BillingCycle => Self::BillingCycle,
            RatingWindow::CalendarHour { timezone } => Self::CalendarHour {
                timezone: timezone.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AggregationScope {
    SubscriptionLine,
    Resource,
}
impl From<t::AggregationScope> for AggregationScope {
    fn from(v: t::AggregationScope) -> Self {
        match v {
            t::AggregationScope::SubscriptionLine => Self::SubscriptionLine,
            t::AggregationScope::Resource => Self::Resource,
        }
    }
}
impl From<AggregationScope> for t::AggregationScope {
    fn from(v: AggregationScope) -> Self {
        match v {
            AggregationScope::SubscriptionLine => Self::SubscriptionLine,
            AggregationScope::Resource => Self::Resource,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Reset {
    RatingWindowStart,
}
impl From<t::Reset> for Reset {
    fn from(v: t::Reset) -> Self {
        match v {
            t::Reset::RatingWindowStart => Self::RatingWindowStart,
        }
    }
}
impl From<Reset> for t::Reset {
    fn from(v: Reset) -> Self {
        match v {
            Reset::RatingWindowStart => Self::RatingWindowStart,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Fold {
    #[serde(rename = "SUM")]
    Sum,
}
impl From<t::Fold> for Fold {
    fn from(v: t::Fold) -> Self {
        match v {
            t::Fold::Sum => Self::Sum,
        }
    }
}
impl From<Fold> for t::Fold {
    fn from(v: Fold) -> Self {
        match v {
            Fold::Sum => Self::Sum,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PartialWindow {
    ActualQuantityFullThresholds,
}
impl From<t::PartialWindow> for PartialWindow {
    fn from(v: t::PartialWindow) -> Self {
        match v {
            t::PartialWindow::ActualQuantityFullThresholds => Self::ActualQuantityFullThresholds,
        }
    }
}
impl From<PartialWindow> for t::PartialWindow {
    fn from(v: PartialWindow) -> Self {
        match v {
            PartialWindow::ActualQuantityFullThresholds => Self::ActualQuantityFullThresholds,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MeterRef {
    pub usage_type_id: String,

    pub version: String,
}
impl From<t::MeterRef> for MeterRef {
    fn from(v: t::MeterRef) -> Self {
        Self {
            usage_type_id: v.usage_type_id,
            version: v.version,
        }
    }
}
impl From<MeterRef> for t::MeterRef {
    fn from(v: MeterRef) -> Self {
        Self {
            usage_type_id: v.usage_type_id,
            version: v.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UsageRatingPolicyInput {
    pub rating_window: RatingWindow,

    pub aggregation_scope: AggregationScope,

    pub reset: Reset,

    pub partial_window: PartialWindow,

    pub fold: Fold,
}
impl From<t::UsageRatingPolicyInput> for UsageRatingPolicyInput {
    fn from(v: t::UsageRatingPolicyInput) -> Self {
        Self {
            rating_window: v.rating_window.into(),
            aggregation_scope: v.aggregation_scope.into(),
            reset: v.reset.into(),
            partial_window: v.partial_window.into(),
            fold: v.fold.into(),
        }
    }
}
impl From<UsageRatingPolicyInput> for t::UsageRatingPolicyInput {
    fn from(v: UsageRatingPolicyInput) -> Self {
        Self {
            rating_window: v.rating_window.into(),
            aggregation_scope: v.aggregation_scope.into(),
            reset: v.reset.into(),
            partial_window: v.partial_window.into(),
            fold: v.fold.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UsageRatingPolicy {
    #[serde(with = "scalars::uuid")]
    pub policy_id: Uuid,

    #[serde(with = "scalars::integer")]
    pub version: u64,

    #[serde(with = "scalars::digest")]
    pub digest: Digest,

    pub content: UsageRatingPolicyInput,
}
impl From<t::UsageRatingPolicy> for UsageRatingPolicy {
    fn from(v: t::UsageRatingPolicy) -> Self {
        Self {
            policy_id: v.policy_id,
            version: v.version,
            digest: v.digest,
            content: v.content.into(),
        }
    }
}
impl From<UsageRatingPolicy> for t::UsageRatingPolicy {
    fn from(v: UsageRatingPolicy) -> Self {
        Self {
            policy_id: v.policy_id,
            version: v.version,
            digest: v.digest,
            content: v.content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum BillingTiming {
    Advance,
    Arrears,
}
impl From<t::BillingTiming> for BillingTiming {
    fn from(v: t::BillingTiming) -> Self {
        match v {
            t::BillingTiming::Advance => Self::Advance,
            t::BillingTiming::Arrears => Self::Arrears,
        }
    }
}
impl From<BillingTiming> for t::BillingTiming {
    fn from(v: BillingTiming) -> Self {
        match v {
            BillingTiming::Advance => Self::Advance,
            BillingTiming::Arrears => Self::Arrears,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum InputSource {
    Entry,
    SkuVersion,
    SellerSettings,
}
impl From<t::InputSource> for InputSource {
    fn from(v: t::InputSource) -> Self {
        match v {
            t::InputSource::Entry => Self::Entry,
            t::InputSource::SkuVersion => Self::SkuVersion,
            t::InputSource::SellerSettings => Self::SellerSettings,
        }
    }
}
impl From<InputSource> for t::InputSource {
    fn from(v: InputSource) -> Self {
        match v {
            InputSource::Entry => Self::Entry,
            InputSource::SkuVersion => Self::SkuVersion,
            InputSource::SellerSettings => Self::SellerSettings,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Rounding {
    HalfEven,
}
impl From<t::Rounding> for Rounding {
    fn from(v: t::Rounding) -> Self {
        match v {
            t::Rounding::HalfEven => Self::HalfEven,
        }
    }
}
impl From<Rounding> for t::Rounding {
    fn from(v: Rounding) -> Self {
        match v {
            Rounding::HalfEven => Self::HalfEven,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InvoiceInputs {
    pub template: String,

    #[serde(with = "scalars::digest")]
    pub template_digest: Digest,

    pub template_source: InputSource,

    pub gl_code: String,

    pub tax_category: String,

    pub timing: BillingTiming,

    pub rounding: Rounding,
}
impl From<t::InvoiceInputs> for InvoiceInputs {
    fn from(v: t::InvoiceInputs) -> Self {
        Self {
            template: v.template,
            template_digest: v.template_digest,
            template_source: v.template_source.into(),
            gl_code: v.gl_code,
            tax_category: v.tax_category,
            timing: v.timing.into(),
            rounding: v.rounding.into(),
        }
    }
}
impl From<InvoiceInputs> for t::InvoiceInputs {
    fn from(v: InvoiceInputs) -> Self {
        Self {
            template: v.template,
            template_digest: v.template_digest,
            template_source: v.template_source.into(),
            gl_code: v.gl_code,
            tax_category: v.tax_category,
            timing: v.timing.into(),
            rounding: v.rounding.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum BillingAnchor {
    Calendar,

    SubscriptionStart,
}
impl From<t::BillingAnchor> for BillingAnchor {
    fn from(v: t::BillingAnchor) -> Self {
        match v {
            t::BillingAnchor::Calendar => Self::Calendar,
            t::BillingAnchor::SubscriptionStart => Self::SubscriptionStart,
        }
    }
}
impl From<BillingAnchor> for t::BillingAnchor {
    fn from(v: BillingAnchor) -> Self {
        match v {
            BillingAnchor::Calendar => Self::Calendar,
            BillingAnchor::SubscriptionStart => Self::SubscriptionStart,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, tag = "kind")]
pub(super) enum TermsSource {
    ExplicitOrder,

    SellerPolicy {
        #[serde(with = "scalars::uuid")]
        id: Uuid,

        #[serde(with = "scalars::integer")]
        version: u64,
    },
}
impl From<t::TermsSource> for TermsSource {
    fn from(v: t::TermsSource) -> Self {
        match v {
            t::TermsSource::ExplicitOrder => Self::ExplicitOrder,
            t::TermsSource::SellerPolicy { id, version } => Self::SellerPolicy { id, version },
        }
    }
}
impl From<TermsSource> for t::TermsSource {
    fn from(v: TermsSource) -> Self {
        match v {
            TermsSource::ExplicitOrder => Self::ExplicitOrder,
            TermsSource::SellerPolicy { id, version } => Self::SellerPolicy { id, version },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BillingTerms {
    #[serde(with = "scalars::integer")]
    pub schema_version: u32,

    pub cycle: BillingCycle,

    pub anchor: BillingAnchor,

    #[serde(with = "scalars::instant")]
    pub anchor_at: OffsetDateTime,

    pub timezone: Timezone,

    pub source: TermsSource,

    #[serde(with = "scalars::digest")]
    pub digest: Digest,
}
impl From<t::BillingTerms> for BillingTerms {
    fn from(v: t::BillingTerms) -> Self {
        Self {
            schema_version: v.schema_version,
            cycle: v.cycle.into(),
            anchor: v.anchor.into(),
            anchor_at: v.anchor_at,
            timezone: v.timezone.into(),
            source: v.source.into(),
            digest: v.digest,
        }
    }
}
impl From<BillingTerms> for t::BillingTerms {
    fn from(v: BillingTerms) -> Self {
        Self {
            schema_version: v.schema_version,
            cycle: v.cycle.into(),
            anchor: v.anchor.into(),
            anchor_at: v.anchor_at,
            timezone: v.timezone.into(),
            source: v.source.into(),
            digest: v.digest,
        }
    }
}

#[cfg(test)]
#[path = "v1_tests.rs"]
mod v1_tests;

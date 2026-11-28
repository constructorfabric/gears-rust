//! The consumer read contract's wire shapes (D-419…D-422): `snake_case`, money and quantities as
//! exact decimal text, dates as `YYYY-MM-DD`. The consumer goldens freeze them. A closed set is
//! its `enum` (D-439), with the tokens the goldens carry.
use crate::api::rest::authoring::dto::PricingQuotaDefinitionRefDto;
use crate::api::rest::closed_sets::{
    PricingChargeKind, PricingEligibility, PricingModel, PricingPeriod, PricingPinnedPriceStatus,
    PricingQuotaBasis, PricingQuotaKind, PricingQuotaSubjectSelector, PricingResolveSource,
    PricingResolvedRevisionState,
};
use crate::infra::usage_policy_wire::digest_text;
use bss_pricing_sdk::read::{QuotaOwnerRef, QuotaTerms};
use std::collections::BTreeMap;
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
    /// The book's currency. Each binding carries its price's own currency spec: the scale is
    /// stored per price, so prices of one book written under different scales each keep theirs.
    pub currency: String,
    /// The date resolved, `YYYY-MM-DD`.
    pub date: String,
    /// Every item of the revision, or the one `item_id` names.
    pub items: Vec<PricingResolveItemDto>,
    /// The revision's quota terms (D-547), as the typed read answers them: every stored rule,
    /// whatever item the read names, with the evidence its definition pinned at the save, and the
    /// stored set digest. Read from the stored rules, with no catalog call. Left out for a
    /// revision without rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_terms: Option<PricingQuotaTermsDto>,
}
/// A revision's quota terms on `GET /resolve` (D-547).
#[toolkit_macros::api_dto(response)]
pub struct PricingQuotaTermsDto {
    /// The revision's stored set digest, lowercase hex (D-541).
    pub quota_rules_digest: String,
    /// Every rule of the revision, ordered by `rule_id`.
    pub rules: Vec<PricingResolvedQuotaRuleDto>,
}
impl PricingQuotaTermsDto {
    /// The typed read's terms as the wire shows them.
    #[must_use]
    pub fn of(terms: &QuotaTerms) -> Self {
        Self {
            quota_rules_digest: digest_text(terms.quota_rules_digest),
            rules: terms
                .rules
                .iter()
                .map(|resolved| {
                    let (rule, e) = (&resolved.rule, &resolved.evidence);
                    PricingResolvedQuotaRuleDto {
                        rule: PricingQuotaTermsRuleDto {
                            rule_id: rule.rule_id,
                            definition_ref: PricingQuotaDefinitionRefDto {
                                id: rule.definition_ref.id,
                                version: rule.definition_ref.version.to_string(),
                                digest: digest_text(rule.definition_ref.digest),
                            },
                            item_id: rule.item_id,
                            dimension_value: rule.dimension_value.clone(),
                            logical_binding_key: rule.logical_binding_key.clone(),
                            subject_selector: rule.subject_selector.into(),
                            scope_selection: rule.scope_selection.clone(),
                            amount: rule.amount.clone(),
                            unit_ref: rule.unit_ref.clone(),
                            basis: rule.basis.into(),
                        },
                        evidence: PricingQuotaEvidenceDto {
                            sku_id: e.sku_id,
                            sku_version: e.sku_version.to_string(),
                            metric_ref: e.metric_ref.clone(),
                            metric_digest: digest_text(e.metric_digest),
                            quota_kind: e.quota_kind.into(),
                            canonical_unit_ref: e.canonical_unit_ref.clone(),
                            unit_digest: digest_text(e.unit_digest),
                            unit_numerator: e.unit_numerator.clone(),
                            unit_denominator: e.unit_denominator.clone(),
                            unit_min: e.unit_min.clone(),
                            unit_step: e.unit_step.clone(),
                            unit_max: e.unit_max.clone(),
                            mapping: PricingQuotaOwnerRefDto::of(&e.mapping),
                            profile: PricingQuotaOwnerRefDto::of(&e.profile),
                            accounting_policy_ref: e.accounting_policy_ref.clone(),
                            accounting_policy_digest: digest_text(e.accounting_policy_digest),
                            scope_policy_ref: e.scope_policy_ref.clone(),
                            scope_policy_digest: digest_text(e.scope_policy_digest),
                            initial_set_supported: e.initial_set_supported,
                            increase_supported: e.increase_supported,
                        },
                    }
                })
                .collect(),
        }
    }
}
/// One rule of a revision's quota terms, with what its definition pinned at the save (D-547).
#[toolkit_macros::api_dto(response)]
pub struct PricingResolvedQuotaRuleDto {
    /// The rule as stored, without its availability observation.
    pub rule: PricingQuotaTermsRuleDto,
    /// What the rule's definition pinned when the rule was saved.
    pub evidence: PricingQuotaEvidenceDto,
}
/// A rule's ten members as stored (D-541): an exact definition version, an amount in an allowed
/// unit, a basis, a logical binding and a scope selection, for one item of the revision.
#[toolkit_macros::api_dto(response)]
pub struct PricingQuotaTermsRuleDto {
    pub rule_id: Uuid,
    pub definition_ref: PricingQuotaDefinitionRefDto,
    pub item_id: Uuid,
    /// The item's dimension value; null for an item without one, or for the item's default rule.
    pub dimension_value: Option<String>,
    pub logical_binding_key: String,
    pub subject_selector: PricingQuotaSubjectSelector,
    pub scope_selection: BTreeMap<String, String>,
    /// A canonical decimal integer in `unit_ref`.
    pub amount: String,
    pub unit_ref: String,
    pub basis: PricingQuotaBasis,
}
/// What a rule's definition pinned when the rule was saved (D-547): the part of its stored
/// evidence the quota arithmetic needs. Digests are 64 lowercase hex characters and versions
/// decimal strings; no text and no availability.
#[toolkit_macros::api_dto(response)]
pub struct PricingQuotaEvidenceDto {
    /// The SKU the definition was approved for.
    pub sku_id: Uuid,
    /// The SKU version it was approved against.
    pub sku_version: String,
    /// The metric: the rule's quota.
    pub metric_ref: String,
    pub metric_digest: String,
    /// The quota's kind, as the definition carried it at the save.
    pub quota_kind: PricingQuotaKind,
    /// The metric's canonical unit.
    pub canonical_unit_ref: String,
    /// The registry content digest of the rule's unit (`unit_ref`).
    pub unit_digest: String,
    /// The rule's unit in the canonical unit: numerator, a canonical decimal integer.
    pub unit_numerator: String,
    /// The rule's unit in the canonical unit: denominator, a canonical decimal integer.
    pub unit_denominator: String,
    /// The smallest limit in the rule's unit, a canonical decimal integer.
    pub unit_min: String,
    /// The grid of a limit in the rule's unit, a canonical decimal integer: a limit is
    /// `unit_min + n × unit_step` for a whole `n ≥ 0`; a step of `0` admits `unit_min` alone.
    pub unit_step: String,
    /// The largest limit in the rule's unit, a canonical decimal integer; null when the unit has
    /// no maximum.
    pub unit_max: Option<String>,
    /// The adapter mapping.
    pub mapping: PricingQuotaOwnerRefDto,
    /// The adapter profile.
    pub profile: PricingQuotaOwnerRefDto,
    pub accounting_policy_ref: String,
    pub accounting_policy_digest: String,
    pub scope_policy_ref: String,
    pub scope_policy_digest: String,
    /// The definition supports an initial set.
    pub initial_set_supported: bool,
    /// The definition supports an increase.
    pub increase_supported: bool,
}
/// A version-pinned owner reference (a mapping, a profile) as the definition pinned it.
#[toolkit_macros::api_dto(response)]
pub struct PricingQuotaOwnerRefDto {
    pub id: Uuid,
    /// The record's version, a decimal string.
    pub version: String,
    /// The version's content digest, lowercase hex.
    pub digest: String,
}
impl PricingQuotaOwnerRefDto {
    fn of(reference: &QuotaOwnerRef) -> Self {
        Self {
            id: reference.id,
            version: reference.version.to_string(),
            digest: digest_text(reference.digest),
        }
    }
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
    /// The price's currency as stored with the price: its book's.
    pub currency: String,
    /// The currency's scale stored with the price: the number of minor digits its money posts
    /// with, a JSON integer. A later change of the currency table does not change it.
    pub currency_scale: u8,
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
    /// The price's currency as stored with the price: its book's.
    pub currency: String,
    /// The currency's scale stored with the price, a JSON integer; never the current table's.
    pub currency_scale: u8,
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

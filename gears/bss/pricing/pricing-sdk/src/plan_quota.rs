//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-store:p1
//! Plan quota rules: the v1 shape a revision stores and an acceptance freezes (D-541). Pricing
//! defines it until Entitlements publishes a rule schema; no GTS id.
//!
//! A rule selects one exact Products quota definition version and states an amount, a unit, a
//! basis and a logical binding for one item of its revision. The set digest is
//! [`crate::digest::quota_rules_digest`].
//!
//! [`PricingQuotaAuthoringClientV1`] authors a draft's rules in-process (D-540): it reads them,
//! replaces the whole set and reads the revision's checks, with the same answers, refusals and
//! versions as Pricing's REST endpoints.
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use time::{Date, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::Digest;

/// The most rules one revision holds.
pub const MAX_RULES: usize = 200;
/// The longest logical binding key.
pub const BINDING_KEY_MAX: usize = 64;
/// The most keys one scope selection names.
pub const SCOPE_KEYS_MAX: usize = 8;
/// The longest scope selection value, in bytes.
pub const SCOPE_VALUE_MAX: usize = 255;

/// How a rule states its amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanQuotaBasis {
    /// The amount is the limit, whatever the quantity sold.
    Fixed,
    /// The amount is the limit for one plan unit; the quantity sold multiplies it.
    PerPlanUnit,
}

impl PlanQuotaBasis {
    /// The REST and digest token: `fixed`, `per_plan_unit`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::PerPlanUnit => "per_plan_unit",
        }
    }

    /// The basis a token names, if any. Tokens are exact: no case folding.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "fixed" => Some(Self::Fixed),
            "per_plan_unit" => Some(Self::PerPlanUnit),
            _ => None,
        }
    }
}

/// Whose quota the rule sets; v1 knows one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuotaSubjectSelector {
    /// The tenant that owns the resources the quota limits.
    ResourceOwner,
}

impl QuotaSubjectSelector {
    /// The REST and digest token: `resource_owner`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::ResourceOwner => "resource_owner",
        }
    }

    /// The selector a token names, if any. Tokens are exact: no case folding.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "resource_owner" => Some(Self::ResourceOwner),
            _ => None,
        }
    }
}

/// The kind of a quota, as its definition pinned it (Products' `QuotaKind`); v1 knows one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanQuotaKind {
    /// Capacity the service allocates to a subject and enforces.
    Allocation,
}

impl PlanQuotaKind {
    /// The REST and digest token: `allocation`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Allocation => "allocation",
        }
    }

    /// The kind a token names, if any. Tokens are exact: no case folding.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "allocation" => Some(Self::Allocation),
            _ => None,
        }
    }
}

/// An exact Products definition version: its id, its version and the digest Products gave it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuotaDefinitionRef {
    /// The definition's id.
    pub id: Uuid,
    /// The definition's version.
    pub version: NonZeroU64,
    /// The version's content digest, as Products computed it; Pricing only compares it.
    pub digest: Digest,
}

/// One rule (quotas DESIGN §3.3.1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuotaRuleV1 {
    /// A client UUID, unique in the revision.
    pub rule_id: Uuid,
    /// The exact definition version the rule sets.
    pub definition_ref: QuotaDefinitionRef,
    /// An item of the same revision.
    pub item_id: Uuid,
    /// The item's dimension value. `None` on an item without a dimension; on an item with one,
    /// `None` is the item's default rule for its quota (D-541).
    pub dimension_value: Option<String>,
    /// The binding that groups the rules one adapter target provisions together.
    pub logical_binding_key: String,
    /// Whose quota the rule sets.
    pub subject_selector: QuotaSubjectSelector,
    /// The scope the quota applies in, by key.
    pub scope_selection: BTreeMap<String, String>,
    /// A canonical decimal integer in `unit_ref`.
    pub amount: String,
    /// The unit of `amount`: one of the definition's allowed units.
    pub unit_ref: String,
    /// How `amount` is stated.
    pub basis: PlanQuotaBasis,
}

/// Whether a Products quota definition version may be sold, as a rule saw it (D-542).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanQuotaAvailabilityState {
    /// Sellable.
    Enabled,
    /// Not sellable yet; the reason says why.
    Disabled,
    /// No longer sellable.
    Withdrawn,
}

impl PlanQuotaAvailabilityState {
    /// The REST token: `enabled`, `disabled`, `withdrawn`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// The state a token names, if any. Tokens are exact: no case folding.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// The availability of a rule's definition version when the rule was saved (D-542).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuotaAvailability {
    /// The state.
    pub state: PlanQuotaAvailabilityState,
    /// The revision of Products' observation.
    pub revision: NonZeroU64,
    /// When Products recorded the state.
    pub observed_at: OffsetDateTime,
    /// Why the version is not enabled; `None` when it is.
    pub reason_code: Option<String>,
}

/// A stored rule with the availability its definition showed when the rule was saved; the checks
/// judge the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPlanQuotaRuleV1 {
    /// The rule.
    pub rule: PlanQuotaRuleV1,
    /// The availability seen at the save.
    pub availability: PlanQuotaAvailability,
}

/// A revision's whole stored rule set (D-540): what a read answers and what a replacement saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuotaRulesV1 {
    /// The revision.
    pub revision_id: Uuid,
    /// The revision's version: the `ETag` of the REST read, the version a replacement expects.
    pub revision_version: u64,
    /// The rules, ordered by `rule_id`.
    pub quota_rules: Vec<SavedPlanQuotaRuleV1>,
    /// The stored digest of the rules ([`crate::digest::quota_rules_digest`]).
    pub quota_rules_digest: Digest,
}

/// An item a check row is about: the item, its SKU and its entry, and on a quota row that names a
/// rule, the rule and its member (D-544); the line of an uncovered sellable cell names only its
/// item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCheckSubjectV1 {
    /// The item.
    pub item_id: Uuid,
    /// The item's SKU.
    pub sku_id: Uuid,
    /// The entry the item names; `None` for an item without one.
    pub price_book_entry_id: Option<Uuid>,
    /// The quota rule a quota row names; `None` on every other row, and for an uncovered sellable
    /// cell, whose line names only its item.
    pub quota_rule_id: Option<Uuid>,
    /// The rule's top-level member (`""` for the rule as a whole); `None` whenever
    /// `quota_rule_id` is `None`.
    pub field: Option<String>,
}

/// A pending price that blocks a check row: its approval unit, the price and its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the wire names of the checks answer, each an id of another aggregate"
)]
pub struct PlanCheckBlockingPriceV1 {
    /// The approval unit.
    pub unit_id: Uuid,
    /// The price.
    pub price_id: Uuid,
    /// The price's entry.
    pub price_book_entry_id: Uuid,
}

/// One row of a revision's checks. An `info` row is always ok and never blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCheckV1 {
    /// The row's code.
    pub code: String,
    /// Whether the row is green.
    pub ok: bool,
    /// What the row checks.
    pub label: String,
    /// What it found.
    pub detail: String,
    /// An information row.
    pub info: bool,
    /// The approval units whose pending prices would cover what is uncovered.
    pub blocked_by: Vec<Uuid>,
    /// The items that turn the row red.
    pub subjects: Vec<PlanCheckSubjectV1>,
    /// The pending prices behind `blocked_by`.
    pub blocked_by_prices: Vec<PlanCheckBlockingPriceV1>,
}

/// A revision's checks on its sale date (D-544): every row, the quota rows of a revision with
/// rules included, with the version and the rule digest they judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRevisionChecksV1 {
    /// Every row, in their order.
    pub checks: Vec<PlanCheckV1>,
    /// Every row is ok: the revision may be submitted.
    pub ready: bool,
    /// The sale date the checks judged.
    pub sale_date: Date,
    /// The quorum a submit of the revision needs.
    pub quorum_required: u32,
    /// The revision version the checks judged.
    pub revision_version: u64,
    /// The stored digest of the rules the checks judged.
    pub quota_rules_digest: Digest,
}

/// Authoring a draft revision's quota rules in-process (D-540): the SDK beside
/// `GET`/`PUT /plan-revisions/{id}/quota-rules` and `GET /plan-revisions/{id}/checks`. Each method
/// authorizes the caller as its endpoint does and answers the same request identically: the same
/// body, the same refusals and the same version.
#[async_trait::async_trait]
pub trait PricingQuotaAuthoringClientV1: Send + Sync {
    /// A revision's stored rules and their digest, under `read` on `plan`; Products is not
    /// asked.
    /// # Errors
    /// 401 or 403 for the caller; 404 for a revision the tenant does not hold; 503 when the
    /// policy cannot judge.
    async fn get_quota_rules(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
    ) -> Result<PlanQuotaRulesV1, CanonicalError>;

    /// Replace a draft's whole rule set (D-543), under `author` on `plan`, at
    /// `expected_version`, once per `operation_id` (the idempotency key): the same order,
    /// validation, fence and stored answer as the `PUT`. An empty `rules` clears the set. The key
    /// space is the `PUT`'s: the same key, version and rules replay across both surfaces.
    /// # Errors
    /// As the `PUT`: 400 for a malformed key, every failure of the set in one refusal, 404, 403
    /// `NOT_DRAFT_AUTHOR`, 409 `REVISION_NOT_DRAFT`, `STALE_REVISION`, `IDEMPOTENCY_CONFLICT`,
    /// `IDEMPOTENCY_KEY_IN_FLIGHT`, `UNIT_CONTENDED` or `CONTENDED`, 503
    /// `QUOTA_CATALOG_UNAVAILABLE` or `REGISTRY_UNAVAILABLE`. The refusal of the set is an
    /// invalid-argument error whose field violations carry the codes, with no reason; it answers
    /// 409 (`status_code()`) when each failure is a conflict (`QUOTA_RULE_DUPLICATE`,
    /// `QUOTA_DEFINITION_CHANGED`), else 400.
    async fn replace_quota_rules(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
        expected_version: u64,
        operation_id: &str,
        rules: Vec<PlanQuotaRuleV1>,
    ) -> Result<PlanQuotaRulesV1, CanonicalError>;

    /// The complete saved revision's checks (D-544), under `read` on `plan`: the same rows as
    /// `GET /plan-revisions/{id}/checks`, the quota rows of a revision with rules validated by
    /// Products on every call.
    /// # Errors
    /// 404; Products' own refusal; 503 `REGISTRY_UNAVAILABLE` or `QUOTA_CATALOG_UNAVAILABLE`.
    async fn check_plan_revision(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
    ) -> Result<PlanRevisionChecksV1, CanonicalError>;
}

/// Whether `key` is a logical binding key: 1 to 64 of ASCII letters, digits, `_`, `-`, `.`.
#[must_use]
pub fn binding_key_is_valid(key: &str) -> bool {
    (1..=BINDING_KEY_MAX).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

#[cfg(test)]
#[path = "plan_quota_tests.rs"]
mod plan_quota_tests;

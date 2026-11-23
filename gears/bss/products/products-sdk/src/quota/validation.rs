//! @cpt-dod:cpt-cf-bss-products-dod-quota-set-validation:p1
//! The quota-set validation contract (P-D-275, P-D-276): Pricing asks Products to judge one
//! complete set of plan quota rules through [`validate_quota_set`]. The answer is facts: one
//! verdict per rule and every check found. Pricing decides what each of its boundaries refuses;
//! Products refuses only a malformed request.
//!
//! [`validate_quota_set`]: super::catalog::ProductsQuotaCatalogClientV1::validate_quota_set
use std::collections::BTreeMap;

use uuid::Uuid;

use super::catalog::{QuotaBasis, QuotaDefinition, QuotaDefinitionRef, QuotaVersion};

/// The most rules one set holds, as a plan revision does; also the most prior limits.
pub const MAX_RULES: usize = 200;
/// The longest logical binding key and scope key, in bytes; neither is empty.
pub const MAX_KEY_BYTES: usize = 64;
/// The most keys one scope selection names.
pub const MAX_SCOPE_KEYS: usize = 8;
/// The longest scope selection value, in bytes; none is empty. A value off this bound, or one
/// that is not printable ASCII, is a `QUOTA_SCOPE_UNSUPPORTED` check, not a malformed request
/// (P-D-276).
pub const MAX_SCOPE_VALUE_BYTES: usize = 255;
/// The longest locale tag, in bytes.
pub const MAX_LOCALE_BYTES: usize = 64;
/// The longest dimension value or subject selector, in bytes.
pub const MAX_TEXT_BYTES: usize = 512;
/// The longest unit or metric reference, in bytes: the longest GTS id the catalog stores (the
/// GTS id maximum), so no reference to a stored unit or metric is refused.
pub const MAX_REF_BYTES: usize = gts::gts::GTS_ID_MAX_LENGTH;

/// Why Pricing validates: an author's draft and its publication, or a sale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaValidationPurpose {
    /// A draft's save, its checks, its submit and its approval.
    Authoring,
    /// A sale's acceptance, at its purchased quantity.
    Acceptance,
}

/// What the set does to an installed allowance (quotas DESIGN §3.3.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaChangeKind {
    /// The first installation of the limits.
    InitialSet,
    /// Raising installed limits.
    Increase,
    /// The installed limits stay as they are.
    Unchanged,
}

/// One plan rule as Products judges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaSetRule {
    /// The rule's id in its plan revision.
    pub rule_id: Uuid,
    /// The exact definition version the rule names.
    pub definition: QuotaDefinitionRef,
    /// The plan item's SKU.
    pub sku_id: Uuid,
    /// The SKU version in force for the plan (its sale date, or the accepted binding's).
    pub sku_version: QuotaVersion,
    /// The logical binding the rule's limit applies through: 1 to [`MAX_KEY_BYTES`] bytes.
    pub logical_binding_key: String,
    /// The requested dimension value the rule applies to; `None` for an item without one, at
    /// most [`MAX_TEXT_BYTES`] bytes. It is carried, not judged: it groups nothing, and every rule
    /// of one binding shares one scope, whatever its value (P-D-276).
    pub alternative: Option<String>,
    /// The subject the limit applies to, as the scope policy spells it (`resource_owner`); at
    /// most [`MAX_TEXT_BYTES`] bytes.
    pub subject_selector: String,
    /// The scope the limit applies in: one value per required dimension, at most
    /// [`MAX_SCOPE_KEYS`] keys of 1 to [`MAX_KEY_BYTES`] bytes.
    pub scope_selection: BTreeMap<String, String>,
    /// A canonical decimal integer in `unit_ref`.
    pub amount: String,
    /// The allowed unit the amount is stated in, at most [`MAX_REF_BYTES`] bytes.
    pub unit_ref: String,
    /// How the amount applies: once, or per purchased plan unit.
    pub basis: QuotaBasis,
}

/// A limit already in effect, for an increase or an unchanged set; one per binding and metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPriorLimit {
    /// The logical binding the limit is installed through: 1 to [`MAX_KEY_BYTES`] bytes.
    pub logical_binding_key: String,
    /// The metric the limit bounds, at most [`MAX_REF_BYTES`] bytes.
    pub metric_ref: String,
    /// The unit `amount` is stated in: the metric's canonical unit or an allowed unit; at most
    /// [`MAX_REF_BYTES`] bytes.
    pub unit_ref: String,
    /// A canonical decimal integer in `unit_ref`.
    pub amount: String,
}

/// One complete applicable set (quotas DESIGN §3.3.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaSetRequest {
    /// Why Pricing validates.
    pub purpose: QuotaValidationPurpose,
    /// What the set does to an installed allowance.
    pub change_kind: QuotaChangeKind,
    /// The purchased plan quantity, a canonical integer from 1; required for `Acceptance`.
    pub quantity: Option<String>,
    /// The limits in effect; only for `Increase` and `Unchanged`, at most [`MAX_RULES`].
    pub prior: Vec<QuotaPriorLimit>,
    /// At most [`MAX_RULES`] rules, each `rule_id` once.
    pub rules: Vec<QuotaSetRule>,
    /// The locale to serve the definitions' texts in, at most [`MAX_LOCALE_BYTES`] bytes;
    /// English when absent or not held.
    pub locale: Option<String>,
}

/// The stable codes a check carries (P-D-276).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QuotaCheckCode {
    /// An unknown version, another SKU or an earlier SKU version, a basis or a unit the
    /// definition does not allow.
    RuleInvalid,
    /// The rule's digest is not the stored one.
    DefinitionChanged,
    /// An amount, or a final limit at the quantity, off the unit's grid or above its `max`.
    AmountInvalid,
    /// The amount does not convert exactly into the canonical unit.
    UnitInexact,
    /// Another subject selector, or a scope selection that is not the policy's dimensions.
    ScopeUnsupported,
    /// Rules that do not fit together, or a change of limits that is not allowed.
    SetIncompatible,
    /// The version is not enabled for sale.
    QualificationUnavailable,
}

impl QuotaCheckCode {
    /// Every code, in the order Pricing's checks rows show them.
    pub const ALL: [Self; 7] = [
        Self::RuleInvalid,
        Self::DefinitionChanged,
        Self::AmountInvalid,
        Self::UnitInexact,
        Self::ScopeUnsupported,
        Self::SetIncompatible,
        Self::QualificationUnavailable,
    ];

    /// The wire token.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RuleInvalid => "QUOTA_RULE_INVALID",
            Self::DefinitionChanged => "QUOTA_DEFINITION_CHANGED",
            Self::AmountInvalid => "QUOTA_AMOUNT_INVALID",
            Self::UnitInexact => "QUOTA_UNIT_INEXACT",
            Self::ScopeUnsupported => "QUOTA_SCOPE_UNSUPPORTED",
            Self::SetIncompatible => "QUOTA_SET_INCOMPATIBLE",
            Self::QualificationUnavailable => "QUOTA_QUALIFICATION_UNAVAILABLE",
        }
    }
}

/// One failed check: the rules it is about, the field (`amount`, `scope_selection.region`, …),
/// and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCheck {
    /// The stable code.
    pub code: QuotaCheckCode,
    /// The rules the check is about: one for a rule check, every rule of the group for a set
    /// check; none for a prior limit that no rule keeps, a check about the whole set.
    pub rule_ids: Vec<Uuid>,
    /// The rule member the check names, or the request member `quantity`, `prior` or
    /// `change_kind`.
    pub field: String,
    /// Why, in plain English.
    pub detail: String,
}

/// What Products resolved for one rule: its definition (absent when unknown to the caller's
/// scope) and, with a quantity, the final limit in the rule's unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRuleVerdict {
    /// The rule.
    pub rule_id: Uuid,
    /// The definition version the rule names, with its current availability; `None` when the
    /// caller's scope does not hold it.
    pub definition: Option<QuotaDefinition>,
    /// The final limit at the request's quantity, in the rule's unit; `None` without a
    /// quantity or when the limit did not pass.
    pub limit: Option<String>,
}

/// The answer: one verdict per rule, in request order, and every check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaSetValidation {
    /// One verdict per rule, in request order.
    pub rules: Vec<QuotaRuleVerdict>,
    /// Every check found: rule checks in rule order, then set checks.
    pub checks: Vec<QuotaCheck>,
}

impl QuotaSetValidation {
    /// No check failed.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.checks.is_empty()
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;

//! @cpt-dod:cpt-cf-bss-products-dod-quota-set-validation:p1
//! The quota-set judgement (P-D-275, P-D-276): a pure function over a request and the definition
//! versions it resolved. Each rule is checked against its definition, then the rules of one
//! logical binding together, then the change kind. It reads nothing, calls nothing and never
//! stops at the first failure or drops a rule: rule checks in rule order, then set checks. The
//! answer is facts; Pricing decides what each of its boundaries refuses.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use bss_products_sdk::quota::catalog::{
    QuotaAllowedUnit, QuotaAvailabilityState, QuotaBasis, QuotaDefinition, QuotaVersion,
};
use bss_products_sdk::quota::gts::{ScopeDimension, SubjectSelector};
use bss_products_sdk::quota::units::{Amount, Factor, UnitError, convert};
use bss_products_sdk::quota::validation::{
    MAX_KEY_BYTES, MAX_LOCALE_BYTES, MAX_REF_BYTES, MAX_RULES, MAX_SCOPE_KEYS,
    MAX_SCOPE_VALUE_BYTES as MAX_SCOPE_VALUE, MAX_TEXT_BYTES, QUANTITY_FIELD, QuotaChangeKind,
    QuotaCheck, QuotaCheckCode, QuotaPriorLimit, QuotaRuleVerdict, QuotaSetRequest, QuotaSetRule,
    QuotaSetValidation, QuotaValidationPurpose,
};
use serde_json::Value;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::review::basis_token;

/// One resolved version with what the set judgement needs of its mapping.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDefinition {
    /// The definition version with its current availability, as the caller may read it.
    pub definition: QuotaDefinition,
    /// The adapter target its mapping enforces on.
    pub target_id: Uuid,
    /// The profile version's content (P-D-270), compared without `change_support`.
    pub profile: Value,
}

/// The most characters of a caller's text a detail repeats.
const ECHO_CHARS: usize = 32;

/// A caller's text as a detail repeats it: quoted with its control characters escaped, cut after
/// [`ECHO_CHARS`] characters. Every caller text in a detail goes through it: a binding key, a
/// scope key, a unit, a metric, an amount, a quantity and a subject selector (P-D-275).
fn quoted(text: &str) -> String {
    let shown: String = text.chars().take(ECHO_CHARS).collect();
    if shown.len() < text.len() {
        format!("{shown:?}...")
    } else {
        format!("{shown:?}")
    }
}

/// A logical binding key or a scope key: 1 to [`MAX_KEY_BYTES`] bytes.
fn key_fits(key: &str) -> bool {
    (1..=MAX_KEY_BYTES).contains(&key.len())
}

/// A malformed request's field and why.
type Refusal = Option<(String, String)>;

/// Why a request is malformed, as `(field, detail)`; `None` for a well-formed one (P-D-275).
/// The counts are judged first, then the locale, each rule's id and texts, the quantity, whether
/// the change kind takes prior limits, and each prior limit. A detail repeats at most a short
/// prefix of the caller's text.
#[must_use]
pub fn request_refusal(request: &QuotaSetRequest) -> Refusal {
    count_refusal(request)
        .or_else(|| {
            let locale = request.locale.as_deref()?;
            (locale.len() > MAX_LOCALE_BYTES).then(|| {
                (
                    "locale".to_owned(),
                    format!("a locale is at most {MAX_LOCALE_BYTES} bytes"),
                )
            })
        })
        .or_else(|| rules_refusal(&request.rules))
        .or_else(|| quantity_refusal(request))
        .or_else(|| prior_refusal(request))
}

/// At most [`MAX_RULES`] rules and as many prior limits.
fn count_refusal(request: &QuotaSetRequest) -> Refusal {
    let (rules, prior) = (request.rules.len(), request.prior.len());
    if rules > MAX_RULES {
        return Some((
            "rules".to_owned(),
            format!("a set holds at most {MAX_RULES} rules, not {rules}"),
        ));
    }
    (prior > MAX_RULES).then(|| {
        (
            "prior".to_owned(),
            format!("a set names at most {MAX_RULES} prior limits, not {prior}"),
        )
    })
}

/// Each rule id once; each rule's binding key, scope keys and other texts within their bounds.
fn rules_refusal(rules: &[QuotaSetRule]) -> Refusal {
    let mut seen = HashSet::new();
    rules.iter().enumerate().find_map(|(i, rule)| {
        let at = |member: &str, detail: String| Some((format!("rules[{i}].{member}"), detail));
        if !seen.insert(rule.rule_id) {
            return at("rule_id", format!("rule {} is named twice", rule.rule_id));
        }
        if !key_fits(&rule.logical_binding_key) {
            return at(
                "logical_binding_key",
                format!("a logical binding key is 1 to {MAX_KEY_BYTES} bytes"),
            );
        }
        if rule.scope_selection.len() > MAX_SCOPE_KEYS {
            return at(
                "scope_selection",
                format!("a scope selection names at most {MAX_SCOPE_KEYS} keys"),
            );
        }
        if !rule.scope_selection.keys().all(|key| key_fits(key)) {
            return at(
                "scope_selection",
                format!("a scope key is 1 to {MAX_KEY_BYTES} bytes"),
            );
        }
        [
            ("alternative", rule.alternative.as_deref(), MAX_TEXT_BYTES),
            (
                "subject_selector",
                Some(rule.subject_selector.as_str()),
                MAX_TEXT_BYTES,
            ),
            ("unit_ref", Some(rule.unit_ref.as_str()), MAX_REF_BYTES),
        ]
        .into_iter()
        .find(|(_, text, bound)| text.is_some_and(|text| text.len() > *bound))
        .and_then(|(member, _, bound)| at(member, format!("{member} is at most {bound} bytes")))
    })
}

/// A quantity is a canonical integer from 1, and an acceptance names one.
fn quantity_refusal(request: &QuotaSetRequest) -> Refusal {
    let detail = match &request.quantity {
        Some(quantity) if !Amount::parse(quantity).is_ok_and(|q| q.get() >= 1) => format!(
            "a quantity is a canonical integer from 1, not {}",
            quoted(quantity)
        ),
        None if request.purpose == QuotaValidationPurpose::Acceptance => {
            "an acceptance names the purchased quantity".to_owned()
        }
        Some(_) | None => return None,
    };
    Some((QUANTITY_FIELD.to_owned(), detail))
}

/// Prior limits only with `Increase` and `Unchanged`, and some with them; each within its
/// bounds, with a canonical amount, and each binding and metric once.
fn prior_refusal(request: &QuotaSetRequest) -> Refusal {
    let detail = match (request.change_kind, request.prior.is_empty()) {
        (QuotaChangeKind::InitialSet, false) => Some("an initial set has no prior limits"),
        (QuotaChangeKind::Increase | QuotaChangeKind::Unchanged, true) => {
            Some("an increase or an unchanged set names the limits in effect")
        }
        (QuotaChangeKind::InitialSet, true)
        | (QuotaChangeKind::Increase | QuotaChangeKind::Unchanged, false) => None,
    };
    if let Some(detail) = detail {
        return Some(("prior".to_owned(), detail.to_owned()));
    }
    let mut seen = HashSet::new();
    request.prior.iter().enumerate().find_map(|(i, prior)| {
        let at = |member: &str, detail: String| Some((format!("prior[{i}].{member}"), detail));
        if !key_fits(&prior.logical_binding_key) {
            return at(
                "logical_binding_key",
                format!("a logical binding key is 1 to {MAX_KEY_BYTES} bytes"),
            );
        }
        for (member, text) in [
            ("metric_ref", &prior.metric_ref),
            ("unit_ref", &prior.unit_ref),
        ] {
            if text.len() > MAX_REF_BYTES {
                return at(member, format!("{member} is at most {MAX_REF_BYTES} bytes"));
            }
        }
        if let Err(e) = Amount::parse(&prior.amount) {
            return at(
                "amount",
                format!(
                    "a prior limit is a canonical integer, not {}: {e}",
                    quoted(&prior.amount)
                ),
            );
        }
        let key = (
            prior.logical_binding_key.as_str(),
            prior.metric_ref.as_str(),
        );
        (!seen.insert(key)).then(|| {
            (
                format!("prior[{i}]"),
                format!(
                    "the prior limit of binding {} and metric {} is named twice",
                    quoted(key.0),
                    quoted(key.1)
                ),
            )
        })
    })
}

/// A rule whose definition resolved, as the set checks see it.
struct Judged<'a> {
    rule: &'a QuotaSetRule,
    resolved: &'a ResolvedDefinition,
    /// The final limit in the canonical unit; `None` when the amount or the final limit did not
    /// pass, or does not convert exactly.
    canonical: Option<u128>,
}

impl Judged<'_> {
    fn definition(&self) -> &QuotaDefinition {
        &self.resolved.definition
    }
}

/// What the amount check found: the final limit in the rule's unit and in the canonical unit.
#[derive(Default, Clone, Copy)]
struct Limits {
    final_limit: Option<u128>,
    canonical: Option<u128>,
}

/// Judge the set against what was resolved (P-D-276): one verdict per rule in request order and
/// every check found, rule checks in rule order, then set checks. Never stops at the first
/// failure. Crate-private: its one caller asks [`request_refusal`] first, and a quantity that
/// does not read would be judged as none.
// @cpt-algo:cpt-cf-bss-products-algo-quota-set-rule:p1
#[must_use]
pub(crate) fn judge(
    request: &QuotaSetRequest,
    resolved: &BTreeMap<(Uuid, QuotaVersion), ResolvedDefinition>,
) -> QuotaSetValidation {
    let quantity = request
        .quantity
        .as_deref()
        .and_then(|q| Amount::parse(q).ok());
    let mut checks = Vec::new();
    let mut verdicts = Vec::with_capacity(request.rules.len());
    let mut judged = Vec::with_capacity(request.rules.len());
    for rule in &request.rules {
        let Some(found) = rule_definition(rule, resolved, &mut checks) else {
            verdicts.push(QuotaRuleVerdict {
                rule_id: rule.rule_id,
                definition: None,
                limit: None,
            });
            continue;
        };
        let definition = &found.definition;
        rule_applicability(rule, definition, &mut checks);
        let limits = rule_unit(rule, definition, &mut checks)
            .map(|unit| rule_amount(rule, unit, quantity, &mut checks))
            .unwrap_or_default();
        rule_scope(rule, definition, &mut checks);
        rule_availability(rule, definition, &mut checks);
        verdicts.push(QuotaRuleVerdict {
            rule_id: rule.rule_id,
            definition: Some(definition.clone()),
            limit: quantity
                .and(limits.final_limit)
                .map(|limit| limit.to_string()),
        });
        judged.push(Judged {
            rule,
            resolved: found,
            canonical: limits.canonical,
        });
    }
    judge_set(request, &judged, &mut checks);
    QuotaSetValidation {
        rules: verdicts,
        checks,
    }
}

/// A check about `rule_ids` on `field`.
fn check(
    code: QuotaCheckCode,
    rule_ids: Vec<Uuid>,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> QuotaCheck {
    QuotaCheck {
        code,
        rule_ids,
        field: field.into(),
        detail: detail.into(),
    }
}

/// `definition {id} version {version}`, as the details name a version.
fn named(rule: &QuotaSetRule) -> String {
    format!(
        "definition {} version {}",
        rule.definition.id, rule.definition.version
    )
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-definition
/// The exact version is in the caller's scope, else `QUOTA_RULE_INVALID` on `definition_ref` and
/// the rule's other checks are skipped; its stored digest is the rule's, else
/// `QUOTA_DEFINITION_CHANGED`.
fn rule_definition<'a>(
    rule: &QuotaSetRule,
    resolved: &'a BTreeMap<(Uuid, QuotaVersion), ResolvedDefinition>,
    checks: &mut Vec<QuotaCheck>,
) -> Option<&'a ResolvedDefinition> {
    let Some(found) = resolved.get(&(rule.definition.id, rule.definition.version)) else {
        checks.push(check(
            QuotaCheckCode::RuleInvalid,
            vec![rule.rule_id],
            "definition_ref",
            format!(
                "no quota definition {} version {} in the catalog",
                rule.definition.id, rule.definition.version
            ),
        ));
        return None;
    };
    if found.definition.definition_digest != rule.definition.digest {
        checks.push(check(
            QuotaCheckCode::DefinitionChanged,
            vec![rule.rule_id],
            "definition_ref.digest",
            format!(
                "the stored digest of {} is {}",
                named(rule),
                found.definition.definition_digest
            ),
        ));
    }
    Some(found)
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-definition

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-applicability
/// The definition's SKU is the rule's, approved at the rule's SKU version or an earlier one
/// (P-D-270), else `QUOTA_RULE_INVALID` on `definition_ref`.
fn rule_applicability(
    rule: &QuotaSetRule,
    definition: &QuotaDefinition,
    checks: &mut Vec<QuotaCheck>,
) {
    if rule.sku_id != definition.sku_id || rule.sku_version < definition.sku_version {
        checks.push(check(
            QuotaCheckCode::RuleInvalid,
            vec![rule.rule_id],
            "definition_ref",
            format!(
                "{} applies to SKU {} from version {}",
                named(rule),
                definition.sku_id,
                definition.sku_version
            ),
        ));
    }
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-applicability

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-unit
/// The basis is one of the definition's, else `QUOTA_RULE_INVALID` on `basis`; the unit is one
/// of its allowed units, else `QUOTA_RULE_INVALID` on `unit_ref` and the amount is not judged.
fn rule_unit<'a>(
    rule: &QuotaSetRule,
    definition: &'a QuotaDefinition,
    checks: &mut Vec<QuotaCheck>,
) -> Option<&'a QuotaAllowedUnit> {
    if !definition.supported_bases.contains(&rule.basis) {
        checks.push(check(
            QuotaCheckCode::RuleInvalid,
            vec![rule.rule_id],
            "basis",
            format!("{} takes no {} basis", named(rule), basis_token(rule.basis)),
        ));
    }
    let unit = definition
        .allowed_units
        .iter()
        .find(|unit| unit.unit_ref == rule.unit_ref);
    if unit.is_none() {
        checks.push(check(
            QuotaCheckCode::RuleInvalid,
            vec![rule.rule_id],
            "unit_ref",
            format!(
                "{} is not an allowed unit of {}",
                quoted(&rule.unit_ref),
                named(rule)
            ),
        ));
    }
    unit
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-unit

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-amount
/// A unit's factor into the canonical unit; a factor that does not read names its unit.
fn unit_factor(unit: &QuotaAllowedUnit) -> Result<Factor, String> {
    Factor::parse(&unit.numerator, &unit.denominator).map_err(|e| {
        format!(
            "the catalog's factor of {} does not read: {e}",
            unit.unit_ref
        )
    })
}

/// Every allowed unit's stored grid and factor read, as a resolved definition's must: a text
/// that does not read is the catalog's defect, a corrupt row (P-D-275), not the author's.
///
/// # Errors
/// The first text that does not read, naming its unit and member.
pub(crate) fn units_read(definition: &QuotaDefinition) -> Result<(), String> {
    definition
        .allowed_units
        .iter()
        .try_for_each(|unit| Grid::of(unit).map(|_| ()))
}

/// An allowed unit's grid, `min + n * step` up to `max`, and its factor into the canonical unit.
struct Grid {
    min: u128,
    step: u128,
    max: Option<u128>,
    factor: Factor,
}

impl Grid {
    /// The unit's stored bounds and factor; a text that does not read names itself.
    fn of(unit: &QuotaAllowedUnit) -> Result<Self, String> {
        let bound = |name: &str, text: &str| {
            Amount::parse(text).map(Amount::get).map_err(|e| {
                format!(
                    "the catalog's {name} of {} does not read: {e}",
                    unit.unit_ref
                )
            })
        };
        Ok(Self {
            min: bound("min", &unit.min)?,
            step: bound("step", &unit.step)?,
            max: unit
                .max
                .as_deref()
                .map(|max| bound("max", max))
                .transpose()?,
            factor: unit_factor(unit)?,
        })
    }

    /// Why `value` is off the grid; `None` when it is on it.
    fn refusal(&self, value: u128) -> Option<String> {
        let (min, step) = (self.min, self.step);
        let Some(offset) = value.checked_sub(min) else {
            return Some(format!("{value} is below the minimum {min}"));
        };
        // A step of zero leaves the minimum alone on the grid.
        let on_step = offset
            .checked_rem(step)
            .map_or(offset == 0, |rest| rest == 0);
        if !on_step {
            return Some(format!("{value} is not on the grid {min} + n * {step}"));
        }
        match self.max {
            Some(max) if value > max => Some(format!("{value} is above the maximum {max}")),
            Some(_) | None => None,
        }
    }
}

/// The amount is a canonical integer on the unit's grid, else `QUOTA_AMOUNT_INVALID` on
/// `amount`; it converts exactly into the canonical unit, else `QUOTA_UNIT_INEXACT` on `amount`.
/// Checked `u128` arithmetic only: an overflow is `QUOTA_AMOUNT_INVALID`.
fn rule_amount(
    rule: &QuotaSetRule,
    unit: &QuotaAllowedUnit,
    quantity: Option<Amount>,
    checks: &mut Vec<QuotaCheck>,
) -> Limits {
    let invalid = |detail: String| {
        check(
            QuotaCheckCode::AmountInvalid,
            vec![rule.rule_id],
            "amount",
            detail,
        )
    };
    let amount = match Amount::parse(&rule.amount) {
        Ok(amount) => amount,
        Err(e) => {
            checks.push(invalid(format!("{}: {e}", quoted(&rule.amount))));
            return Limits::default();
        }
    };
    let grid = match Grid::of(unit) {
        Ok(grid) => grid,
        Err(why) => {
            checks.push(invalid(why));
            return Limits::default();
        }
    };
    let refused = grid.refusal(amount.get());
    if let Some(why) = &refused {
        checks.push(invalid(why.clone()));
    }
    let canonical = match convert(amount, grid.factor, Factor::ONE) {
        Ok(canonical) => Some(canonical.get()),
        Err(UnitError::Inexact) => {
            checks.push(check(
                QuotaCheckCode::UnitInexact,
                vec![rule.rule_id],
                "amount",
                format!(
                    "{amount} {} is not a whole amount of the canonical unit",
                    quoted(&rule.unit_ref)
                ),
            ));
            None
        }
        Err(e) => {
            if refused.is_none() {
                checks.push(invalid(format!("{amount}: {e}")));
            }
            return Limits::default();
        }
    };
    if refused.is_some() {
        return Limits::default();
    }
    final_limit(rule, (amount.get(), canonical), &grid, quantity, checks)
}

/// The final limit `L`: the amount for `Fixed`, the amount times the quantity for `PerPlanUnit`
/// (1 without a quantity). Off the grid, above `max` or beyond 128 bits it is
/// `QUOTA_AMOUNT_INVALID` on `quantity`. Its canonical value is the amount's times the same
/// factor, `None` when the amount does not convert exactly (its own check names it).
fn final_limit(
    rule: &QuotaSetRule,
    (amount, canonical): (u128, Option<u128>),
    grid: &Grid,
    quantity: Option<Amount>,
    checks: &mut Vec<QuotaCheck>,
) -> Limits {
    let times = match rule.basis {
        QuotaBasis::Fixed => 1,
        QuotaBasis::PerPlanUnit => quantity.map_or(1, Amount::get),
    };
    let value = amount.checked_mul(times);
    let canonical_value = canonical.map(|c| c.checked_mul(times));
    let why = match (value, canonical_value) {
        (Some(value), None | Some(Some(_))) => match grid.refusal(value) {
            None => {
                return Limits {
                    final_limit: Some(value),
                    canonical: canonical_value.flatten(),
                };
            }
            Some(why) => format!("at quantity {times}, {why}"),
        },
        (None, _) | (Some(_), Some(None)) => {
            format!("{amount} times {times} does not fit in 128 bits")
        }
    };
    checks.push(check(
        QuotaCheckCode::AmountInvalid,
        vec![rule.rule_id],
        QUANTITY_FIELD,
        why,
    ));
    Limits::default()
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-amount

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-scope
/// The wire token of a subject selector, as the scope policy spells it.
const fn subject_token(selector: SubjectSelector) -> &'static str {
    match selector {
        SubjectSelector::ResourceOwner => "resource_owner",
    }
}

/// The wire token of a scope dimension, as the scope policy spells it.
const fn dimension_token(dimension: ScopeDimension) -> &'static str {
    match dimension {
        ScopeDimension::Region => "region",
        ScopeDimension::ResourcePool => "resource_pool",
    }
}

/// The subject selector is the scope policy's, else `QUOTA_SCOPE_UNSUPPORTED` on
/// `subject_selector`; the scope selection names exactly the policy's required dimensions, each
/// with a value of 1 to 255 printable ASCII bytes, else `QUOTA_SCOPE_UNSUPPORTED` on
/// `scope_selection.<key>`: the required keys in order, then the extra ones.
fn rule_scope(rule: &QuotaSetRule, definition: &QuotaDefinition, checks: &mut Vec<QuotaCheck>) {
    let unsupported = |field: String, detail: String| {
        check(
            QuotaCheckCode::ScopeUnsupported,
            vec![rule.rule_id],
            field,
            detail,
        )
    };
    let policy = &definition.scope_policy;
    let selector = subject_token(policy.subject_selector);
    if rule.subject_selector != selector {
        checks.push(unsupported(
            "subject_selector".to_owned(),
            format!(
                "the scope policy selects {selector}, not {}",
                quoted(&rule.subject_selector)
            ),
        ));
    }
    let required: BTreeSet<&str> = policy
        .required_dimensions
        .iter()
        .map(|dimension| dimension_token(*dimension))
        .collect();
    for key in &required {
        let detail = match rule.scope_selection.get(*key) {
            None => format!("the scope policy requires a {key}"),
            Some(value) if !scope_value_fits(value) => {
                format!("a {key} is 1 to {MAX_SCOPE_VALUE} printable ASCII bytes")
            }
            Some(_) => continue,
        };
        checks.push(unsupported(format!("scope_selection.{key}"), detail));
    }
    for key in rule
        .scope_selection
        .keys()
        .filter(|key| !required.contains(key.as_str()))
    {
        checks.push(unsupported(
            format!("scope_selection.{key}"),
            format!("the scope policy has no dimension {}", quoted(key)),
        ));
    }
}

/// 1 to 255 printable ASCII bytes.
fn scope_value_fits(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_VALUE
        && value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-scope

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-availability
/// A version that is not enabled is `QUOTA_QUALIFICATION_UNAVAILABLE` on `definition_ref`, with
/// its state and reason (`disabled: qualification_pending`).
fn rule_availability(
    rule: &QuotaSetRule,
    definition: &QuotaDefinition,
    checks: &mut Vec<QuotaCheck>,
) {
    let availability = &definition.availability;
    let state = match availability.state {
        QuotaAvailabilityState::Enabled => return,
        QuotaAvailabilityState::Disabled => "disabled",
        QuotaAvailabilityState::Withdrawn => "withdrawn",
    };
    let detail = availability
        .reason_code
        .as_ref()
        .map_or_else(|| state.to_owned(), |reason| format!("{state}: {reason}"));
    checks.push(check(
        QuotaCheckCode::QualificationUnavailable,
        vec![rule.rule_id],
        "definition_ref",
        detail,
    ));
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-rule:p1:inst-quota-rule-availability

/// The set checks, among the rules whose definition resolved (P-D-276): one profile content and
/// one adapter target per binding, one scope per binding, and the change kind.
// @cpt-algo:cpt-cf-bss-products-algo-quota-set-whole:p1
fn judge_set(request: &QuotaSetRequest, judged: &[Judged<'_>], checks: &mut Vec<QuotaCheck>) {
    set_compatible(judged, checks);
    set_scope(judged, checks);
    set_change(request, judged, checks);
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-compatible
/// The rules in groups by `key`, groups in key order, rules in request order.
fn groups<'a, 'r, K: Ord>(
    judged: &'a [Judged<'r>],
    key: impl Fn(&Judged<'r>) -> K,
) -> BTreeMap<K, Vec<&'a Judged<'r>>> {
    let mut groups: BTreeMap<K, Vec<&Judged<'r>>> = BTreeMap::new();
    for one in judged {
        groups.entry(key(one)).or_default().push(one);
    }
    groups
}

/// The rule ids of `rules`, in request order.
fn ids(rules: &[&Judged<'_>]) -> Vec<Uuid> {
    rules.iter().map(|one| one.rule.rule_id).collect()
}

/// A profile's content without its change support, which a binding's rules may differ in.
fn profile_content(profile: &Value) -> Value {
    let mut content = profile.clone();
    if let Some(members) = content.as_object_mut() {
        members.remove("change_support");
    }
    content
}

/// The rules of one logical binding share the profile's content and one adapter target, else
/// one `QUOTA_SET_INCOMPATIBLE` on `logical_binding_key` naming every rule of the binding.
fn set_compatible(judged: &[Judged<'_>], checks: &mut Vec<QuotaCheck>) {
    for (binding, rules) in groups(judged, |one| one.rule.logical_binding_key.as_str()) {
        let Some((first, rest)) = rules.split_first() else {
            continue;
        };
        let content = profile_content(&first.resolved.profile);
        let apart = rest.iter().any(|one| {
            one.resolved.target_id != first.resolved.target_id
                || profile_content(&one.resolved.profile) != content
        });
        if apart {
            checks.push(check(
                QuotaCheckCode::SetIncompatible,
                ids(&rules),
                "logical_binding_key",
                format!(
                    "the rules of binding {} need one profile and one adapter target",
                    quoted(binding)
                ),
            ));
        }
    }
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-compatible

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-scope
/// The rules of one logical binding share one subject selector and one scope selection, whatever
/// their item or dimension value: a binding installs one target and one scope. Else one
/// `QUOTA_SET_INCOMPATIBLE` on `scope_selection` naming every rule of the binding.
fn set_scope(judged: &[Judged<'_>], checks: &mut Vec<QuotaCheck>) {
    for (binding, rules) in groups(judged, |one| one.rule.logical_binding_key.as_str()) {
        let Some((first, rest)) = rules.split_first() else {
            continue;
        };
        let apart = rest.iter().any(|one| {
            one.rule.subject_selector != first.rule.subject_selector
                || one.rule.scope_selection != first.rule.scope_selection
        });
        if apart {
            checks.push(check(
                QuotaCheckCode::SetIncompatible,
                ids(&rules),
                "scope_selection",
                format!(
                    "the rules of binding {} need one subject and one scope",
                    quoted(binding)
                ),
            ));
        }
    }
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-scope

// @cpt-begin:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-change
/// An initial set needs initial-set support. An increase or an unchanged set needs a prior limit
/// for each rule's binding and metric and a rule for each prior limit (else on `prior`). An
/// increase needs increase support and every final limit at least its prior one with one above
/// it; a decrease is refused, and "nothing increases" is reported only without a decrease. An
/// unchanged set needs equal limits. Limits are compared in the canonical unit; a failure is
/// `QUOTA_SET_INCOMPATIBLE` on `change_kind`.
fn set_change(request: &QuotaSetRequest, judged: &[Judged<'_>], checks: &mut Vec<QuotaCheck>) {
    let unsupported = |supported: fn(&QuotaDefinition) -> bool| -> Vec<Uuid> {
        judged
            .iter()
            .filter(|one| !supported(one.definition()))
            .map(|one| one.rule.rule_id)
            .collect()
    };
    let change = |rule_ids: Vec<Uuid>, detail: &str| {
        check(
            QuotaCheckCode::SetIncompatible,
            rule_ids,
            "change_kind",
            detail,
        )
    };
    let compared = compare_with_prior(request, judged, checks);
    match request.change_kind {
        QuotaChangeKind::InitialSet => {
            let without = unsupported(|d| d.change_support.initial_set);
            if !without.is_empty() {
                checks.push(change(
                    without,
                    "the profile does not support an initial set",
                ));
            }
        }
        QuotaChangeKind::Increase => {
            let without = unsupported(|d| d.change_support.increase);
            if !without.is_empty() {
                checks.push(change(without, "the profile does not support an increase"));
            }
            if !compared.below.is_empty() {
                checks.push(change(compared.below, "a cap decrease is refused"));
            } else if compared.above.is_empty() && !compared.equal.is_empty() {
                checks.push(change(
                    compared.equal,
                    "nothing increases: the change is unchanged",
                ));
            }
        }
        QuotaChangeKind::Unchanged => {
            let mut moved = compared.below;
            moved.extend(compared.above);
            if !moved.is_empty() {
                checks.push(change(moved, "an unchanged set keeps every limit as it is"));
            }
        }
    }
}

/// The rules whose final limit is below, above or equal to its prior one.
#[derive(Default)]
struct Compared {
    below: Vec<Uuid>,
    above: Vec<Uuid>,
    equal: Vec<Uuid>,
}

/// Each rule's final limit against the prior limit of its binding and metric, in the canonical
/// unit; a rule without one, or with one that does not read, is `QUOTA_SET_INCOMPATIBLE` on
/// `prior`, and so is a prior limit that no rule keeps (in prior order, naming no rule), unless a
/// rule of its binding did not resolve. An initial set names no prior limit and compares nothing.
fn compare_with_prior(
    request: &QuotaSetRequest,
    judged: &[Judged<'_>],
    checks: &mut Vec<QuotaCheck>,
) -> Compared {
    let mut compared = Compared::default();
    if request.change_kind == QuotaChangeKind::InitialSet {
        return compared;
    }
    let mut priors: BTreeMap<(&str, &str), &QuotaPriorLimit> = BTreeMap::new();
    for prior in &request.prior {
        priors
            .entry((
                prior.logical_binding_key.as_str(),
                prior.metric_ref.as_str(),
            ))
            .or_insert(prior);
    }
    for one in judged {
        let (rule, definition) = (one.rule, one.definition());
        let key = (
            rule.logical_binding_key.as_str(),
            definition.metric_ref.as_str(),
        );
        let prior = priors
            .get(&key)
            .ok_or_else(|| {
                format!(
                    "no prior limit for binding {} and metric {}",
                    quoted(key.0),
                    key.1
                )
            })
            .and_then(|prior| prior_in_canonical(prior, definition));
        let prior = match prior {
            Ok(prior) => prior,
            Err(why) => {
                checks.push(check(
                    QuotaCheckCode::SetIncompatible,
                    vec![rule.rule_id],
                    "prior",
                    why,
                ));
                continue;
            }
        };
        // A rule whose own limit did not pass is named by its own check.
        let Some(limit) = one.canonical else {
            continue;
        };
        let side = match limit.cmp(&prior) {
            std::cmp::Ordering::Less => &mut compared.below,
            std::cmp::Ordering::Greater => &mut compared.above,
            std::cmp::Ordering::Equal => &mut compared.equal,
        };
        side.push(rule.rule_id);
    }
    // A prior limit that no rule keeps would be dropped, an installed cap removed: the check
    // names no rule, and its detail names the prior limit's binding and metric. A rule whose
    // definition did not resolve has its own check and an unknown metric, so a prior limit of its
    // binding is not reported a second time (one failure per cause).
    let unresolved: BTreeSet<&str> = request
        .rules
        .iter()
        .filter(|rule| !judged.iter().any(|one| one.rule.rule_id == rule.rule_id))
        .map(|rule| rule.logical_binding_key.as_str())
        .collect();
    let kept: BTreeSet<(&str, &str)> = judged
        .iter()
        .map(|one| {
            (
                one.rule.logical_binding_key.as_str(),
                one.definition().metric_ref.as_str(),
            )
        })
        .collect();
    for prior in &request.prior {
        let key = (
            prior.logical_binding_key.as_str(),
            prior.metric_ref.as_str(),
        );
        if !kept.contains(&key) && !unresolved.contains(key.0) {
            checks.push(check(
                QuotaCheckCode::SetIncompatible,
                Vec::new(),
                "prior",
                format!(
                    "no rule keeps the prior limit of binding {} and metric {}",
                    quoted(key.0),
                    quoted(key.1)
                ),
            ));
        }
    }
    compared
}

/// A prior limit in the canonical unit: stated in the canonical unit or in an allowed unit.
fn prior_in_canonical(
    prior: &QuotaPriorLimit,
    definition: &QuotaDefinition,
) -> Result<u128, String> {
    let amount = Amount::parse(&prior.amount)
        .map_err(|e| format!("the prior limit {}: {e}", quoted(&prior.amount)))?;
    let factor = if prior.unit_ref == definition.canonical_unit_ref {
        Factor::ONE
    } else {
        let unit = definition
            .allowed_units
            .iter()
            .find(|unit| unit.unit_ref == prior.unit_ref)
            .ok_or_else(|| {
                format!(
                    "{} is not a unit of {}",
                    quoted(&prior.unit_ref),
                    definition.metric_ref
                )
            })?;
        unit_factor(unit)?
    };
    convert(amount, factor, Factor::ONE)
        .map(Amount::get)
        .map_err(|e| format!("the prior limit {amount} {}: {e}", quoted(&prior.unit_ref)))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-set-whole:p1:inst-quota-set-change

#[cfg(test)]
#[path = "set_tests.rs"]
mod set_tests;

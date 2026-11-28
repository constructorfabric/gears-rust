//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-authoring:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-checks:p1
//! Plan quota rules as Pricing judges them (D-541, D-542, D-543, D-544).
//!
//! - [`parse_rules`] reads a request body `{"quota_rules": [...]}` into the v1 shape, or every
//!   violation it finds; [`rule_violations`] judges typed rules (the SDK's) with the same codes, so
//!   both surfaces refuse alike (D-540).
//! - [`item_violations`], [`version_violations`] and [`duplicate_violations`] are Pricing's own
//!   checks of a set against its revision; [`catalog_violations`] turns Products' checks
//!   (products P-D-276) into violations of the rules they name, and [`unjudged`] finds the rules
//!   Products was sent but did not judge against the definition each names (its contract break).
//! - [`refusal_status`] is the status of a refusal of them, and [`check_rows`] the quota rows of
//!   a revision's checks, which also show a sellable cell that selects no rule of a metric the
//!   item's rules carry (`QUOTA_RULE_MISSING`, D-547).
//!
//! The parse also keeps every text within the bounds that Products' validation refuses as a
//! malformed request (`bss_products_sdk::quota::validation`), so a body that parses is never a
//! Products 400; the caps Pricing keeps of its own are tied to Products' bounds at compile time.
use super::plan::{Check, Item, PlanContext, Subject};
use super::price;
use bss_pricing_sdk::Digest;
use bss_pricing_sdk::digest::{CoverageRule, uncovered_metrics};
use bss_pricing_sdk::plan_quota::{
    BINDING_KEY_MAX, MAX_RULES, PlanQuotaBasis, PlanQuotaRuleV1, QuotaDefinitionRef,
    QuotaSubjectSelector, SCOPE_KEYS_MAX, SCOPE_VALUE_MAX, binding_key_is_valid,
};
use bss_pricing_sdk::read::{BindingSelection, UncoveredQuota};
use bss_products_sdk::quota::catalog::{QuotaDefinition, QuotaVersion};
use bss_products_sdk::quota::units::Amount;
use bss_products_sdk::quota::validation::{
    self as products, MAX_REF_BYTES, MAX_TEXT_BYTES, QuotaCheckCode, QuotaSetValidation,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroU64;
use time::Date;
use uuid::Uuid;

/// The body's member, and the path of a violation of the whole set.
const COLLECTION: &str = "quota_rules";
/// A rule, a member or the body that is not what the contract says.
const RULE_INVALID: &str = "QUOTA_RULE_INVALID";
/// A subject selector or a scope selection the contract does not take.
const SCOPE_UNSUPPORTED: &str = "QUOTA_SCOPE_UNSUPPORTED";
/// An amount that is not a canonical decimal integer.
const AMOUNT_INVALID: &str = "QUOTA_AMOUNT_INVALID";
/// Two rules for one binding and metric that apply together.
const RULE_DUPLICATE: &str = "QUOTA_RULE_DUPLICATE";
/// A rule's digest that is not the stored one (Products' check).
const DEFINITION_CHANGED: &str = "QUOTA_DEFINITION_CHANGED";
/// An amount that does not convert exactly into the canonical unit (Products' check).
const UNIT_INEXACT: &str = "QUOTA_UNIT_INEXACT";
/// Rules that do not fit together (Products' check).
const SET_INCOMPATIBLE: &str = "QUOTA_SET_INCOMPATIBLE";
/// A definition version that is not enabled for sale (Products' check).
const QUALIFICATION_UNAVAILABLE: &str = "QUOTA_QUALIFICATION_UNAVAILABLE";
/// A sellable cell that selects no rule of a metric the item's rules carry (D-547).
const RULE_MISSING: &str = "QUOTA_RULE_MISSING";
/// The members of a rule, in the order its violations are reported.
const MEMBERS: [&str; 10] = [
    "rule_id",
    "definition_ref",
    "item_id",
    "dimension_value",
    "logical_binding_key",
    "subject_selector",
    "scope_selection",
    "amount",
    "unit_ref",
    "basis",
];
/// The members of a definition reference.
const REFERENCE_MEMBERS: [&str; 3] = ["id", "version", "digest"];
/// The most characters of a caller's text that a detail repeats.
const ECHO_CHARS: usize = 32;
/// The most unknown members of one object (the body, or one rule) that a refusal names one by
/// one; one more violation counts the rest (D-541).
const UNKNOWN_NAMED: usize = 10;

// Pricing's caps are within the bounds Products' validation refuses as a malformed request, so a
// body that parses is never a Products 400 (D-541): the rules per set, the binding key (and every
// scope key, which follows the binding key rule) and the keys per scope selection. A scope value
// takes Products' own bound, so Pricing's form check is Products'.
const _: () = assert!(MAX_RULES <= products::MAX_RULES);
const _: () = assert!(BINDING_KEY_MAX <= products::MAX_KEY_BYTES);
const _: () = assert!(SCOPE_KEYS_MAX <= products::MAX_SCOPE_KEYS);
const _: () = assert!(SCOPE_VALUE_MAX == products::MAX_SCOPE_VALUE_BYTES);

/// The codes a save may not ignore are every code but availability (D-542).
pub const SAVE_IGNORES: &str = QUALIFICATION_UNAVAILABLE;
/// The codes of a conflict with the current state (D-543): a refusal of only these is 409.
pub const CONFLICT_CODES: [&str; 2] = [RULE_DUPLICATE, DEFINITION_CHANGED];

/// One refusal or red check of a rule (D-543, D-544).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub code: &'static str,
    /// The rule's position in the rules judged (the request body's, or the stored order); `None`
    /// for the whole set.
    pub index: Option<usize>,
    /// The rule, when its id is known.
    pub rule_id: Option<Uuid>,
    /// The rule's top-level member (`amount`, `scope_selection`, ...); empty for the rule itself
    /// (an unknown member, named in `detail`); `quota_rules` for the whole set.
    pub field: &'static str,
    pub detail: String,
    /// The item a line of the whole set is about (an uncovered sellable cell, D-544); `None` for
    /// a rule's violation, which names its rule, and for any other line of the set.
    pub item: Option<Uuid>,
}

impl Violation {
    /// `quota_rules[<i>].<field>`, `quota_rules[<i>]` when `field` is empty, or `quota_rules`.
    #[must_use]
    pub fn path(&self) -> String {
        match (self.index, self.field) {
            (None, _) => COLLECTION.to_owned(),
            (Some(index), "") => format!("{COLLECTION}[{index}]"),
            (Some(index), field) => format!("{COLLECTION}[{index}].{field}"),
        }
    }

    /// A violation of the whole set.
    fn set(code: &'static str, detail: String) -> Self {
        Self {
            code,
            index: None,
            rule_id: None,
            field: COLLECTION,
            detail,
            item: None,
        }
    }

    /// A violation of the whole set about one item.
    fn set_of_item(code: &'static str, item: Uuid, detail: String) -> Self {
        Self {
            item: Some(item),
            ..Self::set(code, detail)
        }
    }
}

/// A rule item as the judgement sees it: its SKU and the dimension key of its entry.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleItem {
    pub id: Uuid,
    pub sku_id: Uuid,
    pub dimension_key: Option<String>,
}

/// A member's failure: its code and why.
type Fault = (&'static str, String);

/// The violations of one rule, at its position.
struct RuleFaults<'a> {
    index: usize,
    rule_id: Option<Uuid>,
    out: &'a mut Vec<Violation>,
}

impl RuleFaults<'_> {
    fn add(&mut self, field: &'static str, (code, detail): Fault) {
        self.out.push(Violation {
            code,
            index: Some(self.index),
            rule_id: self.rule_id,
            field,
            detail,
            item: None,
        });
    }

    fn check(&mut self, field: &'static str, fault: Option<Fault>) {
        if let Some(fault) = fault {
            self.add(field, fault);
        }
    }
}

/// A caller's text as a detail repeats it: escaped, and at most [`ECHO_CHARS`] characters.
fn quoted(text: &str) -> String {
    let shown: String = text.chars().take(ECHO_CHARS).collect();
    if shown.len() < text.len() {
        format!("{shown:?}...")
    } else {
        format!("{shown:?}")
    }
}

fn nul_fault(member: &str, text: &str) -> Option<Fault> {
    text.contains('\0')
        .then(|| (RULE_INVALID, format!("{member} holds U+0000")))
}

/// The binding key rule (D-541).
fn binding_key_fault(key: &str) -> Option<Fault> {
    nul_fault("logical_binding_key", key).or_else(|| {
        (!binding_key_is_valid(key)).then(|| {
            (
                RULE_INVALID,
                format!(
                    "a logical binding key is 1 to 64 of ASCII letters, digits, _, - and ., not {}",
                    quoted(key)
                ),
            )
        })
    })
}

/// A scope selection: at most [`SCOPE_KEYS_MAX`] keys, each of the binding key rule, each value 1
/// to [`SCOPE_VALUE_MAX`] printable ASCII bytes (D-541).
fn scope_fault(scope: &BTreeMap<String, String>) -> Option<Fault> {
    if let Some(fault) = scope.iter().find_map(|(key, value)| {
        nul_fault("scope_selection", key).or_else(|| nul_fault("scope_selection", value))
    }) {
        return Some(fault);
    }
    if scope.len() > SCOPE_KEYS_MAX {
        return Some((
            SCOPE_UNSUPPORTED,
            format!(
                "a scope selection names at most {SCOPE_KEYS_MAX} keys, not {}",
                scope.len()
            ),
        ));
    }
    scope.iter().find_map(|(key, value)| {
        if !binding_key_is_valid(key) {
            return Some((
                SCOPE_UNSUPPORTED,
                format!(
                    "a scope key is 1 to 64 of ASCII letters, digits, _, - and ., not {}",
                    quoted(key)
                ),
            ));
        }
        let printable = value.bytes().all(|b| (b' '..=b'~').contains(&b));
        (value.is_empty() || value.len() > SCOPE_VALUE_MAX || !printable).then(|| {
            (
                SCOPE_UNSUPPORTED,
                format!(
                    "the value of {} is 1 to {SCOPE_VALUE_MAX} printable ASCII bytes, not {}",
                    quoted(key),
                    quoted(value)
                ),
            )
        })
    })
}

/// A canonical decimal integer (D-541): `bss_products_sdk::quota::units::Amount::parse`.
fn amount_fault(amount: &str) -> Option<Fault> {
    nul_fault("amount", amount).or_else(|| {
        Amount::parse(amount).is_err().then(|| {
            (
                AMOUNT_INVALID,
                format!(
                    "an amount is a canonical decimal integer of at most 39 digits, not {}",
                    quoted(amount)
                ),
            )
        })
    })
}

/// A unit reference: 1 to Products' reference bound of bytes, the longest GTS id the catalog
/// stores.
fn unit_fault(unit: &str) -> Option<Fault> {
    nul_fault("unit_ref", unit).or_else(|| {
        (unit.is_empty() || unit.len() > MAX_REF_BYTES).then(|| {
            (
                RULE_INVALID,
                format!("a unit reference is 1 to {MAX_REF_BYTES} bytes"),
            )
        })
    })
}

/// A dimension value: at most Products' text bound of bytes; the item check judges its value.
fn dimension_fault(value: Option<&str>) -> Option<Fault> {
    let value = value?;
    nul_fault("dimension_value", value).or_else(|| {
        (value.len() > MAX_TEXT_BYTES).then(|| {
            (
                RULE_INVALID,
                format!("a dimension value is at most {MAX_TEXT_BYTES} bytes"),
            )
        })
    })
}

/// More rules than one revision holds.
fn too_many(count: usize) -> Option<Violation> {
    (count > MAX_RULES).then(|| {
        Violation::set(
            RULE_INVALID,
            format!("a revision holds at most {MAX_RULES} rules, not {count}"),
        )
    })
}

/// A rule id that an earlier rule of the set already takes.
fn repeated(rule_id: Uuid) -> Fault {
    (RULE_INVALID, format!("rule {rule_id} is named twice"))
}

/// Judge typed rules (the SDK's `replace`) with the codes the parse gives the same values
/// (D-540, D-541): at most 200 rules, each id once, the binding key rule, the scope selection,
/// canonical amounts, the text bounds and no U+0000. The typed members that cannot hold a wrong
/// value (the definition reference, the subject and the basis) are not judged.
#[must_use]
pub fn rule_violations(rules: &[PlanQuotaRuleV1]) -> Vec<Violation> {
    if let Some(violation) = too_many(rules.len()) {
        return vec![violation];
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (index, rule) in rules.iter().enumerate() {
        let mut faults = RuleFaults {
            index,
            rule_id: Some(rule.rule_id),
            out: &mut out,
        };
        if !seen.insert(rule.rule_id) {
            faults.add("rule_id", repeated(rule.rule_id));
        }
        faults.check(
            "dimension_value",
            dimension_fault(rule.dimension_value.as_deref()),
        );
        faults.check(
            "logical_binding_key",
            binding_key_fault(&rule.logical_binding_key),
        );
        faults.check("scope_selection", scope_fault(&rule.scope_selection));
        faults.check("amount", amount_fault(&rule.amount));
        faults.check("unit_ref", unit_fault(&rule.unit_ref));
    }
    out
}

/// The rule member a Products field path names (`scope_selection.region` → `scope_selection`,
/// `definition_ref.digest` → `definition_ref`). A Products field that names no rule member maps
/// to the member it is about (D-543): `change_kind` to `definition_ref`, `prior` and `quantity`
/// to `amount`. Any other path names the rule itself (empty).
#[must_use]
pub fn top_field(path: &str) -> &'static str {
    let head = path.split(['.', '[']).next().unwrap_or(path);
    match head {
        "change_kind" => "definition_ref",
        "prior" | "quantity" => "amount",
        _ => MEMBERS
            .iter()
            .copied()
            .find(|member| *member == head)
            .unwrap_or(""),
    }
}

/// The details of `object`'s unknown members (those `known` does not take) of `owner` (`the body`,
/// `a rule`), in key order: the first [`UNKNOWN_NAMED`] by name, then one that counts the rest.
/// Past the named ones each key is only counted, so a body of many stray members builds a small
/// refusal.
fn unknown_members(
    object: &Map<String, Value>,
    known: impl Fn(&str) -> bool,
    owner: &str,
) -> Vec<String> {
    let mut unknown = object.keys().filter(|key| !known(key));
    let mut out: Vec<String> = unknown
        .by_ref()
        .take(UNKNOWN_NAMED)
        .map(|key| format!("{} is not a member of {owner}", quoted(key)))
        .collect();
    let rest = unknown.count();
    if rest > 0 {
        out.push(format!("{rest} more members are not members of {owner}"));
    }
    out
}

/// The body's violations, and its rules to parse: the body is an object whose only member is
/// `quota_rules`, an array of at most [`MAX_RULES`]. A stray member does not stop the parse: the
/// rules are still read when the array is there and within the cap. At most [`UNKNOWN_NAMED`]
/// stray members are named; the rest are counted.
fn collection(body: &Value) -> (Vec<Violation>, Option<&[Value]>) {
    let Some(object) = body.as_object() else {
        return (
            vec![Violation::set(
                RULE_INVALID,
                "a body is an object whose one member is quota_rules".to_owned(),
            )],
            None,
        );
    };
    let mut out: Vec<Violation> = unknown_members(object, |key| key == COLLECTION, "the body")
        .into_iter()
        .map(|detail| Violation::set(RULE_INVALID, detail))
        .collect();
    let rules = match object.get(COLLECTION) {
        Some(Value::Array(rules)) => Some(rules.as_slice()),
        Some(Value::Null) | None => {
            out.push(Violation::set(
                RULE_INVALID,
                "quota_rules is required: an array, empty to clear the rules".to_owned(),
            ));
            None
        }
        Some(_) => {
            out.push(Violation::set(
                RULE_INVALID,
                "quota_rules is an array".to_owned(),
            ));
            None
        }
    };
    if let Some(violation) = rules.and_then(|rules| too_many(rules.len())) {
        out.push(violation);
        return (out, None);
    }
    (out, rules)
}

/// The text of `member`, or its fault under `code`: absent, null or of another JSON type.
fn text<'a>(
    object: &'a Map<String, Value>,
    member: &str,
    code: &'static str,
) -> Result<&'a str, Fault> {
    match object.get(member) {
        Some(Value::String(text)) => Ok(text),
        Some(Value::Null) | None => Err((code, format!("{member} is required"))),
        Some(_) => Err((code, format!("{member} is a string"))),
    }
}

fn uuid_of(object: &Map<String, Value>, member: &str) -> Result<Uuid, Fault> {
    let text = text(object, member, RULE_INVALID)?;
    if let Some(fault) = nul_fault(member, text) {
        return Err(fault);
    }
    Uuid::parse_str(text).map_err(|_| {
        (
            RULE_INVALID,
            format!("{member} is a UUID, not {}", quoted(text)),
        )
    })
}

/// Lowercase hex of a 32-byte digest, as Products writes `definition_digest`: exactly 64 of
/// `0-9a-f`. Uppercase or another length is refused, never folded.
#[must_use]
pub fn digest_from_hex(text: &str) -> Option<Digest> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if pairs.len() != 32 || !rest.is_empty() {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (slot, [high, low]) in digest.iter_mut().zip(pairs) {
        *slot = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(digest)
}

/// A canonical decimal version from 1, as Products writes versions.
fn version_of(text: &str) -> Option<NonZeroU64> {
    text.parse::<QuotaVersion>()
        .ok()
        .and_then(|version| NonZeroU64::new(version.get()))
}

/// `{id, version, digest}`: a UUID, a canonical version from 1 and 64 lowercase hex.
fn reference_of(object: &Map<String, Value>) -> Result<QuotaDefinitionRef, Fault> {
    let shape = || {
        (
            RULE_INVALID,
            "a definition reference is {id, version, digest}: a UUID, a canonical decimal \
             version from 1 and 64 lowercase hex characters"
                .to_owned(),
        )
    };
    let Some(Value::Object(reference)) = object.get("definition_ref") else {
        return Err(shape());
    };
    if reference.len() != REFERENCE_MEMBERS.len()
        || !reference
            .keys()
            .all(|key| REFERENCE_MEMBERS.contains(&key.as_str()))
    {
        return Err(shape());
    }
    let id = uuid_of(reference, "id").map_err(|_| shape())?;
    let version = text(reference, "version", RULE_INVALID)
        .ok()
        .and_then(version_of)
        .ok_or_else(shape)?;
    let digest = text(reference, "digest", RULE_INVALID)
        .ok()
        .and_then(digest_from_hex)
        .ok_or_else(shape)?;
    Ok(QuotaDefinitionRef {
        id,
        version,
        digest,
    })
}

fn dimension_of(object: &Map<String, Value>) -> Result<Option<String>, Fault> {
    match object.get("dimension_value") {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => match dimension_fault(Some(value.as_str())) {
            Some(fault) => Err(fault),
            None => Ok(Some(value.clone())),
        },
        Some(_) => Err((
            RULE_INVALID,
            "dimension_value is a string, or null (an item without a dimension, or an item's \
             default rule)"
                .to_owned(),
        )),
    }
}

fn binding_key_of(object: &Map<String, Value>) -> Result<String, Fault> {
    let key = text(object, "logical_binding_key", RULE_INVALID)?;
    binding_key_fault(key).map_or_else(|| Ok(key.to_owned()), Err)
}

fn subject_of(object: &Map<String, Value>) -> Result<QuotaSubjectSelector, Fault> {
    let token = text(object, "subject_selector", SCOPE_UNSUPPORTED)?;
    if let Some(fault) = nul_fault("subject_selector", token) {
        return Err(fault);
    }
    QuotaSubjectSelector::from_token(token).ok_or_else(|| {
        (
            SCOPE_UNSUPPORTED,
            format!(
                "the subject selector is resource_owner, not {}",
                quoted(token)
            ),
        )
    })
}

fn scope_of(object: &Map<String, Value>) -> Result<BTreeMap<String, String>, Fault> {
    let shape = || {
        (
            SCOPE_UNSUPPORTED,
            "scope_selection is an object of strings".to_owned(),
        )
    };
    let Some(Value::Object(scope)) = object.get("scope_selection") else {
        return Err(shape());
    };
    let mut out = BTreeMap::new();
    for (key, value) in scope {
        let Value::String(value) = value else {
            return Err(shape());
        };
        out.insert(key.clone(), value.clone());
    }
    scope_fault(&out).map_or(Ok(out), Err)
}

fn amount_of(object: &Map<String, Value>) -> Result<String, Fault> {
    let amount = text(object, "amount", AMOUNT_INVALID).map_err(|(code, detail)| {
        (
            code,
            format!("{detail}: a canonical decimal integer in its unit"),
        )
    })?;
    amount_fault(amount).map_or_else(|| Ok(amount.to_owned()), Err)
}

fn unit_of(object: &Map<String, Value>) -> Result<String, Fault> {
    let unit = text(object, "unit_ref", RULE_INVALID)?;
    unit_fault(unit).map_or_else(|| Ok(unit.to_owned()), Err)
}

fn basis_of(object: &Map<String, Value>) -> Result<PlanQuotaBasis, Fault> {
    let token = text(object, "basis", RULE_INVALID)?;
    PlanQuotaBasis::from_token(token).ok_or_else(|| {
        (
            RULE_INVALID,
            format!("the basis is fixed or per_plan_unit, not {}", quoted(token)),
        )
    })
}

/// Keep a member's value, or record its fault on `field`.
fn keep<T>(
    faults: &mut RuleFaults<'_>,
    field: &'static str,
    member: Result<T, Fault>,
) -> Option<T> {
    member.map_err(|fault| faults.add(field, fault)).ok()
}

/// One rule at `index`: every failure of its members, in member order, after its unknown members
/// (at most [`UNKNOWN_NAMED`] named, the rest counted).
fn parse_rule(
    index: usize,
    value: &Value,
    seen: &mut HashSet<Uuid>,
    out: &mut Vec<Violation>,
) -> Option<PlanQuotaRuleV1> {
    let Some(object) = value.as_object() else {
        out.push(Violation {
            code: RULE_INVALID,
            index: Some(index),
            rule_id: None,
            field: "",
            detail: "a rule is an object".to_owned(),
            item: None,
        });
        return None;
    };
    let rule_id = uuid_of(object, "rule_id");
    let mut faults = RuleFaults {
        index,
        rule_id: rule_id.as_ref().ok().copied(),
        out,
    };
    for detail in unknown_members(object, |key| MEMBERS.contains(&key), "a rule") {
        faults.add("", (RULE_INVALID, detail));
    }
    let rule_id = keep(
        &mut faults,
        "rule_id",
        rule_id.and_then(|id| {
            if seen.insert(id) {
                Ok(id)
            } else {
                Err(repeated(id))
            }
        }),
    );
    let definition_ref = keep(&mut faults, "definition_ref", reference_of(object));
    let item_id = keep(&mut faults, "item_id", uuid_of(object, "item_id"));
    let dimension_value = keep(&mut faults, "dimension_value", dimension_of(object));
    let logical_binding_key = keep(&mut faults, "logical_binding_key", binding_key_of(object));
    let subject_selector = keep(&mut faults, "subject_selector", subject_of(object));
    let scope_selection = keep(&mut faults, "scope_selection", scope_of(object));
    let amount = keep(&mut faults, "amount", amount_of(object));
    let unit_ref = keep(&mut faults, "unit_ref", unit_of(object));
    let basis = keep(&mut faults, "basis", basis_of(object));
    Some(PlanQuotaRuleV1 {
        rule_id: rule_id?,
        definition_ref: definition_ref?,
        item_id: item_id?,
        dimension_value: dimension_value?,
        logical_binding_key: logical_binding_key?,
        subject_selector: subject_selector?,
        scope_selection: scope_selection?,
        amount: amount?,
        unit_ref: unit_ref?,
        basis: basis?,
    })
}

/// Parse `{"quota_rules": [...]}` into rules, in the body's order, or every violation found
/// (D-541, D-543): the body, then each rule in order, each rule's unknown members first, then its
/// members in their order. Every rule is parsed, a stray body member notwithstanding; violations
/// accumulate. The body and each rule name at most their first ten unknown members and count the
/// rest, so the refusal stays small. More than [`MAX_RULES`] rules is a cap: no rule is read.
/// # Errors
/// The violations, each naming its rule and member.
pub fn parse_rules(body: &Value) -> Result<Vec<PlanQuotaRuleV1>, Vec<Violation>> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-parse
    let (mut violations, values) = collection(body);
    let values = values.unwrap_or_default();
    let mut seen = HashSet::new();
    let mut rules = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        if let Some(rule) = parse_rule(index, value, &mut seen, &mut violations) {
            rules.push(rule);
        }
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-parse
    if violations.is_empty() {
        Ok(rules)
    } else {
        Err(violations)
    }
}

/// A violation of the rule at `index` on `field`.
fn on(
    index: usize,
    rule: &PlanQuotaRuleV1,
    field: &'static str,
    code: &'static str,
    detail: String,
) -> Violation {
    Violation {
        code,
        index: Some(index),
        rule_id: Some(rule.rule_id),
        field,
        detail,
        item: None,
    }
}

/// Each rule's item is one of `items`, and its dimension value is the item's: null for an item
/// without a dimension; on an item with a dimension, a value `registry` lists for the item's key,
/// or null for the item's default rule (D-541, D-542).
#[must_use]
pub fn item_violations(
    rules: &[PlanQuotaRuleV1],
    items: &[RuleItem],
    registry: &[(String, Vec<String>)],
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        let Some(item) = items.iter().find(|item| item.id == rule.item_id) else {
            out.push(on(
                index,
                rule,
                "item_id",
                RULE_INVALID,
                format!("item {} is not an item of this revision", rule.item_id),
            ));
            continue;
        };
        let detail = match (
            item.dimension_key.as_deref(),
            rule.dimension_value.as_deref(),
        ) {
            (_, None) => continue,
            (None, Some(value)) => format!(
                "item {} has no dimension: its dimension value is null, not {}",
                item.id,
                quoted(value)
            ),
            (Some(key), Some(value)) => {
                let listed = registry
                    .iter()
                    .any(|(k, values)| k == key && values.iter().any(|v| v == value));
                if listed {
                    continue;
                }
                format!("{} is not a value of {}", quoted(value), quoted(key))
            }
        };
        out.push(on(index, rule, "dimension_value", RULE_INVALID, detail));
    }
    out
}

/// Each rule's item has a SKU version on the revision's sale date: `versions` maps a SKU to the
/// version in force then (D-542). A rule whose SKU has none is `QUOTA_RULE_INVALID` on `item_id`,
/// judged before any Products call; a version below 1 is none. A rule whose item is not in
/// `items` is [`item_violations`]'s.
#[must_use]
pub fn version_violations(
    rules: &[PlanQuotaRuleV1],
    items: &[RuleItem],
    versions: &BTreeMap<Uuid, i64>,
) -> Vec<Violation> {
    rules
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| {
            let item = items.iter().find(|item| item.id == rule.item_id)?;
            let versioned = versions.get(&item.sku_id).is_some_and(|v| *v >= 1);
            (!versioned).then(|| {
                on(
                    index,
                    rule,
                    "item_id",
                    RULE_INVALID,
                    format!(
                        "SKU {} of item {} has no version on the revision's sale date",
                        item.sku_id, item.id
                    ),
                )
            })
        })
        .collect()
}

/// Rules for one `(logical_binding_key, metric)` that would apply together: on two items, or on
/// one item with the same dimension value, two default rules (null) included (D-542). An item's
/// default rule and its rule for a value are alternatives. `metric_of` maps a rule id to its
/// definition's metric; a rule without one is not compared.
#[must_use]
pub fn duplicate_violations(
    rules: &[PlanQuotaRuleV1],
    metric_of: &BTreeMap<Uuid, String>,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        let Some(metric) = metric_of.get(&rule.rule_id) else {
            continue;
        };
        let together: Vec<String> = rules
            .iter()
            .enumerate()
            .filter(|(other_index, other)| {
                *other_index != index
                    && other.logical_binding_key == rule.logical_binding_key
                    && metric_of.get(&other.rule_id) == Some(metric)
                    && (other.item_id != rule.item_id
                        || other.dimension_value == rule.dimension_value)
            })
            .map(|(_, other)| other.rule_id.to_string())
            .collect();
        if !together.is_empty() {
            out.push(on(
                index,
                rule,
                "logical_binding_key",
                RULE_DUPLICATE,
                format!(
                    "binding {} sets {} again in rule(s) {}",
                    quoted(&rule.logical_binding_key),
                    quoted(metric),
                    together.join(", ")
                ),
            ));
        }
    }
    out
}

/// The code of the quota row a Products check is shown on (D-544). The match is exhaustive, so a
/// code Products adds does not compile until it has a row; a violation whose code still has none
/// turns `QUOTA_RULE_INVALID` red ([`check_rows`]).
const fn row_code(code: QuotaCheckCode) -> &'static str {
    match code {
        QuotaCheckCode::RuleInvalid => RULE_INVALID,
        QuotaCheckCode::DefinitionChanged => DEFINITION_CHANGED,
        QuotaCheckCode::AmountInvalid => AMOUNT_INVALID,
        QuotaCheckCode::UnitInexact => UNIT_INEXACT,
        QuotaCheckCode::ScopeUnsupported => SCOPE_UNSUPPORTED,
        QuotaCheckCode::SetIncompatible => SET_INCOMPATIBLE,
        QuotaCheckCode::QualificationUnavailable => QUALIFICATION_UNAVAILABLE,
    }
}

/// Products' checks as violations of the rules they name (D-542, D-543), in Products' order: one
/// violation per named rule, on the member [`top_field`] maps the check's field to, its detail
/// prefixed with Products' full path when that is not the member. A check that names no rule (a
/// prior limit that no rule keeps) is a violation of the whole set. A rule Products did not judge
/// is [`unjudged`]'s.
#[must_use]
pub fn catalog_violations(
    rules: &[PlanQuotaRuleV1],
    validation: &QuotaSetValidation,
) -> Vec<Violation> {
    let position: BTreeMap<Uuid, usize> = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| (rule.rule_id, index))
        .collect();
    let mut out = Vec::new();
    for check in &validation.checks {
        let code = row_code(check.code);
        let field = top_field(&check.field);
        let detail = if check.field == field {
            check.detail.clone()
        } else {
            format!("{}: {}", check.field, check.detail)
        };
        if check.rule_ids.is_empty() {
            out.push(Violation::set(code, detail));
            continue;
        }
        for rule_id in &check.rule_ids {
            out.push(match position.get(rule_id) {
                Some(index) => Violation {
                    code,
                    index: Some(*index),
                    rule_id: Some(*rule_id),
                    field,
                    detail: detail.clone(),
                    item: None,
                },
                // Products names only the rules it was given; a stranger is kept on the set.
                None => Violation::set(code, format!("rule {rule_id}: {detail}")),
            });
        }
    }
    out
}

/// Why a rule Products was sent is not judged: no definition and no check names it.
const LEFT_OUT: &str =
    "Products' verdict neither resolves the rule's definition nor names the rule";
/// Why a rule Products was sent is not judged: its verdict resolves another version.
const OTHER_VERSION: &str =
    "Products' verdict resolves the rule to another definition version than the one it names";
/// Why a rule Products was sent is not judged: its verdict resolves another digest, unnamed.
const OTHER_DIGEST: &str = "Products' verdict resolves the rule to another definition digest \
     than the one it names, with no QUOTA_DEFINITION_CHANGED check on the rule";

/// Whether `definition` is the version `reference` names: its id and its version.
fn same_version(reference: &QuotaDefinitionRef, definition: &QuotaDefinition) -> bool {
    (definition.definition_id, definition.version.get()) == (reference.id, reference.version.get())
}

/// Whether `definition` is exactly the one `reference` names: its id, its version and its digest
/// (D-542). A rule is judged only against that definition ([`unjudged`]), and it is stored only
/// with that definition's verdict (`infra::plan_quota::row_of`).
#[must_use]
pub fn names_definition(reference: &QuotaDefinitionRef, definition: &QuotaDefinition) -> bool {
    same_version(reference, definition)
        && digest_from_hex(&definition.definition_digest) == Some(reference.digest)
}

/// The rules of `sent` (the rules of the request, in its order) that `verdict` does not judge
/// against the definition each names (D-543, D-544), each with why. A rule is judged when every
/// definition Products resolved for it is the one it names ([`names_definition`]); when they are
/// that version with another digest and a `QUOTA_DEFINITION_CHANGED` check names the rule; or
/// when Products resolved none and some check names the rule. Products judges every rule it is
/// sent against the exact version it names, so any other answer (the rule left out, judged
/// without a definition and named in no check, resolved to another version, or to another digest
/// with no changed-definition check) is Products' contract break, never the author's fault: the
/// PUT answers a logged 500, and the checks show the rule red, never green.
#[must_use]
pub fn unjudged<'a>(
    sent: impl IntoIterator<Item = &'a PlanQuotaRuleV1>,
    verdict: &QuotaSetValidation,
) -> Vec<(Uuid, &'static str)> {
    sent.into_iter()
        .filter_map(|rule| {
            let named = |code: Option<QuotaCheckCode>| {
                verdict.checks.iter().any(|check| {
                    check.rule_ids.contains(&rule.rule_id) && code.is_none_or(|c| check.code == c)
                })
            };
            let resolved: Vec<&QuotaDefinition> = verdict
                .rules
                .iter()
                .filter(|judged| judged.rule_id == rule.rule_id)
                .filter_map(|judged| judged.definition.as_ref())
                .collect();
            let reference = &rule.definition_ref;
            let why = if resolved.is_empty() {
                (!named(None)).then_some(LEFT_OUT)
            } else if resolved.iter().any(|d| !same_version(reference, d)) {
                Some(OTHER_VERSION)
            } else if resolved.iter().any(|d| !names_definition(reference, d))
                && !named(Some(QuotaCheckCode::DefinitionChanged))
            {
                Some(OTHER_DIGEST)
            } else {
                None
            };
            why.map(|why| (rule.rule_id, why))
        })
        .collect()
}

/// The rules Products is sent (D-542): those whose item is in `items` and whose SKU has a
/// version from 1 in `versions`, in their order. The others are Pricing's own checks'.
#[must_use]
pub fn sent_rules<'a>(
    rules: &'a [PlanQuotaRuleV1],
    items: &[RuleItem],
    versions: &BTreeMap<Uuid, i64>,
) -> Vec<&'a PlanQuotaRuleV1> {
    rules
        .iter()
        .filter(|rule| {
            items
                .iter()
                .find(|item| item.id == rule.item_id)
                .and_then(|item| versions.get(&item.sku_id))
                .is_some_and(|version| *version >= 1)
        })
        .collect()
}

/// The metric of each rule whose verdict resolves the definition version the rule names (D-542),
/// by rule id: the duplicates are judged by these only, so a rule resolved to another version
/// (Products' contract break, [`unjudged`]) lends no metric.
#[must_use]
pub fn resolved_metrics(
    rules: &[PlanQuotaRuleV1],
    verdict: &QuotaSetValidation,
) -> BTreeMap<Uuid, String> {
    rules
        .iter()
        .filter_map(|rule| {
            let definition = verdict
                .rules
                .iter()
                .filter(|judged| judged.rule_id == rule.rule_id)
                .find_map(|judged| judged.definition.as_ref())?;
            same_version(&rule.definition_ref, definition)
                .then(|| (rule.rule_id, definition.metric_ref.clone()))
        })
        .collect()
}

/// 409 when there is a violation and every one is a conflict code ([`CONFLICT_CODES`]), else 400
/// (D-543).
#[must_use]
pub fn refusal_status(violations: &[Violation]) -> u16 {
    let conflict = !violations.is_empty()
        && violations
            .iter()
            .all(|violation| CONFLICT_CODES.contains(&violation.code));
    if conflict { 409 } else { 400 }
}

/// The rule and member a quota row's subject names (D-544).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleSubject {
    pub rule_id: Uuid,
    /// The rule's top-level member, as [`Violation::field`].
    pub field: &'static str,
}

/// The quota rows in their order (D-544, D-547), each with its label.
const ROWS: [(&str, &str); 9] = [
    (RULE_INVALID, "Quota rules name catalog definitions"),
    (DEFINITION_CHANGED, "Quota definitions are unchanged"),
    (AMOUNT_INVALID, "Quota amounts are on their grids"),
    (UNIT_INEXACT, "Quota amounts convert exactly"),
    (SCOPE_UNSUPPORTED, "Quota scopes are supported"),
    (RULE_DUPLICATE, "One rule per binding and metric"),
    (SET_INCOMPATIBLE, "Each binding's rules fit together"),
    (QUALIFICATION_UNAVAILABLE, "Quota definitions are sellable"),
    (RULE_MISSING, "Every sellable cell has a rule per quota"),
];

/// The subject a violation of `ctx`'s stored rules names (D-466, D-544): for a rule's violation,
/// its item, SKU and entry with the rule and member; for a line of the set about one item (an
/// uncovered sellable cell), that item, its SKU and entry with no rule; `None` for any other line
/// of the set, and for a rule or item the revision does not hold.
fn subject(ctx: &PlanContext, violation: &Violation) -> Option<Subject> {
    let (item_id, quota) = match violation.index {
        Some(index) => {
            let rule = ctx.quota_rules.get(index)?;
            let quota = RuleSubject {
                rule_id: rule.rule_id,
                field: violation.field,
            };
            (rule.item_id, Some(quota))
        }
        None => (violation.item?, None),
    };
    let item = ctx.items.iter().find(|item| item.id == item_id)?;
    Some(Subject {
        item: item.id,
        sku: item.sku_id,
        entry: item.price_book_entry_id,
        quota,
    })
}

/// The cells of `item` that a sale can name on `sale` (D-547): each value the dimension registry
/// lists for the key of the item's entry, and the cell without a value only where the entry's
/// default chain has a price in force on that day ([`price::version_at`] with no value): the
/// price that resolve binds for a sale without a value. A cell that becomes sellable after `sale`
/// is not counted; acceptance refuses its sale. None for an item whose entry the context does not
/// hold.
fn sellable_cells(ctx: &PlanContext, item: &Item, sale: Date) -> Vec<Option<String>> {
    let Some(entry) = ctx
        .entries
        .iter()
        .find(|entry| Some(entry.id) == item.price_book_entry_id)
    else {
        return Vec::new();
    };
    let mut cells = Vec::new();
    if price::version_at(&entry.prices, entry.id, sale, None).is_some() {
        cells.push(None);
    }
    if let Some(key) = &entry.dimension_key {
        cells.extend(
            ctx.dimension_values
                .iter()
                .filter(|(registered, _)| registered == key)
                .flat_map(|(_, values)| values.iter().cloned().map(Some)),
        );
    }
    cells
}

/// Why a sellable cell is uncovered: its item, its cell (a registered value through
/// [`quoted`], or no value) and the metric no rule covers there.
fn missing_detail(missing: &UncoveredQuota) -> String {
    let (item, metric) = (missing.item_id, &missing.metric_ref);
    match &missing.dimension_value {
        Some(value) => format!(
            "item {item} at value {}: no rule of {metric:?} for the value and no default rule",
            quoted(value)
        ),
        None => format!("item {item} without a value: no rule of {metric:?} without a value"),
    }
}

/// Each sellable cell on `sale` of an item with rules that selects no rule of a metric the
/// item's rules carry (D-547), as `QUOTA_RULE_MISSING` on the set with the item as its subject
/// (D-466: the item turns the row red): one violation per item, cell and metric, in the
/// revision's item order, each item's cells by value (the cell without a value first), then by
/// metric. A quota is a metric, whatever its binding, and the coverage is the SDK's
/// ([`uncovered_metrics`]), the rule an acceptance applies to a sale, over the item's own rules
/// only. A rule's metric is the one `metric_of` holds for the definition version it names; a rule
/// without one covers nothing and is red on its own rows. The cells come from the context alone,
/// with no catalog call ([`sellable_cells`]).
fn missing_violations(
    ctx: &PlanContext,
    metric_of: &BTreeMap<Uuid, String>,
    sale: Date,
) -> Vec<Violation> {
    let rules: Vec<CoverageRule<'_>> = ctx
        .quota_rules
        .iter()
        .filter_map(|rule| {
            metric_of.get(&rule.rule_id).map(|metric| CoverageRule {
                item_id: rule.item_id,
                dimension_value: rule.dimension_value.as_deref(),
                metric_ref: metric,
            })
        })
        .collect();
    let mut out = Vec::new();
    for item in &ctx.items {
        let own: Vec<CoverageRule<'_>> = rules
            .iter()
            .filter(|rule| rule.item_id == item.id)
            .copied()
            .collect();
        if own.is_empty() {
            continue;
        }
        let cells: Vec<BindingSelection> = sellable_cells(ctx, item, sale)
            .into_iter()
            .map(|dimension_value| BindingSelection {
                item_id: item.id,
                dimension_value,
            })
            .collect();
        out.extend(
            uncovered_metrics(&own, &cells).iter().map(|missing| {
                Violation::set_of_item(RULE_MISSING, item.id, missing_detail(missing))
            }),
        );
    }
    out
}

/// The quota rows of a revision with rules (D-544), in their order, each red with the
/// subjects of its violations, or green. The violations are Pricing's own of the stored rules
/// against `ctx` (items and their entries' dimension keys, the dimension registry, the SKU
/// versions on the sale date, duplicates by the metrics `verdict` resolved for the versions the
/// rules name, and each sellable cell on `sale` that selects no rule of a metric, D-547) and
/// Products' checks. A rule Products was sent but did not judge against the
/// definition it names ([`unjudged`]) is red `QUOTA_RULE_INVALID` on its `definition_ref`, never
/// green, and logged as Products' contract break. A red row's detail
/// lists `"<path>: <detail>"` per violation; its subjects name one rule and member per violation,
/// in that order, an uncovered sellable cell names its item with no rule, and any other violation
/// of the whole set names none. A violation whose code has no row
/// of its own is never dropped and never green: it turns `QUOTA_RULE_INVALID` red, its code in
/// its line.
#[must_use]
pub fn check_rows(ctx: &PlanContext, verdict: &QuotaSetValidation, sale: Date) -> Vec<Check> {
    let items: Vec<RuleItem> = ctx
        .items
        .iter()
        .map(|item| RuleItem {
            id: item.id,
            sku_id: item.sku_id,
            dimension_key: ctx
                .entries
                .iter()
                .find(|entry| Some(entry.id) == item.price_book_entry_id)
                .and_then(|entry| entry.dimension_key.clone()),
        })
        .collect();
    let metric_of = resolved_metrics(&ctx.quota_rules, verdict);
    let mut violations = item_violations(&ctx.quota_rules, &items, &ctx.dimension_values);
    violations.extend(version_violations(
        &ctx.quota_rules,
        &items,
        &ctx.quota_sku_versions,
    ));
    violations.extend(duplicate_violations(&ctx.quota_rules, &metric_of));
    violations.extend(missing_violations(ctx, &metric_of, sale));
    violations.extend(catalog_violations(&ctx.quota_rules, verdict));
    let sent = sent_rules(&ctx.quota_rules, &items, &ctx.quota_sku_versions);
    for (rule_id, why) in unjudged(sent, verdict) {
        tracing::error!(
            revision_id = %ctx.revision.id,
            rule_id = %rule_id,
            why,
            "pricing: Products' quota verdict does not judge a rule it was sent"
        );
        if let Some((index, rule)) = ctx
            .quota_rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.rule_id == rule_id)
        {
            violations.push(on(
                index,
                rule,
                "definition_ref",
                RULE_INVALID,
                why.to_owned(),
            ));
        }
    }
    rows(ctx, &violations)
}

/// The quota rows of a revision whose rules have no Products verdict in `ctx` (D-542, D-544).
/// Every reader of the checks reads the verdict first, so a normal path never gets here; the
/// rows fail closed: `QUOTA_QUALIFICATION_UNAVAILABLE` is red for every rule, on its
/// `definition_ref`, so the revision never reads ready.
#[must_use]
pub fn unjudged_rows(ctx: &PlanContext) -> Vec<Check> {
    let violations: Vec<Violation> = ctx
        .quota_rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            on(
                index,
                rule,
                "definition_ref",
                QUALIFICATION_UNAVAILABLE,
                "Products' verdict for the rule was not read".to_owned(),
            )
        })
        .collect();
    rows(ctx, &violations)
}

/// The nine quota rows over `violations`, in their order. A violation whose code has no row of
/// its own joins `QUOTA_RULE_INVALID`, its code in its line, and is logged: never dropped, never
/// green.
fn rows(ctx: &PlanContext, violations: &[Violation]) -> Vec<Check> {
    let has_row = |code: &str| ROWS.iter().any(|(row, _)| *row == code);
    for violation in violations.iter().filter(|v| !has_row(v.code)) {
        tracing::error!(
            revision_id = %ctx.revision.id,
            code = violation.code,
            "pricing: a quota violation has no checks row of its own"
        );
    }
    ROWS.iter()
        .map(|(code, label)| {
            let found: Vec<&Violation> = violations
                .iter()
                .filter(|v| v.code == *code || (*code == RULE_INVALID && !has_row(v.code)))
                .collect();
            let detail = if found.is_empty() {
                "ok".to_owned()
            } else {
                found
                    .iter()
                    .map(|v| {
                        if v.code == *code {
                            format!("{}: {}", v.path(), v.detail)
                        } else {
                            format!("{}: {}: {}", v.path(), v.code, v.detail)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            Check {
                code,
                ok: found.is_empty(),
                label: (*label).to_owned(),
                detail,
                info: false,
                subjects: found.iter().filter_map(|v| subject(ctx, v)).collect(),
                blocked_by_prices: Vec::new(),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "plan_quota_tests.rs"]
mod plan_quota_tests;

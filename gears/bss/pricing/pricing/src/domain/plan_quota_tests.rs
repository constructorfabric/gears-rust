//! The plan quota rule judgement (D-541, D-542, D-543): the parse of a request body, the same codes
//! for typed rules, Pricing's own checks of a set against its revision, and Products' checks as
//! violations of the rules they name.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::domain::plan::{Check, PlanContext};
use bss_products_sdk::quota::validation::{
    QuotaCheck, QuotaCheckCode, QuotaRuleVerdict, QuotaSetValidation,
};
use serde_json::{Value, json};

const ITEM_A: Uuid = Uuid::from_u128(0xa);
const ITEM_B: Uuid = Uuid::from_u128(0xb);
const DEF: &str = "4a1e7d60-34b2-4e5b-8709-eab681e15940";
const VCPU_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1";

fn rule_json(n: u128, item: Uuid, amount: &str) -> Value {
    json!({
        "rule_id": Uuid::from_u128(n),
        "definition_ref": {"id": DEF, "version": "1", "digest": "a".repeat(64)},
        "item_id": item, "dimension_value": null, "logical_binding_key": "compute",
        "subject_selector": "resource_owner",
        "scope_selection": {"region": "region-1", "resource_pool": "pool-1"},
        "amount": amount, "unit_ref": VCPU_UNIT, "basis": "per_plan_unit"
    })
}
fn paths(violations: &[Violation]) -> Vec<(&'static str, String)> {
    violations.iter().map(|v| (v.code, v.path())).collect()
}
fn body(rules: Vec<Value>) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("quota_rules".to_owned(), Value::Array(rules));
    Value::Object(body)
}
/// The parse's violations of one rule whose `member` is `value`.
fn with(member: &str, value: Value) -> Vec<(&'static str, String)> {
    let mut rule = rule_json(1, ITEM_A, "10");
    rule[member] = value;
    paths(&parse_rules(&body(vec![rule])).unwrap_err())
}
fn scope_of(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), json!(value)))
            .collect(),
    )
}
fn one(code: &'static str, path: &str) -> Vec<(&'static str, String)> {
    vec![(code, path.to_owned())]
}
fn parsed(rules: Vec<Value>) -> Vec<PlanQuotaRuleV1> {
    parse_rules(&body(rules)).unwrap()
}

#[test]
fn a_complete_body_parses_into_normalized_rules() {
    let rules = parsed(vec![rule_json(2, ITEM_A, "10"), rule_json(1, ITEM_B, "20")]);
    assert_eq!(rules.len(), 2);
    assert_eq!(
        rules[0].rule_id,
        Uuid::from_u128(2),
        "the body's order is kept"
    );
    assert_eq!(rules[0].basis, PlanQuotaBasis::PerPlanUnit);
    assert_eq!(rules[0].definition_ref.digest, [0xaa; 32]);
    assert_eq!(rules[0].definition_ref.version.get(), 1);
    assert_eq!(
        rules[0].subject_selector,
        QuotaSubjectSelector::ResourceOwner
    );
    assert_eq!(rules[0].scope_selection["resource_pool"], "pool-1");
    assert_eq!(rules[1].amount, "20");
}

#[test]
fn the_package_example_body_parses() {
    let example: Value = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/plan-quota-rules.replace.example.json"
    ))
    .unwrap();
    assert_eq!(parse_rules(&example).unwrap().len(), 1);
}

#[test]
fn unknown_members_and_null_collections_are_refused() {
    assert_eq!(
        paths(&parse_rules(&json!({"quota_rules": null})).unwrap_err()),
        one("QUOTA_RULE_INVALID", "quota_rules")
    );
    let mut extra = rule_json(1, ITEM_A, "10");
    extra["paid"] = json!(true);
    let mut null_scope = rule_json(2, ITEM_A, "10");
    null_scope["scope_selection"] = Value::Null;
    assert_eq!(
        paths(&parse_rules(&body(vec![extra, null_scope])).unwrap_err()),
        [
            ("QUOTA_RULE_INVALID", "quota_rules[0]".to_owned()),
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "quota_rules[1].scope_selection".to_owned()
            )
        ]
    );
}

#[test]
fn a_body_is_one_object_with_one_array() {
    for body in [
        json!([]),
        json!({}),
        json!({"quota_rules": {}}),
        json!({"quota_rules": "[]"}),
    ] {
        assert_eq!(
            paths(&parse_rules(&body).unwrap_err()),
            one("QUOTA_RULE_INVALID", "quota_rules"),
            "{body}"
        );
    }
    let v = parse_rules(&json!({"quota_rules": [], "etag": "1"})).unwrap_err();
    assert_eq!(paths(&v), one("QUOTA_RULE_INVALID", "quota_rules"));
    assert!(v[0].detail.contains("etag"), "{}", v[0].detail);
    assert!(parse_rules(&json!({"quota_rules": []})).unwrap().is_empty());
    let v = parse_rules(&body(vec![json!("rule")])).unwrap_err();
    assert_eq!(paths(&v), one("QUOTA_RULE_INVALID", "quota_rules[0]"));
}

/// A stray body member does not stop the parse: the rules' own failures follow it, in one
/// answer. More than 200 rules is a cap, and the rules are not read then.
#[test]
fn a_stray_body_member_still_reports_every_rule_failure() {
    let mut bad = rule_json(1, ITEM_A, "1.5");
    bad["paid"] = json!(true);
    let v = parse_rules(&json!({"quota_rules": [bad], "etag": "1"})).unwrap_err();
    assert_eq!(
        paths(&v),
        [
            ("QUOTA_RULE_INVALID", "quota_rules".to_owned()),
            ("QUOTA_RULE_INVALID", "quota_rules[0]".to_owned()),
            ("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount".to_owned()),
        ]
    );
    let over: Vec<Value> = (0..201).map(|n| rule_json(n, ITEM_A, "1.5")).collect();
    let v = parse_rules(&json!({"quota_rules": over, "etag": "1"})).unwrap_err();
    assert_eq!(
        paths(&v),
        [
            ("QUOTA_RULE_INVALID", "quota_rules".to_owned()),
            ("QUOTA_RULE_INVALID", "quota_rules".to_owned()),
        ],
        "the stray member and the cap, and no rule"
    );
}

#[test]
fn more_than_200_rules_are_refused() {
    let rules: Vec<Value> = (0..201).map(|n| rule_json(n, ITEM_A, "10")).collect();
    assert_eq!(
        paths(&parse_rules(&body(rules)).unwrap_err()),
        one("QUOTA_RULE_INVALID", "quota_rules")
    );
    let rules: Vec<Value> = (0..200).map(|n| rule_json(n, ITEM_A, "10")).collect();
    assert_eq!(parsed(rules).len(), 200);
}

#[test]
fn a_repeated_rule_id_is_refused() {
    let v = parse_rules(&body(vec![
        rule_json(1, ITEM_A, "10"),
        rule_json(1, ITEM_B, "20"),
    ]))
    .unwrap_err();
    assert_eq!(
        paths(&v),
        one("QUOTA_RULE_INVALID", "quota_rules[1].rule_id")
    );
}

#[test]
fn amounts_are_canonical_integers() {
    for (i, amount) in ["1.5", "-1", "1e3", "01", " 1", "", "unlimited"]
        .iter()
        .enumerate()
    {
        let v = parse_rules(&body(vec![rule_json(1, ITEM_A, amount)])).unwrap_err();
        assert_eq!(
            paths(&v),
            one("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount"),
            "{i}: {amount:?}"
        );
    }
    let forty = "1".repeat(40);
    assert_eq!(
        with("amount", json!(forty)),
        one("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount")
    );
    assert_eq!(
        with("amount", json!(10)),
        one("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount"),
        "a JSON number is not a canonical integer string"
    );
    assert!(
        parse_rules(&body(vec![rule_json(1, ITEM_A, "0")])).is_ok(),
        "zero is a limit"
    );
}

#[test]
fn a_binding_key_off_the_rule_is_rule_invalid() {
    for key in ["compute/1", "", &"k".repeat(65)] {
        assert_eq!(
            with("logical_binding_key", json!(key)),
            one("QUOTA_RULE_INVALID", "quota_rules[0].logical_binding_key"),
            "{key:?}"
        );
    }
    assert!(
        parse_rules(&body(vec![{
            let mut r = rule_json(1, ITEM_A, "10");
            r["logical_binding_key"] = json!("a.B-c_9");
            r
        }]))
        .is_ok()
    );
}

#[test]
fn every_member_is_required_with_its_type() {
    for (member, code) in [
        ("rule_id", "QUOTA_RULE_INVALID"),
        ("definition_ref", "QUOTA_RULE_INVALID"),
        ("item_id", "QUOTA_RULE_INVALID"),
        ("logical_binding_key", "QUOTA_RULE_INVALID"),
        ("subject_selector", "QUOTA_SCOPE_UNSUPPORTED"),
        ("scope_selection", "QUOTA_SCOPE_UNSUPPORTED"),
        ("amount", "QUOTA_AMOUNT_INVALID"),
        ("unit_ref", "QUOTA_RULE_INVALID"),
        ("basis", "QUOTA_RULE_INVALID"),
    ] {
        let path = format!("quota_rules[0].{member}");
        assert_eq!(with(member, Value::Null), one(code, &path), "null {member}");
        assert_eq!(
            with(member, json!(true)),
            one(code, &path),
            "boolean {member}"
        );
        let mut absent = rule_json(1, ITEM_A, "10");
        absent.as_object_mut().unwrap().remove(member);
        let v = parse_rules(&body(vec![absent])).unwrap_err();
        assert_eq!(paths(&v), one(code, &path), "absent {member}");
    }
    assert_eq!(
        with("dimension_value", json!(7)),
        one("QUOTA_RULE_INVALID", "quota_rules[0].dimension_value")
    );
    let mut number = rule_json(1, ITEM_A, "10");
    number["dimension_value"] = json!(7);
    assert_eq!(
        parse_rules(&body(vec![number])).unwrap_err()[0].detail,
        "dimension_value is a string, or null (an item without a dimension, or an item's default \
         rule)"
    );
    let mut absent = rule_json(1, ITEM_A, "10");
    absent.as_object_mut().unwrap().remove("dimension_value");
    assert_eq!(
        parsed(vec![absent])[0].dimension_value,
        None,
        "an absent dimension value reads as null"
    );
}

#[test]
fn ids_and_tokens_are_exact() {
    assert_eq!(
        with("rule_id", json!("rule-1")),
        one("QUOTA_RULE_INVALID", "quota_rules[0].rule_id")
    );
    assert_eq!(
        with("item_id", json!("item-a")),
        one("QUOTA_RULE_INVALID", "quota_rules[0].item_id")
    );
    for basis in ["Fixed", "per-plan-unit", "PerPlanUnit"] {
        assert_eq!(
            with("basis", json!(basis)),
            one("QUOTA_RULE_INVALID", "quota_rules[0].basis"),
            "{basis}"
        );
    }
    assert_eq!(
        with("subject_selector", json!("Resource_Owner")),
        one("QUOTA_SCOPE_UNSUPPORTED", "quota_rules[0].subject_selector")
    );
    let mut fixed = rule_json(1, ITEM_A, "10");
    fixed["basis"] = json!("fixed");
    assert_eq!(parsed(vec![fixed])[0].basis, PlanQuotaBasis::Fixed);
}

#[test]
fn a_definition_ref_is_its_three_members_with_a_version_and_lowercase_hex() {
    let at = "quota_rules[0].definition_ref";
    for reference in [
        json!({"id": DEF, "version": "1"}),
        json!({"id": DEF, "version": "1", "digest": "a".repeat(64), "name": "vcpu"}),
        json!({"id": "vcpu", "version": "1", "digest": "a".repeat(64)}),
        json!({"id": DEF, "version": 1, "digest": "a".repeat(64)}),
        json!({"id": DEF, "version": "0", "digest": "a".repeat(64)}),
        json!({"id": DEF, "version": "01", "digest": "a".repeat(64)}),
        json!({"id": DEF, "version": "18446744073709551616", "digest": "a".repeat(64)}),
        json!({"id": DEF, "version": "1", "digest": "A".repeat(64)}),
        json!({"id": DEF, "version": "1", "digest": "a".repeat(63)}),
        json!({"id": DEF, "version": "1", "digest": "a".repeat(66)}),
        json!({"id": DEF, "version": "1", "digest": "g".repeat(64)}),
        json!("4a1e7d60-34b2-4e5b-8709-eab681e15940@1"),
    ] {
        assert_eq!(
            with("definition_ref", reference.clone()),
            one("QUOTA_RULE_INVALID", at),
            "{reference}"
        );
    }
    let digest = format!("0123456789abcdef{}", "f".repeat(48));
    let rules = parsed(vec![{
        let mut r = rule_json(1, ITEM_A, "10");
        r["definition_ref"] =
            json!({"id": DEF, "version": "18446744073709551615", "digest": digest});
        r
    }]);
    assert_eq!(rules[0].definition_ref.version.get(), u64::MAX);
    assert_eq!(
        rules[0].definition_ref.digest[..8],
        [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
    );
    assert_eq!(
        digest_from_hex(&digest),
        Some(rules[0].definition_ref.digest)
    );
    assert_eq!(
        digest_from_hex(&digest.to_uppercase()),
        None,
        "uppercase is refused, not folded"
    );
}

#[test]
fn scope_keys_follow_the_binding_key_rule_and_values_are_printable() {
    let at = "quota_rules[0].scope_selection";
    let nine: serde_json::Map<String, Value> =
        (0..9).map(|n| (format!("k{n}"), json!("v"))).collect();
    for scope in [
        json!({"region/1": "eu"}),
        json!({"": "eu"}),
        scope_of(&[(&"k".repeat(65), "eu")]),
        Value::Object(nine),
        json!({"region": ""}),
        json!({"region": "e".repeat(256)}),
        json!({"region": "eu\tnorth"}),
        json!({"region": "\u{e9}u"}),
        json!({"region": 1}),
        json!({"region": null}),
        json!(["region"]),
    ] {
        assert_eq!(
            with("scope_selection", scope.clone()),
            one("QUOTA_SCOPE_UNSUPPORTED", at),
            "{scope}"
        );
    }
    let eight: serde_json::Map<String, Value> = (0..8)
        .map(|n| {
            (
                format!("k.{n}-_K{}", "x".repeat(58)),
                json!("~ !".repeat(85)),
            )
        })
        .collect();
    let rules = parsed(vec![{
        let mut r = rule_json(1, ITEM_A, "10");
        r["scope_selection"] = Value::Object(eight);
        r
    }]);
    assert_eq!(
        rules[0].scope_selection.len(),
        8,
        "8 keys of 64 bytes, values of 255"
    );
    let empty = parsed(vec![{
        let mut r = rule_json(1, ITEM_A, "10");
        r["scope_selection"] = json!({});
        r
    }]);
    assert!(
        empty[0].scope_selection.is_empty(),
        "Products judges the missing dimensions"
    );
}

#[test]
fn a_nul_in_any_member_is_rule_invalid() {
    for (member, value) in [
        ("rule_id", json!("\u{0}")),
        ("dimension_value", json!("eu\u{0}")),
        ("logical_binding_key", json!("compute\u{0}")),
        ("subject_selector", json!("resource_owner\u{0}")),
        ("scope_selection", json!({"region": "eu\u{0}"})),
        ("scope_selection", json!({"region\u{0}": "eu"})),
        ("amount", json!("1\u{0}")),
        ("unit_ref", json!("u\u{0}")),
        ("basis", json!("fixed\u{0}")),
    ] {
        assert_eq!(
            with(member, value.clone()),
            one("QUOTA_RULE_INVALID", &format!("quota_rules[0].{member}")),
            "{member}: {value}"
        );
    }
}

#[test]
fn unit_and_dimension_texts_stay_within_the_catalogs_bound() {
    let long = "u".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(
        with("unit_ref", json!("u".repeat(MAX_REF_BYTES + 1))),
        one("QUOTA_RULE_INVALID", "quota_rules[0].unit_ref")
    );
    assert_eq!(
        with("unit_ref", json!("")),
        one("QUOTA_RULE_INVALID", "quota_rules[0].unit_ref")
    );
    assert_eq!(
        with("dimension_value", json!(long)),
        one("QUOTA_RULE_INVALID", "quota_rules[0].dimension_value")
    );
    let mut at_bound = rule_json(1, ITEM_A, "10");
    // The longest GTS id the catalog stores is a unit reference Pricing takes.
    at_bound["unit_ref"] = json!("u".repeat(MAX_REF_BYTES));
    at_bound["dimension_value"] = json!("d".repeat(MAX_TEXT_BYTES));
    assert!(parse_rules(&body(vec![at_bound])).is_ok());
}

#[test]
fn every_rule_is_parsed_and_its_failures_keep_member_order() {
    let mut first = rule_json(1, ITEM_A, "x");
    first["basis"] = json!("monthly");
    first["subject_selector"] = json!("tenant");
    first["extra"] = json!(1);
    let mut second = rule_json(2, ITEM_B, "10");
    second["logical_binding_key"] = json!("a b");
    let v = parse_rules(&body(vec![first, rule_json(3, ITEM_A, "1"), second])).unwrap_err();
    assert_eq!(
        paths(&v),
        [
            ("QUOTA_RULE_INVALID", "quota_rules[0]".to_owned()),
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "quota_rules[0].subject_selector".to_owned()
            ),
            ("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount".to_owned()),
            ("QUOTA_RULE_INVALID", "quota_rules[0].basis".to_owned()),
            (
                "QUOTA_RULE_INVALID",
                "quota_rules[2].logical_binding_key".to_owned()
            ),
        ]
    );
    assert_eq!(
        v[0].rule_id,
        Some(Uuid::from_u128(1)),
        "a violation names its rule"
    );
    assert!(v[0].detail.contains("extra"), "{}", v[0].detail);
}

#[test]
fn a_caller_text_is_echoed_escaped_and_short() {
    let v = parse_rules(&body(vec![{
        let mut r = rule_json(1, ITEM_A, "10");
        r["basis"] = json!(format!("\"{}", "b".repeat(100)));
        r
    }]))
    .unwrap_err();
    assert!(v[0].detail.contains("\\\""), "{}", v[0].detail);
    assert!(!v[0].detail.contains(&"b".repeat(40)), "{}", v[0].detail);
}

#[test]
fn typed_rules_are_judged_with_the_parses_codes() {
    let good = parsed(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_B, "20")]);
    assert!(rule_violations(&good).is_empty());
    let mut bad = good.clone();
    bad[0].logical_binding_key = "a/b".into();
    bad[0].amount = "1.0".into();
    bad[1].rule_id = bad[0].rule_id;
    bad[1].scope_selection = (0..9).map(|n| (format!("k{n}"), "v".to_owned())).collect();
    bad[1].unit_ref = "u\u{0}".into();
    bad[1].dimension_value = Some("d".repeat(MAX_TEXT_BYTES + 1));
    assert_eq!(
        paths(&rule_violations(&bad)),
        [
            (
                "QUOTA_RULE_INVALID",
                "quota_rules[0].logical_binding_key".to_owned()
            ),
            ("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount".to_owned()),
            ("QUOTA_RULE_INVALID", "quota_rules[1].rule_id".to_owned()),
            (
                "QUOTA_RULE_INVALID",
                "quota_rules[1].dimension_value".to_owned()
            ),
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "quota_rules[1].scope_selection".to_owned()
            ),
            ("QUOTA_RULE_INVALID", "quota_rules[1].unit_ref".to_owned()),
        ]
    );
    let many: Vec<PlanQuotaRuleV1> = (0..201)
        .map(|n| PlanQuotaRuleV1 {
            rule_id: Uuid::from_u128(n),
            ..good[0].clone()
        })
        .collect();
    assert_eq!(
        paths(&rule_violations(&many)),
        one("QUOTA_RULE_INVALID", "quota_rules")
    );
}

#[test]
fn a_rule_for_a_foreign_item_is_rule_invalid() {
    let rules = parsed(vec![rule_json(1, Uuid::from_u128(0xf), "10")]);
    let items = [RuleItem {
        id: ITEM_A,
        sku_id: Uuid::from_u128(5),
        dimension_key: None,
    }];
    assert_eq!(
        paths(&item_violations(&rules, &items, &[])),
        one("QUOTA_RULE_INVALID", "quota_rules[0].item_id")
    );
}

#[test]
fn a_dimension_value_must_be_the_items_registered_value() {
    let items = [
        RuleItem {
            id: ITEM_A,
            sku_id: Uuid::from_u128(5),
            dimension_key: Some("region".into()),
        },
        RuleItem {
            id: ITEM_B,
            sku_id: Uuid::from_u128(6),
            dimension_key: None,
        },
    ];
    let registry = vec![("region".to_owned(), vec!["eu".to_owned(), "us".to_owned()])];
    let mut eu = rule_json(1, ITEM_A, "10");
    eu["dimension_value"] = json!("eu");
    let mut apac = rule_json(2, ITEM_A, "10");
    apac["dimension_value"] = json!("apac");
    let mut on_b = rule_json(3, ITEM_B, "10");
    on_b["dimension_value"] = json!("eu");
    let rules = parsed(vec![eu, apac, on_b, rule_json(4, ITEM_B, "1")]);
    assert_eq!(
        paths(&item_violations(&rules, &items, &registry)),
        [
            (
                "QUOTA_RULE_INVALID",
                "quota_rules[1].dimension_value".to_owned()
            ),
            (
                "QUOTA_RULE_INVALID",
                "quota_rules[2].dimension_value".to_owned()
            ),
        ]
    );
}

/// D-541 (the owner's ruling of 2026-11-22): a rule with no dimension value on an item that has a
/// dimension is the item's default rule for its quota. It is accepted beside a rule for a value of
/// the same quota; two default rules for one quota of one item are a duplicate.
#[test]
fn a_default_rule_on_an_item_with_a_dimension_is_accepted() {
    let items = [RuleItem {
        id: ITEM_A,
        sku_id: Uuid::from_u128(5),
        dimension_key: Some("region".into()),
    }];
    let registry = vec![("region".to_owned(), vec!["eu".to_owned(), "us".to_owned()])];
    let default = rule_json(1, ITEM_A, "10");
    let mut eu = rule_json(2, ITEM_A, "20");
    eu["dimension_value"] = json!("eu");
    let rules = parsed(vec![default.clone(), eu]);
    assert!(
        item_violations(&rules, &items, &registry).is_empty(),
        "a default rule and a rule for eu"
    );
    let metric = BTreeMap::from([
        (Uuid::from_u128(1), "vcpu".to_owned()),
        (Uuid::from_u128(2), "vcpu".to_owned()),
        (Uuid::from_u128(3), "vcpu".to_owned()),
    ]);
    assert!(
        duplicate_violations(&rules, &metric).is_empty(),
        "the default and the eu rule of one quota are alternatives"
    );
    let mut again = default.clone();
    again["rule_id"] = json!(Uuid::from_u128(3));
    let twice = parsed(vec![default, again]);
    assert!(item_violations(&twice, &items, &registry).is_empty());
    assert_eq!(
        paths(&duplicate_violations(&twice, &metric)),
        [
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[0].logical_binding_key".to_owned()
            ),
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[1].logical_binding_key".to_owned()
            )
        ],
        "two default rules for one quota of one item"
    );
}

#[test]
fn a_rule_whose_sku_has_no_version_on_the_sale_date_is_rule_invalid() {
    let items = [
        RuleItem {
            id: ITEM_A,
            sku_id: Uuid::from_u128(5),
            dimension_key: None,
        },
        RuleItem {
            id: ITEM_B,
            sku_id: Uuid::from_u128(6),
            dimension_key: None,
        },
    ];
    let rules = parsed(vec![
        rule_json(1, ITEM_A, "10"),
        rule_json(2, ITEM_B, "10"),
        rule_json(3, Uuid::from_u128(0xf), "10"),
    ]);
    let versions = BTreeMap::from([(Uuid::from_u128(5), 3)]);
    let v = version_violations(&rules, &items, &versions);
    assert_eq!(
        paths(&v),
        one("QUOTA_RULE_INVALID", "quota_rules[1].item_id"),
        "a foreign item is the item check's, not this one's"
    );
    assert!(v[0].detail.contains("no version"), "{}", v[0].detail);
    let not_positive = BTreeMap::from([(Uuid::from_u128(5), 0), (Uuid::from_u128(6), 1)]);
    assert_eq!(
        paths(&version_violations(&rules, &items, &not_positive)),
        one("QUOTA_RULE_INVALID", "quota_rules[0].item_id"),
        "a version below 1 is no version"
    );
}

#[test]
fn two_rules_for_one_binding_and_metric_that_apply_together_are_duplicates() {
    let rules = parsed(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_B, "20")]);
    let metric = BTreeMap::from([
        (Uuid::from_u128(1), "vcpu".to_owned()),
        (Uuid::from_u128(2), "vcpu".to_owned()),
    ]);
    assert_eq!(
        paths(&duplicate_violations(&rules, &metric)),
        [
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[0].logical_binding_key".to_owned()
            ),
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[1].logical_binding_key".to_owned()
            )
        ]
    );
    let other = BTreeMap::from([
        (Uuid::from_u128(1), "vcpu".to_owned()),
        (Uuid::from_u128(2), "ram".to_owned()),
    ]);
    assert!(
        duplicate_violations(&rules, &other).is_empty(),
        "two metrics on one binding are a set, not duplicates"
    );
    let mut apart = rules.clone();
    apart[1].logical_binding_key = "storage".into();
    assert!(
        duplicate_violations(&apart, &metric).is_empty(),
        "one metric on two bindings is two limits"
    );
    let unresolved = BTreeMap::from([(Uuid::from_u128(1), "vcpu".to_owned())]);
    assert!(
        duplicate_violations(&rules, &unresolved).is_empty(),
        "a rule without a resolved definition has no metric to compare"
    );
}

#[test]
fn dimension_alternatives_of_one_item_are_not_duplicates() {
    let mut eu = rule_json(1, ITEM_A, "10");
    eu["dimension_value"] = json!("eu");
    let mut us = rule_json(2, ITEM_A, "20");
    us["dimension_value"] = json!("us");
    let rules = parsed(vec![eu.clone(), us]);
    let metric = BTreeMap::from([
        (Uuid::from_u128(1), "vcpu".to_owned()),
        (Uuid::from_u128(2), "vcpu".to_owned()),
    ]);
    assert!(duplicate_violations(&rules, &metric).is_empty());
    let mut eu_again = eu.clone();
    eu_again["rule_id"] = json!(Uuid::from_u128(2));
    let same_value = parsed(vec![eu, eu_again.clone()]);
    assert_eq!(
        paths(&duplicate_violations(&same_value, &metric)),
        [
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[0].logical_binding_key".to_owned()
            ),
            (
                "QUOTA_RULE_DUPLICATE",
                "quota_rules[1].logical_binding_key".to_owned()
            )
        ],
        "one item with one value twice applies together"
    );
    eu_again["rule_id"] = json!(Uuid::from_u128(3));
    let rules = parse_rules(&body(vec![
        rule_json(9, ITEM_A, "1"),
        eu_again.clone(),
        eu_again,
    ]));
    assert!(
        rules.is_err(),
        "a repeated rule id is refused before duplicates are judged"
    );
}

fn check(code: QuotaCheckCode, rules: &[u128], field: &str, detail: &str) -> QuotaCheck {
    QuotaCheck {
        code,
        rule_ids: rules.iter().map(|n| Uuid::from_u128(*n)).collect(),
        field: field.into(),
        detail: detail.into(),
    }
}
/// The rule ids of [`unjudged`]'s answer, in its order.
fn ids(unjudged: &[(Uuid, &str)]) -> Vec<Uuid> {
    unjudged.iter().map(|(rule_id, _)| *rule_id).collect()
}
fn validation(checks: Vec<QuotaCheck>) -> QuotaSetValidation {
    QuotaSetValidation {
        rules: Vec::new(),
        checks,
    }
}

#[test]
fn catalog_checks_map_to_rule_paths_and_a_refusal_status() {
    let rules = parsed(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_B, "20")]);
    let v = catalog_violations(
        &rules,
        &validation(vec![
            check(
                QuotaCheckCode::ScopeUnsupported,
                &[2],
                "scope_selection.region",
                "missing",
            ),
            check(
                QuotaCheckCode::DefinitionChanged,
                &[1],
                "definition_ref.digest",
                "changed",
            ),
        ]),
    );
    assert_eq!(
        paths(&v),
        [
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "quota_rules[1].scope_selection".to_owned()
            ),
            (
                "QUOTA_DEFINITION_CHANGED",
                "quota_rules[0].definition_ref".to_owned()
            )
        ]
    );
    assert_eq!(
        v[0].detail, "scope_selection.region: missing",
        "the detail keeps the full path"
    );
    assert_eq!(v[0].rule_id, Some(Uuid::from_u128(2)));
    assert_eq!(refusal_status(&v), 400);
    assert_eq!(refusal_status(&v[1..]), 409);
}

#[test]
fn products_fields_that_name_no_rule_member_map_to_the_member_they_are_about() {
    for (path, member) in [
        ("change_kind", "definition_ref"),
        ("prior", "amount"),
        ("quantity", "amount"),
        ("definition_ref.digest", "definition_ref"),
        ("scope_selection.resource_pool", "scope_selection"),
        ("scope_selection", "scope_selection"),
        ("subject_selector", "subject_selector"),
        ("logical_binding_key", "logical_binding_key"),
        ("unit_ref", "unit_ref"),
        ("basis", "basis"),
        ("amount", "amount"),
        ("rules[3].unit_ref", ""),
        ("locale", ""),
    ] {
        assert_eq!(top_field(path), member, "{path}");
    }
    let rules = parsed(vec![rule_json(1, ITEM_A, "10")]);
    let v = catalog_violations(
        &rules,
        &validation(vec![
            check(
                QuotaCheckCode::SetIncompatible,
                &[1],
                "change_kind",
                "no initial set",
            ),
            check(QuotaCheckCode::AmountInvalid, &[1], "quantity", "over max"),
            check(QuotaCheckCode::SetIncompatible, &[1], "prior", "a decrease"),
        ]),
    );
    assert_eq!(
        paths(&v),
        [
            (
                "QUOTA_SET_INCOMPATIBLE",
                "quota_rules[0].definition_ref".to_owned()
            ),
            ("QUOTA_AMOUNT_INVALID", "quota_rules[0].amount".to_owned()),
            ("QUOTA_SET_INCOMPATIBLE", "quota_rules[0].amount".to_owned()),
        ]
    );
    assert_eq!(v[0].detail, "change_kind: no initial set");
}

#[test]
fn a_set_check_names_each_of_its_rules_and_one_naming_none_is_on_the_set() {
    let rules = parsed(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_B, "20")]);
    let v = catalog_violations(
        &rules,
        &validation(vec![
            check(
                QuotaCheckCode::SetIncompatible,
                &[1, 2],
                "logical_binding_key",
                "two targets",
            ),
            check(
                QuotaCheckCode::SetIncompatible,
                &[],
                "prior",
                "no rule keeps compute/vcpu",
            ),
        ]),
    );
    assert_eq!(
        paths(&v),
        [
            (
                "QUOTA_SET_INCOMPATIBLE",
                "quota_rules[0].logical_binding_key".to_owned()
            ),
            (
                "QUOTA_SET_INCOMPATIBLE",
                "quota_rules[1].logical_binding_key".to_owned()
            ),
            ("QUOTA_SET_INCOMPATIBLE", "quota_rules".to_owned()),
        ]
    );
    assert_eq!((v[2].index, v[2].rule_id), (None, None));
    assert_eq!(v[2].detail, "prior: no rule keeps compute/vcpu");
}

/// A check naming a rule Pricing did not send is kept on the set, with the rule's id in the
/// detail.
#[test]
fn a_check_naming_a_rule_that_was_not_sent_is_on_the_set() {
    let rules = parsed(vec![rule_json(1, ITEM_A, "10")]);
    let v = catalog_violations(
        &rules,
        &validation(vec![check(
            QuotaCheckCode::AmountInvalid,
            &[9],
            "amount",
            "over max",
        )]),
    );
    assert_eq!(paths(&v), one("QUOTA_AMOUNT_INVALID", "quota_rules"));
    assert_eq!((v[0].index, v[0].rule_id), (None, None));
    assert_eq!(
        v[0].detail,
        format!("rule {}: over max", Uuid::from_u128(9))
    );
}

/// A rule Products judged without resolving its definition, and that no check names, is not
/// judged: [`unjudged`] finds it, and the checks show it red `QUOTA_RULE_INVALID` on its
/// definition reference. A rule some check names keeps only that check.
#[test]
fn a_rule_with_no_definition_and_no_check_is_rule_invalid() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_A, "20")]);
    let unresolved = |n: u128| QuotaRuleVerdict {
        rule_id: Uuid::from_u128(n),
        definition: None,
        limit: None,
    };
    let verdict = QuotaSetValidation {
        rules: vec![unresolved(1), unresolved(2)],
        checks: vec![check(
            QuotaCheckCode::ScopeUnsupported,
            &[2],
            "scope_selection",
            "missing",
        )],
    };
    assert_eq!(
        ids(&unjudged(&ctx.quota_rules, &verdict)),
        [Uuid::from_u128(1)]
    );
    assert!(
        catalog_violations(&ctx.quota_rules, &verdict)
            .iter()
            .all(|v| v.rule_id == Some(Uuid::from_u128(2))),
        "the checks of Products name only rule 2"
    );
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(
        red_codes(&rows),
        ["QUOTA_RULE_INVALID", "QUOTA_SCOPE_UNSUPPORTED"]
    );
    assert_eq!(
        rows[0]
            .subjects
            .iter()
            .map(|s| s.quota.unwrap())
            .collect::<Vec<_>>(),
        [RuleSubject {
            rule_id: Uuid::from_u128(1),
            field: "definition_ref"
        }]
    );
}

/// A rule Products was sent but left out of its verdict, naming it in no check, reads red, never
/// green. A rule Pricing did not send (its item is not the revision's) is reported once, by
/// Pricing's own check, and a judged rule stays green.
#[test]
fn a_rule_products_leaves_out_of_its_verdict_reads_red() {
    let ctx = context_with_rules(vec![
        rule_json(1, ITEM_A, "10"),
        rule_json(2, ITEM_A, "20"),
        rule_json(3, ITEM_B, "30"),
    ]);
    let sent = sent_rules(
        &ctx.quota_rules,
        &[RuleItem {
            id: ITEM_A,
            sku_id: SKU,
            dimension_key: None,
        }],
        &ctx.quota_sku_versions,
    );
    assert_eq!(
        sent.iter().map(|rule| rule.rule_id).collect::<Vec<_>>(),
        [Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    let verdict = judged(&[1]);
    assert_eq!(
        ids(&unjudged(sent.iter().copied(), &verdict)),
        [Uuid::from_u128(2)]
    );
    let empty = validation(Vec::new());
    assert_eq!(
        ids(&unjudged(sent, &empty)),
        [Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_INVALID"]);
    let invalid = &rows[0];
    assert_eq!(
        invalid
            .subjects
            .iter()
            .map(|s| s.quota.unwrap())
            .collect::<Vec<_>>(),
        [RuleSubject {
            rule_id: Uuid::from_u128(2),
            field: "definition_ref"
        }],
        "the rule left out names its definition reference; the foreign item's rule has no subject"
    );
    assert_eq!(
        invalid.detail.matches("quota_rules[2].item_id").count(),
        1,
        "{}",
        invalid.detail
    );
    assert!(
        !invalid.detail.contains("quota_rules[2].definition_ref"),
        "a rule Pricing did not send is not reported twice: {}",
        invalid.detail
    );
}

#[test]
fn only_a_changed_definition_or_a_duplicate_is_a_conflict() {
    let rules = parsed(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_B, "20")]);
    let metric = BTreeMap::from([
        (Uuid::from_u128(1), "vcpu".to_owned()),
        (Uuid::from_u128(2), "vcpu".to_owned()),
    ]);
    let mut v = duplicate_violations(&rules, &metric);
    v.extend(catalog_violations(
        &rules,
        &validation(vec![check(
            QuotaCheckCode::DefinitionChanged,
            &[1],
            "definition_ref.digest",
            "x",
        )]),
    ));
    assert_eq!(refusal_status(&v), 409);
    for code in [
        QuotaCheckCode::RuleInvalid,
        QuotaCheckCode::AmountInvalid,
        QuotaCheckCode::UnitInexact,
        QuotaCheckCode::ScopeUnsupported,
        QuotaCheckCode::SetIncompatible,
        QuotaCheckCode::QualificationUnavailable,
    ] {
        let mut mixed = v.clone();
        mixed.extend(catalog_violations(
            &rules,
            &validation(vec![check(code, &[2], "amount", "x")]),
        ));
        assert_eq!(refusal_status(&mixed), 400, "{}", code.code());
    }
    assert_eq!(refusal_status(&[]), 400, "no violation is no conflict");
    assert_eq!(
        SAVE_IGNORES,
        QuotaCheckCode::QualificationUnavailable.code()
    );
    assert_eq!(
        CONFLICT_CODES,
        ["QUOTA_RULE_DUPLICATE", "QUOTA_DEFINITION_CHANGED"]
    );
}

// ---------------------------------------------------------------- the quota rows of the checks

const SKU: Uuid = Uuid::from_u128(5);
const ENTRY: Uuid = Uuid::from_u128(0xe);
const BOOK: Uuid = Uuid::from_u128(0xbb);
/// The sale date the quota rows judge: 2026-11-21.
const SALE: time::Date = match time::Date::from_calendar_date(2026, time::Month::November, 21) {
    Ok(date) => date,
    Err(_) => panic!("a calendar date"),
};

/// One item `ITEM_A` (SKU 5, an entry without a dimension key) whose SKU has version 3 on the
/// sale date, and the parsed `rules`; no verdict yet.
fn context_with_rules(rules: Vec<Value>) -> PlanContext {
    use crate::domain::plan::{
        Defaults, Entry, Item, Plan, Reference, ReferenceState, Revision, RevisionState,
    };
    use crate::domain::price_book_entry::{ChargeKind, ReferenceState as EntryReference};
    PlanContext {
        plan: Plan {
            id: Uuid::from_u128(100),
            code: "pro".into(),
            name: "Pro".into(),
        },
        revision: Revision {
            id: Uuid::from_u128(200),
            rev_no: 1,
            book_id: BOOK,
            state: RevisionState::Draft,
            available_from: None,
        },
        items: vec![Item {
            id: ITEM_A,
            sku_id: SKU,
            price_book_entry_id: Some(ENTRY),
            reference: Reference {
                state: ReferenceState::Confirmed,
                reservation_id: Some(Uuid::from_u128(99)),
            },
        }],
        skus: Vec::new(),
        entries: vec![Entry {
            id: ENTRY,
            book_id: BOOK,
            sku_id: SKU,
            charge_kind: ChargeKind::Recurring,
            period: Some("month".into()),
            dimension_key: None,
            reference_state: EntryReference::Confirmed,
            prices: Vec::new(),
            pending: Vec::new(),
        }],
        books: Vec::new(),
        dimension_values: Vec::new(),
        published_sku_ids: Vec::new(),
        quorum: 0,
        defaults: Defaults {
            gl: None,
            tax_category: None,
        },
        quota_rules: parsed(rules),
        quota_digest: "d".repeat(64),
        quota_verdict: None,
        quota_sku_versions: BTreeMap::from([(SKU, 3)]),
        revision_version: 1,
    }
}
/// A resolved definition of `metric`, as Products answers it.
fn definition_of(metric: &str) -> bss_products_sdk::quota::catalog::QuotaDefinition {
    let policy =
        "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated_vcpu.v1";
    let scope =
        "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1";
    serde_json::from_value(json!({
        "definition_id": DEF, "version": "1", "definition_digest": "a".repeat(64),
        "sku_id": SKU, "sku_version": "3", "metric_ref": metric, "metric_digest": "b".repeat(64),
        "quota_kind": "allocation", "display_name": "m", "description": "m", "locale": "en",
        "canonical_unit_ref": VCPU_UNIT, "allowed_units": [], "subject_type_ref": "t",
        "resource_type_refs": [],
        "mapping_ref": {"id": Uuid::from_u128(0xa1), "version": "1", "digest": "d".repeat(64)},
        "profile_ref": {"id": Uuid::from_u128(0xb1), "version": "1", "digest": "e".repeat(64)},
        "qualification_ref": null, "accounting_policy_ref": policy,
        "accounting_policy_digest": "2".repeat(64),
        "accounting_policy": {"id": policy, "quantity_basis": "allocated_capacity",
            "aggregation": "sum", "description": {"en": "Counts allocated capacity."}},
        "scope_policy_ref": scope, "scope_policy_digest": "3".repeat(64),
        "scope_policy": {"id": scope, "subject_selector": "resource_owner",
            "aggregation": "single_verified_scope", "required_dimensions": ["region"]},
        "change_support": {"initial_set": true, "increase": true},
        "profile_conditions": "c", "supported_bases": ["Fixed", "PerPlanUnit"],
        "availability": {"state": "enabled", "revision": "2",
            "observed_at": "2026-11-21T00:00:00Z", "reason_code": null}
    }))
    .unwrap()
}
fn red_codes(rows: &[Check]) -> Vec<&'static str> {
    rows.iter().filter(|r| !r.ok).map(|r| r.code).collect()
}
/// Products' verdict on `rules`: each resolved with a vCPU definition, and no check.
fn judged(rules: &[u128]) -> QuotaSetValidation {
    QuotaSetValidation {
        rules: rules
            .iter()
            .map(|n| QuotaRuleVerdict {
                rule_id: Uuid::from_u128(*n),
                definition: Some(definition_of("cf.compute.vcpu")),
                limit: None,
            })
            .collect(),
        checks: Vec::new(),
    }
}

#[test]
fn quota_rows_name_the_rule_and_field() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let verdict = validation(vec![check(
        QuotaCheckCode::AmountInvalid,
        &[1],
        "amount",
        "11 is off the grid",
    )]);
    let rows = check_rows(&ctx, &verdict, SALE);
    let amount = rows
        .iter()
        .find(|r| r.code == "QUOTA_AMOUNT_INVALID")
        .unwrap();
    assert!(!amount.ok);
    assert_eq!(
        amount.subjects[0].quota,
        Some(RuleSubject {
            rule_id: Uuid::from_u128(1),
            field: "amount"
        })
    );
    assert_eq!(amount.subjects[0].item, ITEM_A);
    assert_eq!(
        (amount.subjects[0].sku, amount.subjects[0].entry),
        (SKU, Some(ENTRY))
    );
    assert_eq!(amount.detail, "quota_rules[0].amount: 11 is off the grid");
    assert_eq!(amount.label, "Quota amounts are on their grids");
    assert!(!amount.info && amount.blocked_by_prices.is_empty());
    assert_eq!(
        rows.iter().filter(|r| r.ok).count(),
        8,
        "the other eight rows are green"
    );
}

#[test]
fn a_disabled_definition_turns_only_the_qualification_row_red() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let verdict = validation(vec![check(
        QuotaCheckCode::QualificationUnavailable,
        &[1],
        "definition_ref",
        "disabled: qualification_pending",
    )]);
    assert_eq!(
        red_codes(&check_rows(&ctx, &verdict, SALE)),
        ["QUOTA_QUALIFICATION_UNAVAILABLE"]
    );
}

#[test]
fn the_quota_rows_keep_their_order() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let rows = check_rows(&ctx, &judged(&[1]), SALE);
    let codes: Vec<&str> = rows.iter().map(|r| r.code).collect();
    assert_eq!(
        codes,
        [
            "QUOTA_RULE_INVALID",
            "QUOTA_DEFINITION_CHANGED",
            "QUOTA_AMOUNT_INVALID",
            "QUOTA_UNIT_INEXACT",
            "QUOTA_SCOPE_UNSUPPORTED",
            "QUOTA_RULE_DUPLICATE",
            "QUOTA_SET_INCOMPATIBLE",
            "QUOTA_QUALIFICATION_UNAVAILABLE",
            "QUOTA_RULE_MISSING"
        ]
    );
    assert!(
        rows.iter()
            .all(|r| r.ok && r.subjects.is_empty() && r.detail == "ok")
    );
}

#[test]
fn the_rows_judge_items_versions_and_duplicates_from_the_context() {
    let mut ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_A, "20")]);
    ctx.quota_sku_versions.clear();
    let verdict = QuotaSetValidation {
        rules: [1, 2]
            .into_iter()
            .map(|n| QuotaRuleVerdict {
                rule_id: Uuid::from_u128(n),
                definition: Some(definition_of("cf.compute.vcpu")),
                limit: None,
            })
            .collect(),
        checks: Vec::new(),
    };
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(
        red_codes(&rows),
        ["QUOTA_RULE_INVALID", "QUOTA_RULE_DUPLICATE"]
    );
    let invalid = &rows[0];
    assert_eq!(
        invalid
            .subjects
            .iter()
            .map(|s| s.quota.unwrap())
            .collect::<Vec<_>>(),
        [
            RuleSubject {
                rule_id: Uuid::from_u128(1),
                field: "item_id"
            },
            RuleSubject {
                rule_id: Uuid::from_u128(2),
                field: "item_id"
            }
        ],
        "a SKU without a version on the sale date names each rule's item"
    );
    let duplicate = rows
        .iter()
        .find(|r| r.code == "QUOTA_RULE_DUPLICATE")
        .unwrap();
    assert_eq!(duplicate.subjects.len(), 2);
    assert!(duplicate.subjects.iter().all(|s| s.item == ITEM_A));

    ctx.quota_sku_versions.insert(SKU, 3);
    ctx.quota_rules.truncate(1);
    ctx.quota_rules[0].dimension_value = Some("eu".into());
    let rows = check_rows(&ctx, &judged(&[1]), SALE);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_INVALID"]);
    assert_eq!(
        rows[0].subjects[0].quota.unwrap().field,
        "dimension_value",
        "an item without a dimension takes no dimension value"
    );
}

#[test]
fn a_check_that_names_no_rule_is_a_line_of_its_row_without_a_subject() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let verdict = validation(vec![
        check(
            QuotaCheckCode::SetIncompatible,
            &[],
            "prior",
            "no rule keeps compute/vcpu",
        ),
        check(
            QuotaCheckCode::SetIncompatible,
            &[1],
            "change_kind",
            "no increase",
        ),
    ]);
    let rows = check_rows(&ctx, &verdict, SALE);
    let set = rows
        .iter()
        .find(|r| r.code == "QUOTA_SET_INCOMPATIBLE")
        .unwrap();
    assert!(!set.ok);
    assert_eq!(
        set.detail,
        "quota_rules: prior: no rule keeps compute/vcpu; \
         quota_rules[0].definition_ref: change_kind: no increase"
    );
    assert_eq!(set.subjects.len(), 1, "the set-wide line names no item");
    assert_eq!(set.subjects[0].quota.unwrap().field, "definition_ref");
}

#[test]
fn the_quota_rows_follow_every_other_row_and_fail_closed_without_a_verdict() {
    let mut ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let today = time::Date::from_calendar_date(2026, time::Month::November, 21).unwrap();
    let rest = {
        let mut quota_free = ctx.clone();
        quota_free.quota_rules.clear();
        crate::domain::plan::checks(&quota_free, today)
    };
    assert!(rest.iter().all(|r| !r.code.starts_with("QUOTA_")));
    let unjudged = crate::domain::plan::checks(&ctx, today);
    assert_eq!(
        unjudged.len(),
        rest.len() + 9,
        "rules with no verdict still get their nine rows"
    );
    assert_eq!(&unjudged[..rest.len()], rest.as_slice());
    assert_eq!(
        red_codes(&unjudged[rest.len()..]),
        ["QUOTA_QUALIFICATION_UNAVAILABLE"],
        "a revision with rules and no verdict fails closed"
    );
    let red = unjudged
        .iter()
        .find(|r| r.code == "QUOTA_QUALIFICATION_UNAVAILABLE")
        .unwrap();
    assert_eq!(
        red.subjects
            .iter()
            .map(|s| (s.item, s.quota.unwrap()))
            .collect::<Vec<_>>(),
        [(
            ITEM_A,
            RuleSubject {
                rule_id: Uuid::from_u128(1),
                field: "definition_ref"
            }
        )]
    );
    assert!(red.detail.contains("verdict"), "{}", red.detail);
    assert!(!crate::domain::plan::ready(&unjudged));
    ctx.quota_verdict = Some(validation(vec![check(
        QuotaCheckCode::UnitInexact,
        &[1],
        "amount",
        "not whole",
    )]));
    let with = crate::domain::plan::checks(&ctx, today);
    assert_eq!(with.len(), rest.len() + 9);
    assert_eq!(&with[..rest.len()], rest.as_slice());
    assert!(
        with[rest.len()..]
            .iter()
            .all(|r| r.code.starts_with("QUOTA_"))
    );
    assert_eq!(red_codes(&with[rest.len()..]), ["QUOTA_UNIT_INEXACT"]);
    assert!(!crate::domain::plan::ready(&with));
}

/// Every code Products can answer has a row of its own, under its own token: a check of any code,
/// on a rule or on the whole set, turns that row red and the revision is not ready.
#[test]
fn every_products_code_turns_its_own_row_red() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    for code in QuotaCheckCode::ALL {
        assert_eq!(row_code(code), code.code(), "{code:?}");
        assert!(
            ROWS.iter().any(|(row, _)| *row == row_code(code)),
            "{code:?} has a row"
        );
        for rules in [&[1][..], &[]] {
            let verdict = QuotaSetValidation {
                checks: vec![check(code, rules, "amount", "x")],
                ..judged(&[1])
            };
            let rows = check_rows(&ctx, &verdict, SALE);
            assert_eq!(red_codes(&rows), [code.code()], "{code:?} on {rules:?}");
            assert!(!crate::domain::plan::ready(&rows), "{code:?} on {rules:?}");
        }
    }
}

/// A violation whose code has no row of its own is never dropped and never green: it turns
/// `QUOTA_RULE_INVALID` red, its code in its line.
#[test]
fn a_violation_without_a_row_of_its_own_turns_rule_invalid_red() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let stray = Violation {
        code: "QUOTA_SOMETHING_NEW",
        index: Some(0),
        rule_id: Some(Uuid::from_u128(1)),
        field: "amount",
        detail: "new".to_owned(),
        item: None,
    };
    let rows = rows(&ctx, &[stray]);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_INVALID"]);
    assert_eq!(
        rows[0].detail,
        "quota_rules[0].amount: QUOTA_SOMETHING_NEW: new"
    );
    assert_eq!(
        rows[0].subjects[0].quota,
        Some(RuleSubject {
            rule_id: Uuid::from_u128(1),
            field: "amount"
        })
    );
    assert!(!crate::domain::plan::ready(&rows));
}

/// A rule counts as judged only against the definition it names (its id, version and digest): a
/// verdict that resolves it to another version or another definition, or to another digest with
/// no `QUOTA_DEFINITION_CHANGED` check on it, is Products' contract break, red
/// `QUOTA_RULE_INVALID` on the rule's definition reference, whatever other check names it. A
/// changed digest that Products names is only its own row, and the named definition is green.
#[test]
fn a_rule_is_judged_only_against_the_definition_it_names() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    let reference = &ctx.quota_rules[0].definition_ref;
    let resolving = |change: fn(&mut QuotaDefinition), checks: Vec<QuotaCheck>| {
        let mut verdict = judged(&[1]);
        change(verdict.rules[0].definition.as_mut().unwrap());
        QuotaSetValidation { checks, ..verdict }
    };
    let other_version: fn(&mut QuotaDefinition) = |d| d.version = QuotaVersion::new(2).unwrap();
    let other_id: fn(&mut QuotaDefinition) = |d| d.definition_id = Uuid::from_u128(0xdd);
    let other_digest: fn(&mut QuotaDefinition) = |d| d.definition_digest = "b".repeat(64);
    let scope = || {
        vec![check(
            QuotaCheckCode::ScopeUnsupported,
            &[1],
            "scope_selection",
            "missing",
        )]
    };
    assert!(names_definition(
        reference,
        &definition_of("cf.compute.vcpu")
    ));
    for (what, verdict) in [
        ("another version", resolving(other_version, Vec::new())),
        ("another definition", resolving(other_id, Vec::new())),
        ("another digest", resolving(other_digest, Vec::new())),
        (
            "another version beside a check",
            resolving(other_version, scope()),
        ),
        (
            "another digest beside another check",
            resolving(other_digest, scope()),
        ),
    ] {
        let resolved = verdict.rules[0].definition.as_ref().unwrap();
        assert!(!names_definition(reference, resolved), "{what}");
        assert_eq!(
            ids(&unjudged(&ctx.quota_rules, &verdict)),
            [Uuid::from_u128(1)],
            "{what}"
        );
        let rows = check_rows(&ctx, &verdict, SALE);
        assert!(!rows[0].ok, "{what}: {:?}", rows[0]);
        assert_eq!(
            rows[0]
                .subjects
                .iter()
                .map(|s| s.quota.unwrap())
                .collect::<Vec<_>>(),
            [RuleSubject {
                rule_id: Uuid::from_u128(1),
                field: "definition_ref"
            }],
            "{what}"
        );
        assert!(
            rows[0].detail.contains("another definition"),
            "{what}: {}",
            rows[0].detail
        );
        assert!(!crate::domain::plan::ready(&rows), "{what}");
    }
    let changed = resolving(
        other_digest,
        vec![check(
            QuotaCheckCode::DefinitionChanged,
            &[1],
            "definition_ref.digest",
            "changed",
        )],
    );
    assert!(unjudged(&ctx.quota_rules, &changed).is_empty());
    assert_eq!(
        red_codes(&check_rows(&ctx, &changed, SALE)),
        ["QUOTA_DEFINITION_CHANGED"]
    );
    assert!(red_codes(&check_rows(&ctx, &judged(&[1]), SALE)).is_empty());
}

/// The duplicates are judged by the metrics of the definition versions the rules name: a verdict
/// that resolves a rule to another version lends it no metric.
#[test]
fn a_rule_resolved_to_another_version_lends_no_metric() {
    let ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10"), rule_json(2, ITEM_A, "20")]);
    let mut verdict = judged(&[1, 2]);
    assert_eq!(resolved_metrics(&ctx.quota_rules, &verdict).len(), 2);
    verdict.rules[1].definition.as_mut().unwrap().version = QuotaVersion::new(2).unwrap();
    assert_eq!(
        resolved_metrics(&ctx.quota_rules, &verdict),
        BTreeMap::from([(Uuid::from_u128(1), "cf.compute.vcpu".to_owned())])
    );
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_INVALID"], "no duplicate");
}

/// A refusal names at most the first ten unknown members of the body and of each rule, and counts
/// the rest in one more violation: however many stray members a body sends, its refusal stays
/// small. Ten unknown members are named with no count.
#[test]
fn many_unknown_members_name_the_first_ten_and_count_the_rest() {
    let mut stray = rule_json(1, ITEM_A, "10");
    let mut members = serde_json::Map::new();
    for n in 0..1000 {
        stray[format!("extra_{n:04}").as_str()] = json!(true);
        members.insert(format!("stray_{n:04}"), json!(true));
    }
    members.insert(
        "quota_rules".to_owned(),
        json!([stray, rule_json(2, ITEM_A, "20")]),
    );
    let v = parse_rules(&Value::Object(members)).unwrap_err();
    assert_eq!(v.len(), 22, "{v:#?}");
    assert!(v[..11].iter().all(|v| v.path() == "quota_rules"), "{v:#?}");
    assert!(
        v[11..].iter().all(|v| v.path() == "quota_rules[0]"),
        "{v:#?}"
    );
    assert!(v.iter().all(|v| v.code == "QUOTA_RULE_INVALID"));
    assert_eq!(v[0].detail, "\"stray_0000\" is not a member of the body");
    assert_eq!(v[9].detail, "\"stray_0009\" is not a member of the body");
    assert_eq!(v[10].detail, "990 more members are not members of the body");
    assert_eq!(v[11].detail, "\"extra_0000\" is not a member of a rule");
    assert_eq!(v[21].detail, "990 more members are not members of a rule");
    let mut ten = rule_json(1, ITEM_A, "10");
    for n in 0..10 {
        ten[format!("extra_{n}").as_str()] = json!(true);
    }
    let v = parse_rules(&body(vec![ten])).unwrap_err();
    assert_eq!(v.len(), 10, "ten are named, none counted: {v:#?}");
}

// ------------------------------------------------- an uncovered sellable cell (QUOTA_RULE_MISSING)

const VCPU: &str = "cf.compute.vcpu";
const RAM: &str = "cf.compute.ram";

/// [`context_with_rules`] with `ITEM_A`'s entry priced by `region`, whose registered values are
/// `values`.
fn by_region(rules: Vec<Value>, values: &[&str]) -> PlanContext {
    let mut ctx = context_with_rules(rules);
    ctx.entries[0].dimension_key = Some("region".into());
    ctx.dimension_values = vec![(
        "region".into(),
        values.iter().map(|v| (*v).to_owned()).collect(),
    )];
    ctx
}
/// Rule `n` on `ITEM_A` for `value` (`None`: the item's default rule) on `binding`.
fn cell_rule(n: u128, value: Option<&str>, binding: &str) -> Value {
    let mut rule = rule_json(n, ITEM_A, "10");
    rule["dimension_value"] = json!(value);
    rule["logical_binding_key"] = json!(binding);
    rule
}
/// Products' verdict: each rule resolved with a definition of its metric, and no check.
fn judged_as(rules: &[(u128, &str)]) -> QuotaSetValidation {
    QuotaSetValidation {
        rules: rules
            .iter()
            .map(|(n, metric)| QuotaRuleVerdict {
                rule_id: Uuid::from_u128(*n),
                definition: Some(definition_of(metric)),
                limit: None,
            })
            .collect(),
        checks: Vec::new(),
    }
}
/// An approved price of `ITEM_A`'s entry on the chain `dim` (`None`: the default chain) from
/// `from`, open-ended.
fn chain_price(n: u128, dim: Option<&str>, from: time::Date) -> crate::domain::price::Price {
    use crate::domain::{money::PriceData, price::*, price_book_entry::Model};
    Price {
        id: Uuid::from_u128(0x9000 + n),
        price_book_entry_id: ENTRY,
        version_no: 1,
        dim_value: dim.map(str::to_owned),
        model: Model::PerUnit,
        currency: crate::domain::test_support::spec("EUR", 2),
        price: Some(PriceData::PerUnit {
            rate: crate::domain::test_support::dec("0.10"),
        }),
        min_fee: None,
        eligibility: Eligibility::All,
        effective_from: from,
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state: PriceState::Approved,
    }
}
fn missing_row(rows: &[Check]) -> &Check {
    rows.iter()
        .find(|r| r.code == "QUOTA_RULE_MISSING")
        .unwrap()
}

/// D-547: a quota is a metric. Every sellable cell of an item with rules must select a rule of
/// each metric the item's rules carry: a registered value whose metric has neither a rule for
/// the value nor a default rule turns `QUOTA_RULE_MISSING` red, a line of the set naming the item,
/// the value and the metric, with the item as its subject (D-466) and no rule, and the revision
/// is not ready. A default rule of that metric turns it green.
#[test]
fn an_uncovered_sellable_cell_turns_quota_rule_missing_red() {
    assert_eq!(
        RULE_MISSING,
        bss_pricing_sdk::acceptance::CommercialReason::QuotaRuleMissing.code(),
        "the row carries the acceptance's code"
    );
    let rules = vec![
        cell_rule(1, None, "compute"),
        cell_rule(2, Some("eu"), "compute"),
        cell_rule(3, Some("us"), "compute"),
    ];
    let verdict = judged_as(&[(1, VCPU), (2, RAM), (3, RAM)]);
    let ctx = by_region(rules.clone(), &["eu", "us", "apac"]);
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_MISSING"]);
    let missing = missing_row(&rows);
    assert_eq!(
        missing.detail,
        "quota_rules: item 00000000-0000-0000-0000-00000000000a at value \"apac\": no rule of \
         \"cf.compute.ram\" for the value and no default rule"
    );
    assert_eq!(missing.label, "Every sellable cell has a rule per quota");
    assert_eq!(
        missing.subjects,
        [crate::domain::plan::Subject {
            item: ITEM_A,
            sku: SKU,
            entry: Some(ENTRY),
            quota: None,
        }],
        "the item turns the row red; no rule is at fault"
    );
    assert!(!crate::domain::plan::ready(&rows));

    let mut with_default = rules;
    with_default.push(cell_rule(4, None, "compute"));
    let ctx = by_region(with_default, &["eu", "us", "apac"]);
    let verdict = judged_as(&[(1, VCPU), (2, RAM), (3, RAM), (4, RAM)]);
    let rows = check_rows(&ctx, &verdict, SALE);
    assert!(red_codes(&rows).is_empty(), "a RAM default covers apac");
}

/// D-547: the checks judge a quota by its metric, whatever its binding: one binding per region
/// for one metric leaves no sellable cell uncovered; a cell no binding names is uncovered once.
#[test]
fn per_region_bindings_leave_no_sellable_cell_uncovered() {
    let rules = vec![
        cell_rule(1, Some("eu"), "compute-eu"),
        cell_rule(2, Some("us"), "compute-us"),
    ];
    let verdict = judged_as(&[(1, VCPU), (2, VCPU)]);
    let rows = check_rows(&by_region(rules.clone(), &["eu", "us"]), &verdict, SALE);
    assert!(red_codes(&rows).is_empty(), "{rows:#?}");
    let rows = check_rows(&by_region(rules, &["eu", "us", "apac"]), &verdict, SALE);
    assert_eq!(
        missing_row(&rows).detail,
        "quota_rules: item 00000000-0000-0000-0000-00000000000a at value \"apac\": no rule of \
         \"cf.compute.vcpu\" for the value and no default rule"
    );
}

/// D-547: the cell without a value is sellable only where the revision prices it directly, on the
/// default chain on the sale date (a sale without a value binds that price); a value's own price,
/// or a default price from a later day, does not make it sellable.
#[test]
fn the_cell_without_a_value_counts_only_where_the_default_chain_prices_it() {
    let rules = vec![
        cell_rule(1, Some("eu"), "compute"),
        cell_rule(2, Some("us"), "compute"),
    ];
    let verdict = judged_as(&[(1, VCPU), (2, VCPU)]);
    let mut ctx = by_region(rules, &["eu", "us"]);
    assert!(red_codes(&check_rows(&ctx, &verdict, SALE)).is_empty());
    ctx.entries[0].prices = vec![chain_price(1, Some("eu"), SALE)];
    assert!(
        red_codes(&check_rows(&ctx, &verdict, SALE)).is_empty(),
        "a value's own price"
    );
    ctx.entries[0].prices = vec![chain_price(2, None, SALE.next_day().unwrap())];
    assert!(
        red_codes(&check_rows(&ctx, &verdict, SALE)).is_empty(),
        "a default price from a later day"
    );
    ctx.entries[0].prices = vec![chain_price(3, None, SALE)];
    let rows = check_rows(&ctx, &verdict, SALE);
    assert_eq!(red_codes(&rows), ["QUOTA_RULE_MISSING"]);
    assert_eq!(
        missing_row(&rows).detail,
        "quota_rules: item 00000000-0000-0000-0000-00000000000a without a value: no rule of \
         \"cf.compute.vcpu\" without a value"
    );
}

/// D-547: an item without a dimension sells one cell, without a value, which its rules cover.
#[test]
fn a_dimensionless_item_with_rules_leaves_no_cell_uncovered() {
    let mut ctx = context_with_rules(vec![rule_json(1, ITEM_A, "10")]);
    ctx.entries[0].prices = vec![chain_price(1, None, SALE)];
    let rows = check_rows(&ctx, &judged(&[1]), SALE);
    assert!(red_codes(&rows).is_empty(), "{rows:#?}");
    assert_eq!(missing_row(&rows).detail, "ok");
}

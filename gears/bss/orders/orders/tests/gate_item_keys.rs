#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::{
    answers::{OverlapKey, OverlapKeyLine},
    item_keys::{
        ClassificationProvenance, ItemOverlapKey, distinct_scopes, valid_text, validated_line_keys,
    },
};
use uuid::Uuid;
type Set = fn(&mut OverlapKey, String);
fn request(item: u128) -> OverlapKeyLine {
    OverlapKeyLine {
        line_id: Uuid::from_u128(1),
        plan_revision_id: Uuid::from_u128(2),
        paid_recurring_sku: Uuid::from_u128(3),
        item_id: Uuid::from_u128(item),
        dimension_value: Some("requested-fallback".into()),
        sku_version: 7,
    }
}
fn answer(request: OverlapKeyLine, scope: &str) -> OverlapKey {
    OverlapKey {
        line_id: request.line_id,
        catalog_subscription_product_key: format!("product-{}", request.item_id),
        overlap_scope_key: scope.into(),
        item_key: Some(ItemOverlapKey {
            profile: "item_keys_v1".into(),
            selection: request,
            catalog_subscription_product_key: String::new(),
            overlap_scope_key: scope.into(),
            classification: ClassificationProvenance {
                source: "test-provider".into(),
                reference: "classification".into(),
                version: "7".into(),
            },
        }),
    }
}
fn complete(request: OverlapKeyLine, scope: &str) -> OverlapKey {
    let mut key = answer(request, scope);
    key.item_key
        .as_mut()
        .unwrap()
        .catalog_subscription_product_key
        .clone_from(&key.catalog_subscription_product_key);
    key
}
#[test]
fn two_items_sharing_sku_and_scope_keep_both_product_associations() {
    let expected = vec![request(10), request(11)];
    let answers = vec![
        complete(expected[1].clone(), "scope"),
        complete(expected[0].clone(), "scope"),
    ];
    let valid = validated_line_keys(&expected, &answers);
    let items: Vec<_> = valid[&Uuid::from_u128(1)]
        .iter()
        .map(|k| k.item_key.clone().unwrap())
        .collect();
    assert_eq!(items.len(), 2);
    assert_ne!(
        items[0].catalog_subscription_product_key,
        items[1].catalog_subscription_product_key
    );
    assert_eq!(distinct_scopes(&items).len(), 1);
    assert_eq!(items[0].selection, expected[0]);
}
#[test]
fn missing_duplicate_foreign_or_mismatched_item_evidence_fails_closed() {
    let expected = vec![request(10), request(11)];
    let good = vec![
        complete(expected[0].clone(), "a"),
        complete(expected[1].clone(), "b"),
    ];
    let mut cases = vec![
        vec![good[0].clone()],
        vec![good[0].clone(), good[0].clone()],
    ];
    for change in 0..9 {
        let mut invalid = good.clone();
        let item = invalid[1].item_key.as_mut().unwrap();
        match change {
            0 => item.selection.item_id = Uuid::from_u128(99),
            1 => item.selection.plan_revision_id = Uuid::from_u128(99),
            2 => item.selection.paid_recurring_sku = Uuid::from_u128(99),
            3 => item.selection.sku_version += 1,
            4 => item.selection.dimension_value = None,
            5 => item.classification.version.clear(),
            6 => item.profile = "unknown".into(),
            7 => item.overlap_scope_key = "foreign".into(),
            _ => invalid[1].line_id = Uuid::from_u128(99),
        }
        cases.push(invalid);
    }
    for invalid in cases {
        assert!(
            validated_line_keys(&expected, &invalid).is_empty(),
            "{invalid:?}"
        );
    }
}
#[test]
fn legacy_response_is_only_valid_for_one_selection_and_stays_legacy() {
    let mut legacy = complete(request(10), "scope");
    legacy.item_key = None;
    let valid = validated_line_keys(&[request(10)], &[legacy.clone()]);
    assert!(valid[&Uuid::from_u128(1)][0].item_key.is_none());
    assert!(validated_line_keys(&[request(10), request(11)], &[legacy]).is_empty());
}
#[test]
fn an_inner_product_key_that_differs_from_the_outer_fails() {
    let mut key = complete(request(10), "scope");
    key.item_key
        .as_mut()
        .unwrap()
        .catalog_subscription_product_key = "other".into();
    assert!(validated_line_keys(&[request(10), request(11)], &[key.clone()]).is_empty());
    assert!(validated_line_keys(&[request(10)], &[key]).is_empty());
}
#[test]
fn provider_text_bounds() {
    let max = "a".repeat(1024);
    let over = "a".repeat(1025);
    assert!(valid_text(&max));
    assert!(!valid_text(&over));
    assert!(!valid_text("a\0b"));
    assert!(!valid_text(""));
    // Outer and inner product/scope are set together so only the text bound can refuse them.
    let fields: [(&str, Set); 5] = [
        ("product (outer and inner)", |k, v| {
            k.catalog_subscription_product_key.clone_from(&v);
            k.item_key
                .as_mut()
                .unwrap()
                .catalog_subscription_product_key = v;
        }),
        ("scope (outer and inner)", |k, v| {
            k.overlap_scope_key.clone_from(&v);
            k.item_key.as_mut().unwrap().overlap_scope_key = v;
        }),
        ("classification source", |k, v| {
            k.item_key.as_mut().unwrap().classification.source = v;
        }),
        ("classification reference", |k, v| {
            k.item_key.as_mut().unwrap().classification.reference = v;
        }),
        ("classification version", |k, v| {
            k.item_key.as_mut().unwrap().classification.version = v;
        }),
    ];
    for (name, set) in fields {
        let apply = |value: &str| {
            let mut key = complete(request(10), "scope");
            set(&mut key, value.to_owned());
            validated_line_keys(&[request(10)], &[key])
        };
        assert_eq!(apply(&max).len(), 1, "{name}: 1024 bytes kept");
        assert!(apply(&over).is_empty(), "{name}: 1025 bytes dropped");
        assert!(apply("a\0b").is_empty(), "{name}: NUL dropped");
    }
    // Legacy single-selection answer: only the outer fields are checked.
    for bad in [over.as_str(), "a\0b"] {
        let mut legacy = complete(request(10), "scope");
        legacy.item_key = None;
        legacy.catalog_subscription_product_key = bad.into();
        assert!(validated_line_keys(&[request(10)], &[legacy.clone()]).is_empty());
        legacy.catalog_subscription_product_key = "product".into();
        legacy.overlap_scope_key = bad.into();
        assert!(validated_line_keys(&[request(10)], &[legacy]).is_empty());
    }
    // The dimension value is checked on the request echoed inside the item key.
    let dim = |v: &str| {
        let mut req = request(10);
        req.dimension_value = Some(v.into());
        validated_line_keys(&[req.clone()], &[complete(req, "scope")])
    };
    assert_eq!(dim(&max).len(), 1);
    assert!(dim(&over).is_empty());
    assert!(dim("a\0b").is_empty());
}
#[test]
fn duplicate_item_and_dimension_requests_are_omitted() {
    let expected = vec![request(10), request(10)];
    let answers = vec![
        complete(expected[0].clone(), "scope"),
        complete(expected[1].clone(), "scope"),
    ];
    assert!(validated_line_keys(&expected, &answers).is_empty());
}

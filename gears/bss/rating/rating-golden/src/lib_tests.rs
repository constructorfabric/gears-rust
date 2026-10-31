use proptest::prelude::*;
use serde_json::{Value, json};

use super::{
    ADDED_MEMBER, Breach, assert_additive, assert_census, check_output, non_additive_changes,
    with_added_members,
};

#[test]
fn an_added_member_is_additive() {
    let golden = json!({"a": "1", "nested": {"b": 2}});
    let fresh = json!({"a": "1", "nested": {"b": 2, "c": true}, "d": null});
    assert!(non_additive_changes(&golden, &fresh).is_empty());
}

#[test]
fn a_removed_member_is_a_breach() {
    let breaches = non_additive_changes(&json!({"a": 1, "b": 2}), &json!({"a": 1}));
    assert_eq!(
        breaches,
        vec![Breach {
            pointer: "/b".to_owned(),
            change: "member removed or renamed".to_owned()
        }]
    );
}

#[test]
fn a_retyped_member_is_a_breach() {
    let breaches = non_additive_changes(&json!({"amount": "10"}), &json!({"amount": 10}));
    assert_eq!(breaches.len(), 1);
    assert_eq!(breaches[0].pointer, "/amount");
    assert!(breaches[0].change.contains("retyped from string to number"));
}

#[test]
fn a_changed_canonical_form_is_a_breach() {
    let breaches = non_additive_changes(&json!({"q": "10"}), &json!({"q": "10.0"}));
    assert_eq!(breaches.len(), 1);
    assert!(breaches[0].change.contains("value changed"));
}

#[test]
fn arrays_keep_their_items() {
    let golden = json!({"lines": [{"k": "a"}, {"k": "b"}]});
    assert!(
        non_additive_changes(
            &golden,
            &json!({"lines": [{"k": "a"}, {"k": "b"}, {"k": "c"}]})
        )
        .is_empty()
    );
    let breaches = non_additive_changes(&golden, &json!({"lines": [{"k": "a", "x": 1}]}));
    assert_eq!(breaches.len(), 1);
    assert_eq!(breaches[0].pointer, "/lines");
    let breaches = non_additive_changes(&golden, &json!({"lines": [{"k": "a"}, {"j": "b"}]}));
    assert_eq!(breaches[0].pointer, "/lines/1/k");
}

#[test]
fn pointers_escape_reserved_characters() {
    let breaches = non_additive_changes(&json!({"a/b~c": 1}), &json!({}));
    assert_eq!(breaches[0].pointer, "/a~1b~0c");
    assert_eq!(
        breaches[0].to_string(),
        "/a~1b~0c: member removed or renamed"
    );
}

#[test]
fn every_object_gets_the_added_member() {
    let extended = with_added_members(json!({"a": {"b": [{"c": 1}, 2]}, "d": "x"}));
    assert_eq!(extended[ADDED_MEMBER], "unknown");
    assert_eq!(extended["a"][ADDED_MEMBER], "unknown");
    assert_eq!(extended["a"]["b"][0][ADDED_MEMBER], "unknown");
    assert_eq!(extended["a"]["b"][1], 2);
    assert_eq!(extended["d"], "x");
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictReader {
    a: String,
}

#[test]
fn a_reader_that_refuses_unknown_members_fails_the_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("strict.json"), r#"{"a": "1"}"#).unwrap();
    let sample = StrictReader { a: "1".to_owned() };
    let message = panic_message(|| assert_additive(dir.path(), "strict", &sample));
    assert!(
        message.contains("consumers ignore unknown fields"),
        "{message}"
    );
}

/// The message `check` panics with; fails the test when it does not panic.
fn panic_message(check: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let payload = std::panic::catch_unwind(check).expect_err("the check panics");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

#[test]
fn a_missing_fixture_fails_the_check_and_is_recorded_only_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let sample = json!({"a": "1"});
    // Without the update switch, CI never records a golden it was meant to check against.
    let message = panic_message(|| {
        check_output(dir.path(), "missing", &sample, false);
    });
    assert!(message.contains("is missing"), "{message}");
    assert!(!dir.path().join("missing.json").exists());
    // With it, the fixture is recorded, and checked from then on.
    assert_eq!(check_output(dir.path(), "missing", &sample, true), None);
    let recorded: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("missing.json")).unwrap())
            .unwrap();
    assert_eq!(recorded, sample);
    assert_eq!(
        check_output(dir.path(), "missing", &sample, false),
        Some(sample)
    );
}

#[test]
fn an_update_extends_a_fixture_by_an_addition_and_never_records_a_breach() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view.json");
    std::fs::write(&path, r#"{"a": "1"}"#).unwrap();
    check_output(dir.path(), "view", &json!({"a": "1", "b": 2}), true);
    let extended: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(extended, json!({"a": "1", "b": 2}));
    let message = panic_message(|| {
        check_output(dir.path(), "view", &json!({"b": 2}), true);
    });
    assert!(message.contains("non-additive change"), "{message}");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        kept,
        json!({"a": "1", "b": 2}),
        "a breach is never recorded"
    );
}

#[test]
fn an_unreadable_fixture_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the fixture should be: it exists, but cannot be read as one.
    std::fs::create_dir(dir.path().join("view.json")).unwrap();
    let message = panic_message(|| {
        check_output(dir.path(), "view", &json!({"a": "1"}), true);
    });
    assert!(message.contains("unreadable"), "{message}");
    assert!(dir.path().join("view.json").is_dir());
}

#[test]
fn the_census_refuses_a_fixture_without_a_check_and_a_check_without_a_fixture() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), "{}").unwrap();
    std::fs::write(dir.path().join("b.json"), "{}").unwrap();
    assert_census(dir.path(), &["a", "b"]);
    for names in [&["a"][..], &["a", "b", "c"][..]] {
        let message = panic_message(|| assert_census(dir.path(), names));
        assert!(
            message.contains("differ from the checked contract set"),
            "{names:?}: {message}"
        );
    }
}

/// Any JSON value: every scalar form, nested arrays and objects with arbitrary keys.
fn any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>()
            .prop_filter("JSON has no NaN or infinity", |f| f.is_finite())
            .prop_map(Value::from),
        ".*".prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            proptest::collection::vec((".*", inner), 0..6)
                .prop_map(|members| Value::Object(members.into_iter().collect())),
        ]
    })
}

/// `value` with `extra` appended to every array, at every depth.
fn with_appended_items(value: Value, extra: &Value) -> Value {
    match value {
        Value::Object(members) => Value::Object(
            members
                .into_iter()
                .map(|(key, member)| (key, with_appended_items(member, extra)))
                .collect(),
        ),
        Value::Array(items) => {
            let mut items: Vec<Value> = items
                .into_iter()
                .map(|item| with_appended_items(item, extra))
                .collect();
            items.push(extra.clone());
            Value::Array(items)
        }
        other => other,
    }
}

proptest! {
    #[test]
    fn a_document_is_additive_over_itself(value in any_json()) {
        prop_assert!(non_additive_changes(&value, &value).is_empty());
    }

    #[test]
    fn members_added_anywhere_stay_additive(value in any_json()) {
        prop_assert!(non_additive_changes(&value, &with_added_members(value.clone())).is_empty());
    }

    #[test]
    fn items_appended_to_any_array_stay_additive(value in any_json(), extra in any_json()) {
        prop_assert!(
            non_additive_changes(&value, &with_appended_items(value.clone(), &extra)).is_empty()
        );
    }

    #[test]
    fn taking_the_added_members_away_again_is_a_breach(value in any_json()) {
        // Every object gained a member, so every object loses one: a breach wherever an object is.
        let breaches = non_additive_changes(&with_added_members(value.clone()), &value);
        prop_assert_eq!(breaches.is_empty(), !has_object(&value));
    }
}

fn has_object(value: &Value) -> bool {
    match value {
        Value::Object(_) => true,
        Value::Array(items) => items.iter().any(has_object),
        _ => false,
    }
}

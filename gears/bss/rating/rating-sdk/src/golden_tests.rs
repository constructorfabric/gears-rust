use serde_json::json;

use super::*;

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
    let dir = std::env::temp_dir().join(format!("rating-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("strict.json"), r#"{"a": "1"}"#).unwrap();
    let sample = StrictReader { a: "1".to_owned() };
    let outcome = std::panic::catch_unwind(|| assert_additive(&dir, "strict", &sample));
    std::fs::remove_dir_all(&dir).unwrap();
    let message = outcome.unwrap_err();
    let message = message
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("consumers ignore unknown fields"),
        "{message}"
    );
}

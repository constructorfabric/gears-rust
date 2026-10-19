//! The additive-only check over golden JSON fixtures (DESIGN §3.3 "Compatibility of Rating-owned
//! contracts", T-D-71).
//!
//! A golden fixture freezes the JSON a V1 DTO, REST response or canonical form produced for a fixed
//! sample. CI serializes the same sample with the current code and compares:
//!
//! - every member of the golden must still be produced, with the same JSON type and, for scalars,
//!   the same value: a removed, renamed or retyped field, or a changed canonical form, is a breach;
//! - a member the golden does not have is an addition and passes;
//! - an array keeps at least the golden's elements, each compared the same way.
//!
//! The other half of the rule is the reader's: a payload an old producer wrote still deserializes,
//! so no new field is required, and so does a payload a newer V1 producer wrote, so a consumer
//! ignores a member it does not know. [`assert_additive`] checks both by reading the golden, and the
//! golden with an extra member in every object, into the current type.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Set to `1` to record a missing fixture or to extend one by an additive change. A breach is
/// never recorded: a non-additive change is a new major version with fixtures of its own.
pub const UPDATE_ENV: &str = "UPDATE_RATING_GOLDEN";

/// One non-additive difference between a golden fixture and the current serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    /// JSON pointer of the member that changed (`""` is the document root).
    pub pointer: String,
    /// What changed.
    pub change: String,
}

impl std::fmt::Display for Breach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let at = if self.pointer.is_empty() {
            "/"
        } else {
            self.pointer.as_str()
        };
        write!(f, "{at}: {}", self.change)
    }
}

/// Every non-additive change from `golden` to `fresh`; empty when `fresh` only adds.
#[must_use]
pub fn non_additive_changes(golden: &Value, fresh: &Value) -> Vec<Breach> {
    let mut breaches = Vec::new();
    compare(golden, fresh, &mut String::new(), &mut breaches);
    breaches
}

/// The member [`assert_additive`] adds to every object to stand for a later V1 addition.
pub const ADDED_MEMBER: &str = "added_by_a_later_v1_release";

/// Checks `sample` against the golden fixture `<dir>/<name>.json`: the current serialization may
/// only add to it, and the fixture must still deserialize into `T`, also with an unknown member
/// added to every object.
///
/// # Panics
///
/// When the fixture is missing (unless [`UPDATE_ENV`] is set, which records it), unreadable, has a
/// non-additive change, or no longer deserializes. This is a test assertion.
pub fn assert_additive<T: Serialize + DeserializeOwned>(dir: &Path, name: &str, sample: &T) {
    let Some(golden) = assert_additive_output(dir, name, sample) else {
        return;
    };
    let extended = with_added_members(golden.clone());
    if let Err(e) = serde_json::from_value::<T>(golden) {
        panic!("{name}: a payload written to the golden no longer deserializes: {e}");
    }
    if let Err(e) = serde_json::from_value::<T>(extended) {
        panic!(
            "{name}: a payload with a member a later V1 release adds is refused \
             (consumers ignore unknown fields): {e}"
        );
    }
}

/// `value` with [`ADDED_MEMBER`] in every object, at every depth.
#[must_use]
pub fn with_added_members(value: Value) -> Value {
    match value {
        Value::Object(members) => {
            let mut members: serde_json::Map<String, Value> = members
                .into_iter()
                .map(|(key, member)| (key, with_added_members(member)))
                .collect();
            members.insert(ADDED_MEMBER.to_owned(), Value::from("unknown"));
            Value::Object(members)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(with_added_members).collect()),
        other => other,
    }
}

/// [`assert_additive`] for a type Rating only produces (a REST response): the current
/// serialization may only add to the fixture. Returns the fixture as it was before this call, or
/// `None` when it was just recorded.
///
/// # Panics
///
/// When the fixture is missing (unless [`UPDATE_ENV`] is set), unreadable, or has a non-additive
/// change. This is a test assertion.
#[allow(clippy::expect_used)]
pub fn assert_additive_output<T: Serialize>(dir: &Path, name: &str, sample: &T) -> Option<Value> {
    let path = dir.join(format!("{name}.json"));
    let fresh = serde_json::to_value(sample).expect("the sample serializes");
    let update = std::env::var(UPDATE_ENV).is_ok_and(|v| v == "1");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // Only a missing fixture is recorded; an unreadable one is never overwritten.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                update,
                "golden fixture {} is missing; record it with {UPDATE_ENV}=1",
                path.display()
            );
            write_fixture(&path, &fresh);
            return None;
        }
        Err(e) => panic!("golden fixture {} is unreadable: {e}", path.display()),
    };
    let golden: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("golden fixture {} is not JSON: {e}", path.display()));
    let breaches = non_additive_changes(&golden, &fresh);
    assert!(
        breaches.is_empty(),
        "{name}: non-additive change to a V1 contract (a breaking change is a new major version):\n{}",
        breaches
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    if update && golden != fresh {
        write_fixture(&path, &fresh);
    }
    Some(golden)
}

/// [`assert_additive`] for a type Rating only accepts (a REST request): a request an old client
/// sends, frozen in `<dir>/<name>.json`, must still be accepted, so no new field is required and
/// no field was retyped. Request fixtures are written by hand.
///
/// # Panics
///
/// When the fixture is missing or unreadable, or no longer deserializes. This is a test
/// assertion.
pub fn assert_still_accepted<T: DeserializeOwned>(dir: &Path, name: &str) {
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden fixture {} is unreadable: {e}", path.display()));
    if let Err(e) = serde_json::from_str::<T>(&text) {
        panic!("{name}: a request an old client sends is no longer accepted: {e}");
    }
}

/// Checks that `dir` holds exactly the fixtures `names` (each as `<name>.json`), so a fixture
/// cannot outlive its contract test or be added without one.
///
/// # Panics
///
/// When the directory is unreadable or its fixture set differs from `names`.
#[allow(clippy::expect_used)]
pub fn assert_census(dir: &Path, names: &[&str]) {
    let on_disk: BTreeSet<String> = std::fs::read_dir(dir)
        .expect("the golden directory is readable")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .map(str::to_owned)
        })
        .collect();
    let expected: BTreeSet<String> = names.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(
        on_disk,
        expected,
        "the golden fixtures in {} differ from the checked contract set",
        dir.display()
    );
}

#[allow(clippy::expect_used)]
fn write_fixture(path: &Path, value: &Value) {
    let mut text = serde_json::to_string_pretty(value).expect("the sample serializes");
    text.push('\n');
    std::fs::write(path, text).expect("the golden fixture is writable");
}

fn compare(golden: &Value, fresh: &Value, pointer: &mut String, out: &mut Vec<Breach>) {
    match (golden, fresh) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, old_value) in old {
                let len = pointer.len();
                pointer.push('/');
                pointer.push_str(&escape(key));
                match new.get(key) {
                    Some(new_value) => compare(old_value, new_value, pointer, out),
                    None => out.push(Breach {
                        pointer: pointer.clone(),
                        change: "member removed or renamed".to_owned(),
                    }),
                }
                pointer.truncate(len);
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            if new.len() < old.len() {
                out.push(Breach {
                    pointer: pointer.clone(),
                    change: format!("array shrank from {} to {} items", old.len(), new.len()),
                });
            }
            for (index, (old_item, new_item)) in old.iter().zip(new).enumerate() {
                let len = pointer.len();
                pointer.push('/');
                pointer.push_str(&index.to_string());
                compare(old_item, new_item, pointer, out);
                pointer.truncate(len);
            }
        }
        (old, new) if kind(old) != kind(new) => out.push(Breach {
            pointer: pointer.clone(),
            change: format!("retyped from {} to {}", kind(old), kind(new)),
        }),
        (old, new) if old != new => out.push(Breach {
            pointer: pointer.clone(),
            change: format!("value changed from {old} to {new}"),
        }),
        _ => {}
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// RFC 6901 escaping of one reference token.
fn escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
#[path = "golden_tests.rs"]
mod tests;

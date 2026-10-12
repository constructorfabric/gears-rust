//! Helpers shared by the crate's tests.
#![allow(clippy::expect_used)]

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Every variant of a unit-variant enum with documented variants, read from
/// its generated schema (one `oneOf` branch per `const`) and deserialized
/// back, so the list is complete by construction.
///
/// Panics rather than returning nothing when the schema has another shape
/// (schemars emits a plain `enum` for undocumented variants), so a loop over
/// the result can never silently test nothing.
pub fn variants<T: JsonSchema + DeserializeOwned>() -> Vec<T> {
    let schema = schemars::schema_for!(T);
    let variants: Vec<T> = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|branch| serde_json::from_value(branch["const"].clone()).expect("a variant"))
        .collect();
    assert!(!variants.is_empty(), "no `oneOf` constants in {schema:?}");
    variants
}

//! Field projection support for `$select` `OData` queries.
//!
//! This module provides utilities for projecting DTOs based on selected fields.
//! It allows handlers to filter response objects to only include requested fields.

use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// Project a JSON value to only include selected fields.
///
/// Supports dot notation for nested field selection (e.g., `access_control.read`).
/// For objects, recursively includes only the specified fields.
/// For arrays, projects each element.
/// For other types, returns the value unchanged.
///
/// # Arguments
///
/// * `value` - The JSON value to project
/// * `selected_fields` - Set of field names to include (case-insensitive, supports dot notation)
///
/// # Returns
///
/// A new JSON value containing only the selected fields
///
/// # Examples
///
/// ```ignore
/// // Select top-level field
/// $select=id,name
///
/// // Select nested field (includes entire nested object)
/// $select=access_control
///
/// // Select specific nested field
/// $select=access_control.read,access_control.write
/// ```
#[allow(clippy::implicit_hasher)] // we don't care for now about the hasher of the hashset
#[must_use]
pub fn project_json(value: &Value, selected_fields: &HashSet<String>) -> Value {
    match value {
        Value::Object(map) => {
            let mut projected = Map::new();
            for (key, val) in map {
                let key_lower = key.to_lowercase();

                // Check if this exact field is selected
                if selected_fields.contains(&key_lower) {
                    // Include entire field (no further filtering)
                    projected.insert(key.clone(), val.clone());
                } else {
                    // Check if any nested fields are selected (dot notation)
                    let nested_fields = extract_nested_fields(&key_lower, selected_fields);
                    if !nested_fields.is_empty() {
                        // Recursively project nested fields
                        projected.insert(key.clone(), project_json(val, &nested_fields));
                    }
                }
            }
            Value::Object(projected)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| project_json(v, selected_fields))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Extract nested field selectors for a given parent field.
///
/// For example, if `selected_fields` contains `access_control.read` and `access_control.write`,
/// this function returns a set containing `read` and `write` when called with `access_control`.
fn extract_nested_fields(parent_key: &str, selected_fields: &HashSet<String>) -> HashSet<String> {
    let prefix = format!("{parent_key}.");
    selected_fields
        .iter()
        .filter(|field| field.starts_with(&prefix))
        .map(|field| field[prefix.len()..].to_string())
        .collect()
}

/// Helper function to apply field projection to a serializable value.
///
/// # Arguments
///
/// * `value` - The value to project
/// * `selected_fields` - Optional slice of field names to include
///
/// # Returns
///
/// The projected JSON value, or the original value if no fields are selected
pub fn apply_select<T: serde::Serialize>(value: T, selected_fields: Option<&[String]>) -> Value {
    match selected_fields {
        Some(fields) if !fields.is_empty() => {
            let fields_set: HashSet<String> = fields.iter().map(|f| f.to_lowercase()).collect();
            match serde_json::to_value(value) {
                Ok(v) => project_json(&v, &fields_set),
                Err(e) => {
                    tracing::warn!(error = %e, "DTO serialization failed in apply_select; returning empty object");
                    json!({})
                }
            }
        }
        _ => serde_json::to_value(value).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "DTO serialization failed in apply_select; returning empty object");
            json!({})
        }),
    }
}

/// Convert a page of items to a page of projected JSON values.
///
/// This is a convenience function that combines serialization and projection
/// for paginated responses. It automatically applies `$select` projection if specified.
///
/// # Arguments
///
/// * `page` - The page containing items to project
/// * `selected_fields` - Optional slice of field names to include
///
/// # Returns
///
/// A `modkit_odata::Page<Value>` with projected items
#[must_use]
pub fn page_to_projected_json<T: serde::Serialize>(
    page: &modkit_odata::Page<T>,
    selected_fields: Option<&[String]>,
) -> modkit_odata::Page<Value> {
    let projected_items: Vec<Value> = page
        .items
        .iter()
        .map(|item| apply_select(item, selected_fields))
        .collect();

    modkit_odata::Page {
        items: projected_items,
        page_info: page.page_info.clone(),
    }
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;

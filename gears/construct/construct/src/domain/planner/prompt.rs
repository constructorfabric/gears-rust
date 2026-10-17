use construct_sdk::gts::RECORD_TYPES;
use construct_sdk::person_types::Cardinality;
use serde_json::Value;
use toolkit_macros::domain_model;

use super::catalog::PropertyCatalog;
use crate::domain::profile::Category;
use crate::domain::record_intake::ReceivedRecord;

const INSTRUCTIONS: &str = include_str!("instructions.md");
const PROPERTIES_PLACEHOLDER: &str = "{properties}";
const NOT_BLANK: &str = "\\S";

/// Why a record cannot be shown to the model.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusableRecord {
    /// No record type Construct takes has the record's type id.
    UnknownType,
    /// The record type's schema does not parse or has no payload schema.
    NoPayloadSchema,
    /// The record has no payload.
    NoPayload,
}

impl UnusableRecord {
    /// The name the planner logs.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::UnknownType => "unknown_record_type",
            Self::NoPayloadSchema => "no_payload_schema",
            Self::NoPayload => "no_payload",
        }
    }
}

/// The planner's instructions, with every property of the person types and its value form.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-prompt:p1
#[must_use]
pub fn instructions(catalog: &PropertyCatalog) -> String {
    let properties = Category::ALL
        .iter()
        .flat_map(|category| {
            catalog.names().filter_map(move |property| {
                let spec = catalog.get(property)?;
                (spec.category == *category).then(|| {
                    let cardinality = match spec.cardinality {
                        Cardinality::One => "one",
                        Cardinality::Many => "many",
                    };
                    format!(
                        "{}: {property} ({cardinality}): {}",
                        category.name(),
                        form(spec.schema())
                    )
                })
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    INSTRUCTIONS.replace(PROPERTIES_PLACEHOLDER, &properties)
}

/// The record and the numbered profile as the model gets them, marked as data, with no field named `id` or `*_id`.
/// The data holds no `<` or `>`, so it cannot close its block or open another.
///
/// # Errors
///
/// [`UnusableRecord`] when the record's type or payload cannot be shown.
pub fn record_input(
    record: &ReceivedRecord,
    numbered_profile: &str,
) -> Result<String, UnusableRecord> {
    let record_type = RECORD_TYPES
        .iter()
        .find(|record_type| record_type.type_id == record.envelope.type_id)
        .ok_or(UnusableRecord::UnknownType)?;
    let schema: Value =
        serde_json::from_str(record_type.schema).map_err(|_| UnusableRecord::NoPayloadSchema)?;
    let payload_schema = schema
        .pointer("/allOf/1/properties/payload")
        .ok_or(UnusableRecord::NoPayloadSchema)?;
    let payload = record
        .record
        .get("payload")
        .ok_or(UnusableRecord::NoPayload)?;
    let kind = schema
        .get("description")
        .and_then(Value::as_str)
        .and_then(|description| description.split(". ").next())
        .unwrap_or("A record");
    let profile = if numbered_profile.is_empty() {
        "(empty)"
    } else {
        numbered_profile
    };
    Ok(format!(
        "<record>\nType: {kind}.\nPayload schema: {}\nPayload: {}\n</record>\n\n<profile>\n{}\n</profile>",
        as_data(&schema_without_ids(payload_schema).to_string()),
        as_data(&without_ids(payload).to_string()),
        as_data(profile)
    ))
}

/// `text` with each `<` and `>` written as its JSON escape. Every one of them is inside a JSON string, where the escape
/// means the same character.
fn as_data(text: &str) -> String {
    text.replace('<', "\\u003c").replace('>', "\\u003e")
}

fn is_id(name: &str) -> bool {
    name == "id" || name.ends_with("_id")
}

fn without_ids(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(name, _)| !is_id(name))
                .map(|(name, field)| (name.clone(), without_ids(field)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_ids).collect()),
        other => other.clone(),
    }
}

fn schema_without_ids(schema: &Value) -> Value {
    let Value::Object(keywords) = schema else {
        return schema.clone();
    };
    Value::Object(
        keywords
            .iter()
            .map(|(keyword, value)| {
                let value = match (keyword.as_str(), value) {
                    ("properties", Value::Object(properties)) => Value::Object(
                        properties
                            .iter()
                            .filter(|(name, _)| !is_id(name))
                            .map(|(name, property)| (name.clone(), schema_without_ids(property)))
                            .collect(),
                    ),
                    ("required", Value::Array(names)) => Value::Array(
                        names
                            .iter()
                            .filter(|name| name.as_str().is_none_or(|name| !is_id(name)))
                            .cloned()
                            .collect(),
                    ),
                    (_, value) => schema_without_ids(value),
                };
                (keyword.clone(), value)
            })
            .collect(),
    )
}

fn form(schema: &Value) -> String {
    let base = if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | ")
    } else {
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => object_form(schema),
            Some("integer") => "integer".to_owned(),
            Some("number") => "number".to_owned(),
            Some("boolean") => "true or false".to_owned(),
            _ if schema.get("format").and_then(Value::as_str) == Some("uri") => "URL".to_owned(),
            _ => match schema.get("pattern").and_then(Value::as_str) {
                Some(pattern) if pattern != NOT_BLANK && schema.get("description").is_none() => {
                    format!("text matching {pattern}")
                }
                _ => "text".to_owned(),
            },
        }
    };
    match schema.get("description").and_then(Value::as_str) {
        Some(description) => format!("{base} ({description})"),
        None => base,
    }
}

fn object_form(schema: &Value) -> String {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            let mut fields: Vec<(&String, &Value)> = properties.iter().collect();
            fields.sort_by_key(|(name, _)| *name);
            fields
                .into_iter()
                .map(|(name, field)| {
                    let marker = if required.contains(&name.as_str()) {
                        " (required)"
                    } else {
                        ""
                    };
                    format!("{name}{marker}: {}", form(field))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!("{{{fields}}}")
}

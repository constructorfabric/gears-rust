use std::collections::BTreeMap;

use construct_sdk::person_types::{Cardinality, PERSON_TYPES, PROPERTIES, property_schema};
use jsonschema::{Draft, Validator};
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::profile::Category;

/// One property of the person types: its category, its cardinality and its schema.
#[domain_model]
#[derive(Debug)]
pub struct PropertySpec {
    pub category: Category,
    pub cardinality: Cardinality,
    schema: Value,
    validator: Validator,
}

impl PropertySpec {
    /// The property's JSON schema in its person type.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// Checks a value against the property's schema.
    ///
    /// # Errors
    ///
    /// The first rule the value breaks and where, followed by the schema the property takes. The reason does not
    /// repeat the value.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let Some(error) = self.validator.iter_errors(value).next() else {
            return Ok(());
        };
        let rule = error
            .schema_path()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let place = match error.instance_path().as_str() {
            "" => "the value".to_owned(),
            path => format!("`{path}`"),
        };
        Err(format!("{place} breaks `{rule}`; it takes {}", self.schema))
    }
}

/// Every property of the person types, by name.
#[domain_model]
#[derive(Debug)]
pub struct PropertyCatalog {
    properties: BTreeMap<&'static str, PropertySpec>,
}

impl PropertyCatalog {
    /// Reads every property from the person types the SDK ships.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when a person type does not parse or a property has no schema.
    pub fn load() -> Result<Self, DomainError> {
        let mut schemas = BTreeMap::new();
        for person_type in PERSON_TYPES {
            let schema: Value = serde_json::from_str(person_type.schema).map_err(|error| {
                DomainError::internal(format!(
                    "person type {} does not parse: {error}",
                    person_type.type_id
                ))
            })?;
            schemas.insert(person_type.type_id, schema);
        }
        let mut properties = BTreeMap::new();
        for property in PROPERTIES {
            let category = Category::from_type_id(property.type_id).ok_or_else(|| {
                DomainError::internal(format!("{} is not a person type", property.type_id))
            })?;
            let schema = schemas
                .get(property.type_id)
                .and_then(|schema| property_schema(schema, property.name))
                .ok_or_else(|| {
                    DomainError::internal(format!(
                        "{} has no schema for {}",
                        property.type_id, property.name
                    ))
                })?;
            let validator = jsonschema::options()
                .with_draft(Draft::Draft7)
                .build(&schema)
                .map_err(|error| {
                    DomainError::internal(format!(
                        "the schema of {} does not compile: {error}",
                        property.name
                    ))
                })?;
            properties.insert(
                property.name,
                PropertySpec {
                    category,
                    cardinality: property.cardinality,
                    schema,
                    validator,
                },
            );
        }
        Ok(Self { properties })
    }

    /// The property named `property`, or `None` when the person types have no such property.
    #[must_use]
    pub fn get(&self, property: &str) -> Option<&PropertySpec> {
        self.properties.get(property)
    }

    /// The names of all properties, in alphabetical order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.properties.keys().copied()
    }
}

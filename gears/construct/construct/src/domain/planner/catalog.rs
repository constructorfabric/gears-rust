use std::collections::BTreeMap;

use construct_sdk::linked_schema::{LinkKind, NodeKind, Schema};
use jsonschema::{Draft, Validator};
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::profile::{Category, profile_schema};

/// A JSON schema with its compiled validator.
#[domain_model]
#[derive(Debug)]
struct Checked {
    schema: Value,
    validator: Validator,
}

impl Checked {
    fn compile(name: &str, schema: Value) -> Result<Self, DomainError> {
        let validator = jsonschema::options()
            .with_draft(Draft::Draft7)
            .build(&schema)
            .map_err(|error| {
                DomainError::internal(format!("the schema of {name} does not compile: {error}"))
            })?;
        Ok(Self { schema, validator })
    }

    fn check(&self, value: &Value) -> Result<(), String> {
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

/// One node kind: its category and the schema of the object of fields a node of it holds.
#[domain_model]
#[derive(Debug)]
pub struct KindSpec {
    pub category: Category,
    checked: Checked,
}

impl KindSpec {
    /// The kind in the schema.
    #[must_use]
    pub fn kind(&self) -> &'static NodeKind {
        self.category.kind()
    }

    /// The JSON schema of the kind's fields.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.checked.schema
    }

    /// Checks the fields of a node against the kind's schema.
    ///
    /// # Errors
    ///
    /// The first rule the value breaks and where, followed by the schema the kind takes. The reason does not repeat
    /// the value.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        self.checked.check(value)
    }
}

/// One link kind and the schema of its fields.
#[domain_model]
#[derive(Debug)]
pub struct LinkSpec {
    kind: &'static LinkKind,
    checked: Checked,
}

impl LinkSpec {
    /// The link kind in the schema.
    #[must_use]
    pub fn kind(&self) -> &'static LinkKind {
        self.kind
    }

    /// The JSON schema of the link's fields.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.checked.schema
    }

    /// Checks the fields of a link against the link kind's schema.
    ///
    /// # Errors
    ///
    /// As [`KindSpec::check`].
    pub fn check(&self, value: &Value) -> Result<(), String> {
        self.checked.check(value)
    }
}

/// Every node kind and link kind of the profile schema, by name, with its schema compiled.
#[domain_model]
#[derive(Debug)]
pub struct Catalog {
    kinds: BTreeMap<&'static str, KindSpec>,
    links: BTreeMap<&'static str, LinkSpec>,
}

impl Catalog {
    /// Reads every kind and link kind from the profile schema.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when a schema does not compile.
    pub fn load() -> Result<Self, DomainError> {
        let schema = profile_schema();
        let mut kinds = BTreeMap::new();
        for kind in &schema.nodes {
            let category = Category::from_name(kind.name).ok_or_else(|| {
                DomainError::internal(format!("{} is not a kind of the schema", kind.name))
            })?;
            let checked = Checked::compile(kind.name, Schema::fields_schema(kind))?;
            kinds.insert(kind.name, KindSpec { category, checked });
        }
        let mut links = BTreeMap::new();
        for kind in &schema.links {
            let checked = Checked::compile(kind.name, Schema::link_fields_schema(kind))?;
            links.insert(kind.name, LinkSpec { kind, checked });
        }
        Ok(Self { kinds, links })
    }

    /// The node kind named `name`, or `None` when the schema has no such kind.
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<&KindSpec> {
        self.kinds.get(name)
    }

    /// The link kind named `name`, or `None` when the schema has no such link kind.
    #[must_use]
    pub fn link(&self, name: &str) -> Option<&LinkSpec> {
        self.links.get(name)
    }

    /// The names of all node kinds, in alphabetical order.
    pub fn kind_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.kinds.keys().copied()
    }

    /// The names of all link kinds, in alphabetical order.
    pub fn link_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.links.keys().copied()
    }
}

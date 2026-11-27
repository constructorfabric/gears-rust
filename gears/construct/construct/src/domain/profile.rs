use std::fmt;
use std::sync::OnceLock;

use construct_sdk::linked_schema::{NodeKind, Schema, SchemaPrefix, default_schema};
use serde_json::Value;
use toolkit_macros::domain_model;

use super::plan::NodeKey;
use super::subject_settings::SubjectKey;

/// The schema every profile has: the linked default schema.
#[must_use]
pub fn profile_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(default_schema)
}

/// A category of a person's profile: one node kind of the profile's schema, such as a project or a commitment. The
/// name of the kind is its registered name, which the model, the REST API and the permission policy use.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Category(&'static str);

impl Category {
    /// Every category of the schema, in the order the planner lists them.
    #[must_use]
    pub fn all() -> Vec<Self> {
        profile_schema()
            .nodes
            .iter()
            .map(|kind| Self(kind.name))
            .collect()
    }

    /// The category whose graph node type is `type_id`, in the types of any person, or `None` for any other type.
    #[must_use]
    pub fn from_type_id(type_id: &str) -> Option<Self> {
        Self::from_name(Schema::node_kind_name(type_id)?)
    }

    /// The category whose plain name, the entity kind a plan step carries, is `name`, or `None` for any other
    /// name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        profile_schema()
            .nodes
            .iter()
            .find(|kind| kind.name == name)
            .map(|kind| Self(kind.name))
    }

    /// The GTS id of the category's graph node type for the person with `prefix`.
    #[must_use]
    pub fn type_id(self, prefix: &SchemaPrefix) -> String {
        Schema::node_type_id(prefix, self.0)
    }

    /// The plain name the model sees.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0
    }

    /// The category's node kind in the schema.
    ///
    /// # Panics
    ///
    /// Never: a category is made from a kind of the schema.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a category is only ever made from a kind of the schema"
    )]
    pub fn kind(self) -> &'static NodeKind {
        profile_schema()
            .node(self.0)
            .expect("a category is a kind of the schema")
    }
}

/// Categories the tests of the pipeline use, where it does not matter which kind a fact is.
#[cfg(test)]
impl Category {
    pub const ME: Self = Self("me");
    pub const PERSON: Self = Self("person");
    pub const PROJECT: Self = Self("project");
    pub const PRODUCT: Self = Self("product");
    pub const TEAM: Self = Self("team");
    pub const COMMITMENT: Self = Self("commitment");
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// One stored node: its node key, its category, its one property, whose name is the category's and whose value is the
/// object of the node's fields, and the node that owns it, when an entry kind owns it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub node_key: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Value,
    pub owner: Option<NodeKey>,
}

/// A node the person forgot, and the message it came from: the same message does not create it again.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub kind: String,
    /// What names the node in its kind, as [`Mark::identity`] gives it.
    pub key: String,
    /// The tool the node was learned in, and the message it was learned from.
    pub tool: String,
    pub message: String,
}

impl Mark {
    /// What names a node of `category` with `value`, compared without case and spacing, or none when the value
    /// does not hold the property that names the kind.
    #[must_use]
    pub fn identity(category: Category, value: &Value) -> Option<String> {
        let named = value.get(category.kind().key)?;
        Some(match named {
            Value::String(text) => text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
            other => other.to_string(),
        })
    }
}

/// One stored link: its kind, the nodes at its ends and its fields.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub kind: String,
    pub from: NodeKey,
    pub to: NodeKey,
    pub fields: Value,
}

/// A subject's profile as the planner reads it: the version of its root node, none for a subject without a
/// profile yet, its facts and the links between them.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub version: Option<u64>,
    pub facts: Vec<Fact>,
    pub links: Vec<Link>,
    /// What the person forgot, with the message it came from.
    pub marks: Vec<Mark>,
}

/// A profile that holds, or would hold, more facts than it may: more than one
/// read takes.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullProfile {
    pub subject: SubjectKey,
    pub facts: FactCount,
    /// The most facts it may hold.
    pub limit: usize,
}

/// How many facts a full profile has.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactCount {
    /// What a write would leave it holding.
    WouldHold(usize),
    /// The least it holds, after a read cut short by its limit.
    AtLeast(usize),
}

impl fmt::Display for FactCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WouldHold(facts) => write!(f, "would hold {facts} facts"),
            Self::AtLeast(facts) => write!(f, "holds at least {facts} facts"),
        }
    }
}

impl fmt::Display for FullProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the profile of subject {} in tenant {} {}, past the limit of {}",
            self.subject.subject_id, self.subject.tenant_id, self.facts, self.limit
        )
    }
}

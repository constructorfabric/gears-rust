use construct_sdk::linked_schema::{NodeKind, Schema, SchemaPrefix};
use serde_json::Value;
use std::fmt;
use toolkit_macros::domain_model;

use super::plan::NodeKey;
use super::subject_settings::SubjectKey;

/// A category of a person's profile: one node kind of the profile's schema, such as a project or a commitment. The
/// name of the kind is its registered name, which the model, the REST API and the permission policy use.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Category(&'static str);

impl Category {
    /// Every category of `schema`, in the order the planner lists them.
    #[must_use]
    pub fn all(schema: &Schema) -> Vec<Self> {
        schema.nodes.iter().map(|kind| Self(kind.name)).collect()
    }

    /// The category a stored record names, such as a review request for a fact: whichever kind the person's schema
    /// holds now. A request for a kind removed since stays readable under its name.
    #[must_use]
    pub fn of_stored(name: &str) -> Self {
        Self(construct_sdk::schema_doc::intern(name))
    }

    /// The category whose graph node type is `type_id`, in the types of any person, or `None` for any other type
    /// or a kind `schema` does not have.
    #[must_use]
    pub fn from_type_id(schema: &Schema, type_id: &str) -> Option<Self> {
        Self::from_name(schema, Schema::node_kind_name(type_id)?)
    }

    /// The category of `schema` whose plain name, the entity kind a plan step carries, is `name`, or `None` for
    /// any other name.
    #[must_use]
    pub fn from_name(schema: &Schema, name: &str) -> Option<Self> {
        schema
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

    /// The category's node kind in `schema`, or `None` when `schema` has no such kind.
    #[must_use]
    pub fn kind(self, schema: &Schema) -> Option<&NodeKind> {
        schema.node(self.0)
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
    /// What names a node of a kind whose naming property is `key`, with `value`, compared without case and spacing,
    /// or none when the value does not hold that property.
    #[must_use]
    pub fn identity(key: &str, value: &Value) -> Option<String> {
        let named = value.get(key)?;
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

/// Each owned node's key with its owner's, for [`owned_by`], from nodes given as a key and the owner's key. Copies keys
/// only.
#[must_use]
pub fn owners<'a>(
    nodes: impl IntoIterator<Item = (&'a NodeKey, Option<&'a NodeKey>)>,
) -> Vec<(NodeKey, NodeKey)> {
    nodes
        .into_iter()
        .filter_map(|(node, owner)| Some((node.clone(), owner?.clone())))
        .collect()
}

/// The ownership of `facts`, for [`owned_by`].
#[must_use]
pub fn ownership(facts: &[Fact]) -> Vec<(NodeKey, NodeKey)> {
    owners(
        facts
            .iter()
            .map(|fact| (&fact.node_key, fact.owner.as_ref())),
    )
}

/// The keys of the nodes `node` owns, however deep, in `ownership` (from [`ownership`]). The planner's walk of "a
/// node and what it owns"; the writer walks the stored nodes' payloads the same way (`with_owned`).
#[must_use]
pub fn owned_by(ownership: &[(NodeKey, NodeKey)], node: &NodeKey) -> Vec<NodeKey> {
    let mut found: Vec<NodeKey> = Vec::new();
    let mut parents = vec![node.clone()];
    while let Some(parent) = parents.pop() {
        for (owned, _) in ownership.iter().filter(|(_, owner)| *owner == parent) {
            if owned != node && !found.contains(owned) {
                found.push(owned.clone());
                parents.push(owned.clone());
            }
        }
    }
    found
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

/// The default schema, for the tests whose person has not changed it.
#[cfg(test)]
#[must_use]
pub fn profile_schema() -> &'static Schema {
    static SCHEMA: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(construct_sdk::linked_schema::default_schema)
}

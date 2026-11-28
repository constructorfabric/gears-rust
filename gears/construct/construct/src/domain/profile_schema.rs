//! A person's profile schema at run time: the port the rest of the gear reads it through, the changes the host may
//! make to it and the rules a changed schema must keep.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::linked_schema::{LEARNED, ORIGIN, OWNER, RECORD, Schema, default_schema};
use construct_sdk::schema_doc::{LinkKindDoc, NodeKindDoc, OwnerDoc, PropertyDoc, SchemaDoc};
use jsonschema::Draft;
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::subject_settings::SubjectKey;

/// The most node kinds, link kinds and properties or fields of one kind a schema may hold, and the longest name and
/// description. They keep a schema, the types it registers and the names the gear keeps small.
pub const MAX_NODE_KINDS: usize = 64;
pub const MAX_LINK_KINDS: usize = 64;
pub const MAX_PROPERTIES: usize = 32;
pub const MAX_NAME: usize = 40;
pub const MAX_DESCRIPTION: usize = 500;
/// The most bytes the stored document of one schema takes, as JSON. The column is `TEXT`, which MySQL caps at 64 KB.
pub const MAX_DOCUMENT_BYTES: usize = 60_000;
/// The most names one person may make the process keep, over the life of their schema, removals not refunded. Every
/// name is kept in memory for the life of the process, once; a name is charged to the person whose change first
/// brings it in, before it is kept. A person's schema holds at most 4 224 names at a time.
pub const MAX_NAMES_CHARGED: i64 = 8_192;

/// Names a node's payload already uses beside the one property named like its kind.
const RESERVED: [&str; 4] = [ORIGIN, RECORD, LEARNED, OWNER];

/// Where the gear reads the schema of one person from.
#[async_trait]
pub trait ProfileSchemas: Send + Sync {
    /// The schema of the person: the one the host last set, or the default schema of a person whose host set none.
    ///
    /// # Errors
    ///
    /// When the stored schema cannot be read.
    async fn schema(&self, key: SubjectKey) -> Result<Arc<Schema>, DomainError>;
}

/// Every person has the default schema, and nothing changes it.
#[domain_model]
#[derive(Debug, Clone)]
pub struct DefaultSchemas(Arc<Schema>);

impl DefaultSchemas {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(default_schema()))
    }
}

impl Default for DefaultSchemas {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProfileSchemas for DefaultSchemas {
    async fn schema(&self, _key: SubjectKey) -> Result<Arc<Schema>, DomainError> {
        Ok(Arc::clone(&self.0))
    }
}

/// One change to a schema. A request holds several, applied in order to a copy: all of them hold, or none is made.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SchemaChange {
    AddNodeKind {
        kind: NodeKindDoc,
    },
    /// Removing a node kind forgets all its nodes.
    RemoveNodeKind {
        name: String,
    },
    AddLinkKind {
        kind: LinkKindDoc,
    },
    RemoveLinkKind {
        name: String,
    },
    AddProperty {
        kind: String,
        property: PropertyDoc,
    },
    RemoveProperty {
        kind: String,
        name: String,
    },
    AddLinkField {
        kind: String,
        field: PropertyDoc,
    },
    RemoveLinkField {
        kind: String,
        name: String,
    },
}

/// Why a changed schema is refused.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRefusal(pub String);

impl fmt::Display for SchemaRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<SchemaRefusal> for DomainError {
    fn from(refusal: SchemaRefusal) -> Self {
        Self::InvalidValue {
            field: "schema",
            rule: refusal.0,
        }
    }
}

fn refuse<T>(reason: impl Into<String>) -> Result<T, SchemaRefusal> {
    Err(SchemaRefusal(reason.into()))
}

/// `doc` with `changes` made, in order. Removing a node kind also takes it out of the owners and the link ends that name it, and removes a link kind left with no kind at an end. The result is checked as a whole by [`validate`].
///
/// # Errors
///
/// The first change that names a kind or property that does not exist, or the first rule the result breaks.
pub fn apply(doc: &SchemaDoc, changes: &[SchemaChange]) -> Result<SchemaDoc, SchemaRefusal> {
    let mut next = doc.clone();
    for change in changes {
        match change {
            SchemaChange::AddNodeKind { kind } => next.nodes.push(kind.clone()),
            SchemaChange::RemoveNodeKind { name } => {
                let before = next.nodes.len();
                next.nodes.retain(|kind| &kind.name != name);
                if next.nodes.len() == before {
                    return refuse(format!("there is no node kind `{name}` to remove"));
                }
                // The kind leaves the owners and link ends that named it; one left with none is refused by the
                // rules below, which name it.
                for kind in &mut next.nodes {
                    if let OwnerDoc::Kinds(owners) = &mut kind.owner {
                        owners.retain(|owner| owner != name);
                    }
                }
                for link in &mut next.links {
                    link.from.retain(|end| end != name);
                    link.to.retain(|end| end != name);
                }
                // A link kind left with no kind at one end has no use: it goes with the kind.
                next.links
                    .retain(|link| !link.from.is_empty() && !link.to.is_empty());
            }
            SchemaChange::AddLinkKind { kind } => next.links.push(kind.clone()),
            SchemaChange::RemoveLinkKind { name } => {
                let before = next.links.len();
                next.links.retain(|kind| &kind.name != name);
                if next.links.len() == before {
                    return refuse(format!("there is no link kind `{name}` to remove"));
                }
            }
            SchemaChange::AddProperty { kind, property } => {
                let Some(found) = next.nodes.iter_mut().find(|node| &node.name == kind) else {
                    return refuse(format!("there is no node kind `{kind}`"));
                };
                found.properties.push(property.clone());
            }
            SchemaChange::RemoveProperty { kind, name } => {
                let Some(found) = next.nodes.iter_mut().find(|node| &node.name == kind) else {
                    return refuse(format!("there is no node kind `{kind}`"));
                };
                let before = found.properties.len();
                found.properties.retain(|property| &property.name != name);
                if found.properties.len() == before {
                    return refuse(format!("kind `{kind}` has no property `{name}` to remove"));
                }
            }
            SchemaChange::AddLinkField { kind, field } => {
                let Some(found) = next.links.iter_mut().find(|link| &link.name == kind) else {
                    return refuse(format!("there is no link kind `{kind}`"));
                };
                found.fields.push(field.clone());
            }
            SchemaChange::RemoveLinkField { kind, name } => {
                let Some(found) = next.links.iter_mut().find(|link| &link.name == kind) else {
                    return refuse(format!("there is no link kind `{kind}`"));
                };
                let before = found.fields.len();
                found.fields.retain(|field| &field.name != name);
                if found.fields.len() == before {
                    return refuse(format!(
                        "link kind `{kind}` has no field `{name}` to remove"
                    ));
                }
            }
        }
    }
    validate(&next)?;
    Ok(next)
}

/// The names of the node kinds `old` has and `new` does not.
#[must_use]
pub fn removed_node_kinds(old: &SchemaDoc, new: &SchemaDoc) -> Vec<String> {
    old.nodes
        .iter()
        .filter(|kind| new.nodes.iter().all(|kept| kept.name != kind.name))
        .map(|kind| kind.name.clone())
        .collect()
}

/// Whether `name` is spelled as a kind or property name may be.
pub(crate) fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn check_name(what: &str, name: &str) -> Result<(), SchemaRefusal> {
    if name_ok(name) {
        Ok(())
    } else {
        refuse(format!(
            "{what} name `{name}` must be lower case letters, digits and `_`, start with a letter and take at most {MAX_NAME} characters"
        ))
    }
}

fn check_text(what: &str, text: &str) -> Result<(), SchemaRefusal> {
    if text.chars().count() > MAX_DESCRIPTION {
        return refuse(format!(
            "the description of {what} takes at most {MAX_DESCRIPTION} characters"
        ));
    }
    Ok(())
}

fn check_properties(what: &str, properties: &[PropertyDoc]) -> Result<(), SchemaRefusal> {
    if properties.len() > MAX_PROPERTIES {
        return refuse(format!("{what} takes at most {MAX_PROPERTIES} properties"));
    }
    let mut seen = HashSet::new();
    for property in properties {
        check_name("a property", &property.name)?;
        if !seen.insert(property.name.as_str()) {
            return refuse(format!(
                "{what} has two properties named `{}`",
                property.name
            ));
        }
        if !property.schema.is_object() {
            return refuse(format!(
                "the schema of property `{}` of {what} must be a JSON schema object",
                property.name
            ));
        }
        // Graph storage places a property in the node's own type, where a reference to the document (`#`, `#/..`)
        // resolves against the node, not against the property: such a schema would be accepted here and refuse the
        // values it describes there.
        if let Some(reference) = local_ref(&property.schema) {
            return refuse(format!(
                "property `{}` of {what} refers to `{reference}`: a reference into the document is not allowed in a property schema",
                property.name
            ));
        }
        jsonschema::options()
            .with_draft(Draft::Draft7)
            .build(&property.schema)
            .map_err(|error| {
                SchemaRefusal(format!(
                    "the schema of property `{}` of {what} is not a valid JSON schema: {error}",
                    property.name
                ))
            })?;
    }
    // A schema that compiles alone can fail once it is a member of the object a node holds, as one that names its
    // own dialect and `$id` does: the whole is what every message of the person is checked against, and graph
    // storage compiles it nested too. Checked here, a refusal charges the person no names.
    let members: serde_json::Map<String, serde_json::Value> = properties
        .iter()
        .map(|property| (property.name.clone(), property.schema.clone()))
        .collect();
    jsonschema::options()
        .with_draft(Draft::Draft7)
        .build(&serde_json::json!({ "type": "object", "properties": members }))
        .map_err(|error| {
            SchemaRefusal(format!(
                "the properties of {what} do not make a valid JSON schema together: {error}"
            ))
        })?;
    Ok(())
}

/// The first `$ref` in `schema` that points into the document it is in, if any.
fn local_ref(schema: &serde_json::Value) -> Option<&str> {
    match schema {
        serde_json::Value::Object(map) => {
            map.iter()
                .find_map(|(key, value)| match (key.as_str(), value) {
                    ("$ref", serde_json::Value::String(target)) if target.starts_with('#') => {
                        Some(target.as_str())
                    }
                    _ => local_ref(value),
                })
        }
        serde_json::Value::Array(items) => items.iter().find_map(local_ref),
        _ => None,
    }
}

/// Checks the rules a schema keeps: names, sizes, no duplicate, a key that is a property, every kind a link or an
/// owner names exists, and every property is a JSON schema.
///
/// # Errors
///
/// The first rule the schema breaks.
pub fn validate(doc: &SchemaDoc) -> Result<(), SchemaRefusal> {
    let bytes = serde_json::to_string(doc).map_or(usize::MAX, |json| json.len());
    if bytes > MAX_DOCUMENT_BYTES {
        return refuse(format!(
            "a schema takes at most {MAX_DOCUMENT_BYTES} bytes as JSON, this one {bytes}"
        ));
    }
    if doc.nodes.len() > MAX_NODE_KINDS {
        return refuse(format!(
            "a schema takes at most {MAX_NODE_KINDS} node kinds"
        ));
    }
    if doc.links.len() > MAX_LINK_KINDS {
        return refuse(format!(
            "a schema takes at most {MAX_LINK_KINDS} link kinds"
        ));
    }
    let mut nodes = HashSet::new();
    for kind in &doc.nodes {
        check_name("a node kind", &kind.name)?;
        if RESERVED.contains(&kind.name.as_str()) {
            return refuse(format!("`{}` is a reserved name", kind.name));
        }
        if !nodes.insert(kind.name.as_str()) {
            return refuse(format!("the node kind `{}` is already in use", kind.name));
        }
    }
    let mut links = HashSet::new();
    for kind in &doc.links {
        check_name("a link kind", &kind.name)?;
        if !links.insert(kind.name.as_str()) {
            return refuse(format!("the link kind `{}` is already in use", kind.name));
        }
    }
    for kind in &doc.nodes {
        let what = format!("kind `{}`", kind.name);
        check_text(&what, &kind.description)?;
        check_properties(&what, &kind.properties)?;
        if !kind
            .properties
            .iter()
            .any(|property| property.name == kind.key)
        {
            return refuse(format!(
                "the key `{}` of {what} is not one of its properties",
                kind.key
            ));
        }
        if let OwnerDoc::Kinds(owners) = &kind.owner {
            if owners.is_empty() {
                return refuse(format!("{what} names no owner kind"));
            }
            for owner in owners {
                if !nodes.contains(owner.as_str()) {
                    return refuse(format!(
                        "{what} is owned by `{owner}`, which is not a node kind"
                    ));
                }
                if owner == &kind.name {
                    return refuse(format!("{what} cannot own itself"));
                }
            }
        }
    }
    for kind in &doc.links {
        let what = format!("link kind `{}`", kind.name);
        check_text(&what, &kind.description)?;
        check_properties(&what, &kind.fields)?;
        if kind.from.is_empty() || kind.to.is_empty() {
            return refuse(format!("{what} needs at least one kind at each end"));
        }
        for end in kind.from.iter().chain(&kind.to) {
            if !nodes.contains(end.as_str()) {
                return refuse(format!("{what} names `{end}`, which is not a node kind"));
            }
        }
    }
    Ok(())
}

/// `doc` as the JSON the gear stores.
///
/// # Errors
///
/// Internal, for a document that has no JSON form, which a schema never is.
pub fn doc_json(doc: &SchemaDoc) -> Result<String, DomainError> {
    serde_json::to_string(doc)
        .map_err(|err| DomainError::internal(format!("a schema has no JSON form: {err}")))
}

/// A stored schema and its version: how many times it was changed, an erasure counting as one; 0 for a row a charge
/// stored before any schema was set, which holds the default schema.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSchema {
    pub doc: SchemaDoc,
    pub version: i64,
    /// How many names the person has made the process keep over the schema's life: see `reserve_names`.
    pub names_charged: i64,
}

/// What a lookup of one subject's stored schema under a policy scope found.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaLookup {
    Stored(StoredSchema),
    /// The scope covers the key and the host set no schema: the default applies.
    Absent,
    OutOfScope(super::settings_scope::ScopeRefusal),
}

/// What a store of one subject's schema did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaStore {
    /// Stored, as this version.
    Stored(i64),
    /// The schema was changed since the version the change was made on: nothing is stored.
    Conflict,
    OutOfScope(super::settings_scope::ScopeRefusal),
}

/// Where the schemas the host set are kept.
#[async_trait]
pub trait SubjectSchemaRepository: Send + Sync {
    async fn find<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
    ) -> Result<SchemaLookup, DomainError>;

    /// Charges `to - from` names to the person before the process keeps any of them: moves `names_charged` from
    /// `from` to `to` on the schema at version `expected`, so of two changes made on one version only one is charged and
    /// goes on. For a person whose host set no schema (`expected` is `None`) it stores the row, holding `current`,
    /// the default schema, as version 0. Answers the version the row has: the one the change is stored over.
    #[expect(
        clippy::too_many_arguments,
        reason = "the key, the version it expects, the document, and the two counts of the compare-and-set"
    )]
    async fn reserve_names<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        current: &SchemaDoc,
        from: i64,
        to: i64,
    ) -> Result<SchemaStore, DomainError>;

    /// Stores `doc` as the version after `expected`, which is `None` for a subject with no row. `charged` is the
    /// count of names the change read and was charged against, and a precondition: the store answers `Conflict`
    /// unless the row still holds exactly that count, and it never writes the count over the row's. Only
    /// `reserve_names` moves the count, so a change that read it before another was charged cannot undo the charge.
    /// For a subject with no row the count is stored with the row.
    async fn store<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &toolkit_security::AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        doc: &SchemaDoc,
        charged: i64,
    ) -> Result<SchemaStore, DomainError>;
}

/// What a change of a person's schema does outside the gear's own table.
#[async_trait]
pub trait SchemaEffects: Send + Sync {
    /// Registers the graph types of `schema` for the person, so a kind added now takes its first node on the next
    /// message.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] when graph storage refuses the types, such as a kind removed earlier and added
    /// again with a schema graph storage holds as incompatible; the reason is graph storage's.
    async fn register(&self, key: SubjectKey, schema: &Schema) -> Result<(), DomainError>;

    /// Forgets every node of the person's that is of a kind the stored schema does not have, with the nodes they
    /// own, in one write; answers how many it forgot. Run after the schema without the kinds is stored, and again by every change, so a removal
    /// that failed is finished by sending an empty change or any other change that does not add the kind back. Safe
    /// to repeat.
    ///
    /// # Errors
    ///
    /// When the profile cannot be read or the removal cannot be written. The nodes then stay in graph storage,
    /// hidden from every read, as nodes of a kind the schema does not have.
    async fn forget(&self, key: SubjectKey) -> Result<usize, DomainError>;
}

/// Every kind, property and field name of `doc`.
fn names_of(doc: &SchemaDoc) -> HashSet<&str> {
    let mut names = HashSet::new();
    for kind in &doc.nodes {
        names.insert(kind.name.as_str());
        names.extend(
            kind.properties
                .iter()
                .map(|property| property.name.as_str()),
        );
    }
    for kind in &doc.links {
        names.insert(kind.name.as_str());
        names.extend(kind.fields.iter().map(|field| field.name.as_str()));
    }
    names
}

/// How many names `next` brings in that neither `before` has nor the process keeps already, for whichever person: what
/// the change adds to the memory the process holds, and is charged to the person who makes it.
#[must_use]
pub fn names_brought_in(before: &SchemaDoc, next: &SchemaDoc) -> usize {
    let held = names_of(before);
    construct_sdk::schema_doc::uninterned(names_of(next).difference(&held).copied())
}

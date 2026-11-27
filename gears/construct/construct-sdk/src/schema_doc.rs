//! The wire and storage form of a profile schema: [`SchemaDoc`].
//!
//! [`Schema`] names its kinds with `&'static str`, which suits the compiled-in default. A schema a person's host
//! changes at run time arrives as JSON, so it is held as a [`SchemaDoc`] with owned strings, and [`SchemaDoc::build`]
//! turns it into a [`Schema`]. The names of a built schema are interned: a name is kept for the life of the process,
//! once, however many schemas or versions use it. A description is free text and is not interned. Names are bounded
//! in length, and the gear charges each name to the person whose change first brings it in, up to a budget a person
//! may spend, so the memory the names hold grows only as far as the people who make changes are charged. A name a
//! person's change brings in is free to every other person afterwards.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::linked_schema::{LinkKind, NodeKind, Owner, PropertyDef, Schema};

/// One property of a node kind or a field of a link kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertyDoc {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    /// The property's JSON schema (draft 7).
    pub schema: Value,
}

/// Who owns the nodes of a kind: `root` is the person, a list is the kinds whose node owns them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerDoc {
    Root,
    Kinds(Vec<String>),
}

/// One node kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeKindDoc {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub owner: OwnerDoc,
    #[serde(default)]
    pub reference: bool,
    #[serde(default)]
    pub single: bool,
    /// The property that names a node of the kind.
    pub key: String,
    pub properties: Vec<PropertyDoc>,
}

/// One link kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkKindDoc {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub fields: Vec<PropertyDoc>,
}

/// A profile schema as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaDoc {
    pub nodes: Vec<NodeKindDoc>,
    pub links: Vec<LinkKindDoc>,
}

static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

/// How many of `names` the process does not keep yet: what interning them would add to its memory.
#[must_use]
pub fn uninterned<'a>(names: impl IntoIterator<Item = &'a str>) -> usize {
    let guard = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    let held = guard.as_ref();
    names
        .into_iter()
        .collect::<HashSet<&str>>()
        .into_iter()
        .filter(|name| !held.is_some_and(|set| set.contains(name)))
        .count()
}

/// `text` as a name that lives for the life of the process, kept once however often it is asked for.
#[must_use]
pub fn intern(text: &str) -> &'static str {
    let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    let names = names.get_or_insert_with(HashSet::new);
    if let Some(known) = names.get(text) {
        return known;
    }
    let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

fn properties(docs: &[PropertyDoc]) -> Vec<PropertyDef> {
    docs.iter()
        .map(|doc| PropertyDef {
            name: intern(&doc.name),
            required: doc.required,
            schema: doc.schema.clone(),
        })
        .collect()
}

fn property_docs(defs: &[PropertyDef]) -> Vec<PropertyDoc> {
    defs.iter()
        .map(|def| PropertyDoc {
            name: def.name.to_owned(),
            required: def.required,
            schema: def.schema.clone(),
        })
        .collect()
}

impl SchemaDoc {
    /// The document of `schema`.
    #[must_use]
    pub fn of(schema: &Schema) -> Self {
        Self {
            nodes: schema
                .nodes
                .iter()
                .map(|kind| NodeKindDoc {
                    name: kind.name.to_owned(),
                    description: kind.description.clone(),
                    owner: match &kind.owner {
                        Owner::Root => OwnerDoc::Root,
                        Owner::Kinds(names) => {
                            OwnerDoc::Kinds(names.iter().map(|name| (*name).to_owned()).collect())
                        }
                    },
                    reference: kind.reference,
                    single: kind.single,
                    key: kind.key.to_owned(),
                    properties: property_docs(&kind.properties),
                })
                .collect(),
            links: schema
                .links
                .iter()
                .map(|kind| LinkKindDoc {
                    name: kind.name.to_owned(),
                    description: kind.description.clone(),
                    from: kind.from.iter().map(|name| (*name).to_owned()).collect(),
                    to: kind.to.iter().map(|name| (*name).to_owned()).collect(),
                    fields: property_docs(&kind.fields),
                })
                .collect(),
        }
    }

    /// The schema of the document, with its names interned. The document is not checked: the gear validates it
    /// before it builds one.
    #[must_use]
    pub fn build(&self) -> Schema {
        Schema {
            nodes: self
                .nodes
                .iter()
                .map(|doc| NodeKind {
                    name: intern(&doc.name),
                    description: doc.description.clone(),
                    owner: match &doc.owner {
                        OwnerDoc::Root => Owner::Root,
                        OwnerDoc::Kinds(names) => {
                            Owner::Kinds(names.iter().map(|name| intern(name)).collect())
                        }
                    },
                    reference: doc.reference,
                    single: doc.single,
                    key: intern(&doc.key),
                    properties: properties(&doc.properties),
                })
                .collect(),
            links: self
                .links
                .iter()
                .map(|doc| LinkKind {
                    name: intern(&doc.name),
                    description: doc.description.clone(),
                    from: doc.from.iter().map(|name| intern(name)).collect(),
                    to: doc.to.iter().map(|name| intern(name)).collect(),
                    fields: properties(&doc.fields),
                })
                .collect(),
        }
    }
}

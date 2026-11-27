//! The profile schema: the node kinds a person's profile holds and the link kinds between them.
//!
//! A node kind has a name, its properties (each a JSON schema), who owns its nodes (the profile root, which is the
//! person, or a node of another kind) and whether it is a reference node. A link kind has a name, the kinds at its
//! ends and its fields. [`default_schema`] is the schema the gear uses: the linked profile of things (people,
//! projects, products, activities, teams, customers, partners, competitors), the entries that hang on them
//! (commitments, decisions, questions, metrics) and the reference nodes that say where something was learned
//! (tickets, sources).
//!
//! Each kind and each link kind is one graph type in graph storage. The type ids carry a per-person prefix, so the
//! kinds of two people never collide in one tenant: [`SchemaPrefix`]. One node is one value of its kind: its payload
//! holds one property, named like the kind, whose value is the object of the node's fields, besides the origin and,
//! for a record's node, the record. A link is an edge whose payload holds the link's fields, the origin and the
//! record.

use std::fmt;

use serde_json::{Map, Value, json};
use toolkit_gts::gts_id;
use uuid::Uuid;

/// Who owns the nodes of a kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The profile root: the person.
    Root,
    /// A node of one of these kinds. Forgetting that node forgets the nodes it owns.
    Kinds(Vec<&'static str>),
}

/// One property of a node kind or a field of a link kind: a name and its JSON schema.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDef {
    pub name: &'static str,
    pub required: bool,
    /// The property's JSON schema (draft 7), with its description.
    pub schema: Value,
}

/// One kind of node in the profile.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-cardinality:p1
#[derive(Debug, Clone, PartialEq)]
pub struct NodeKind {
    /// The kind's registered name, which the REST API, the SDK and the model use: lower case, one word.
    pub name: &'static str,
    /// What the kind is, for the model and the published schema.
    pub description: &'static str,
    pub owner: Owner,
    /// A reference node points at where something came from, rather than being a thing the person works with.
    pub reference: bool,
    /// Whether the profile holds at most one node of the kind.
    pub single: bool,
    /// The property that names a node of the kind: two nodes of one kind, under one owner, with the same value of
    /// it are the same node.
    pub key: &'static str,
    pub properties: Vec<PropertyDef>,
}

/// One kind of link between two nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkKind {
    pub name: &'static str,
    pub description: &'static str,
    /// The kinds a link of this kind may start at.
    pub from: Vec<&'static str>,
    /// The kinds a link of this kind may end at.
    pub to: Vec<&'static str>,
    pub fields: Vec<PropertyDef>,
}

/// A profile's schema: its node kinds and link kinds.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-person-types:p1
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub nodes: Vec<NodeKind>,
    pub links: Vec<LinkKind>,
}

/// The part of a graph type id that tells one person's kinds from another's: `u` and the person's id, in a form a
/// GTS id segment takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaPrefix(String);

impl SchemaPrefix {
    /// The prefix of the person `subject_id`.
    #[must_use]
    pub fn of(subject_id: Uuid) -> Self {
        Self(format!("u{}", subject_id.simple()))
    }

    /// The person's prefix as the GTS segment it is.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One graph type a person's schema registers: its type id and its JSON schema.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaType {
    pub type_id: String,
    pub schema: Value,
}

/// The members of a node's payload besides the one property that holds its fields.
pub const ORIGIN: &str = "origin";
pub const RECORD: &str = "record";
/// The key of the node that owns this one, in the payload of a node an entry kind owns.
pub const OWNER: &str = "owner";
/// The member of a node's payload that lists where it was learned: the tool and the message, newest last.
pub const LEARNED: &str = "learned";
/// The most places a node remembers it was learned from.
pub const LEARNED_MAX: usize = 50;
/// The member of a link's payload that holds its fields.
pub const FIELDS: &str = "fields";

const NODE_BASE: &str = gts_id!("cf.core.graph.node.v1~cf.core.graph.owned_node.v1~");
const EDGE_BASE: &str = gts_id!("cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~");

impl Schema {
    /// The node kind named `name`.
    #[must_use]
    pub fn node(&self, name: &str) -> Option<&NodeKind> {
        self.nodes.iter().find(|kind| kind.name == name)
    }

    /// The link kind named `name`.
    #[must_use]
    pub fn link(&self, name: &str) -> Option<&LinkKind> {
        self.links.iter().find(|kind| kind.name == name)
    }

    /// The type id of the node kind `name` for `prefix`.
    #[must_use]
    pub fn node_type_id(prefix: &SchemaPrefix, name: &str) -> String {
        format!("{NODE_BASE}cf.construct.{prefix}.{name}.v1~")
    }

    /// The type id of the link kind `name` for `prefix`.
    #[must_use]
    pub fn link_type_id(prefix: &SchemaPrefix, name: &str) -> String {
        format!("{EDGE_BASE}cf.construct.{prefix}.{name}.v1~")
    }

    /// The node kind a node type id of `prefix` names, none for any other type id.
    #[must_use]
    pub fn node_of_type_id(&self, prefix: &SchemaPrefix, type_id: &str) -> Option<&NodeKind> {
        self.nodes
            .iter()
            .find(|kind| Self::node_type_id(prefix, kind.name) == type_id)
    }

    /// The link kind an edge type id of `prefix` names, none for any other type id.
    #[must_use]
    pub fn link_of_type_id(&self, prefix: &SchemaPrefix, type_id: &str) -> Option<&LinkKind> {
        self.links
            .iter()
            .find(|kind| Self::link_type_id(prefix, kind.name) == type_id)
    }

    /// Every graph type of the schema for `prefix`: the node kinds, then the link kinds.
    #[must_use]
    pub fn graph_types(&self, prefix: &SchemaPrefix) -> Vec<SchemaType> {
        let nodes = self.nodes.iter().map(|kind| SchemaType {
            type_id: Self::node_type_id(prefix, kind.name),
            schema: node_schema(prefix, kind),
        });
        let links = self.links.iter().map(|kind| SchemaType {
            type_id: Self::link_type_id(prefix, kind.name),
            schema: Self::link_schema(prefix, kind),
        });
        nodes.chain(links).collect()
    }

    /// The JSON schema of the object a node of `kind` holds in its one property.
    #[must_use]
    pub fn fields_schema(kind: &NodeKind) -> Value {
        object_schema(&kind.properties)
    }

    /// The JSON schema of the object a link of `kind` holds in `fields`.
    #[must_use]
    pub fn link_fields_schema(kind: &LinkKind) -> Value {
        object_schema(&kind.fields)
    }

    fn link_schema(prefix: &SchemaPrefix, kind: &LinkKind) -> Value {
        let types = |names: &[&'static str]| -> Vec<String> {
            names
                .iter()
                .map(|name| Self::node_type_id(prefix, name))
                .collect()
        };
        let id = Self::link_type_id(prefix, kind.name);
        json!({
            "$id": format!("gts://{id}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "description": kind.description,
            "x-gts-traits": { "src_types": types(&kind.from), "dst_types": types(&kind.to) },
            "type": "object",
            "allOf": [
                { "$ref": format!("gts://{EDGE_BASE}") },
                {
                    "type": "object",
                    "properties": {
                        "payload": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": if kind.fields.iter().any(|field| field.required) {
                                json!([ORIGIN, FIELDS])
                            } else {
                                json!([ORIGIN])
                            },
                            "properties": {
                                ORIGIN: origin_schema(),
                                RECORD: record_schema(),
                                FIELDS: object_schema(&kind.fields),
                            }
                        }
                    }
                }
            ]
        })
    }
}

/// The graph type of a node kind: its payload requires `origin` and takes `record` beside it.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-fact-origin:p1
fn node_schema(prefix: &SchemaPrefix, kind: &NodeKind) -> Value {
    let id = Schema::node_type_id(prefix, kind.name);
    let mut properties = Map::new();
    properties.insert(ORIGIN.to_owned(), origin_schema());
    properties.insert(RECORD.to_owned(), record_schema());
    properties.insert(LEARNED.to_owned(), learned_schema());
    properties.insert(kind.name.to_owned(), object_schema(&kind.properties));
    let mut required = vec![json!(ORIGIN), json!(kind.name)];
    if matches!(kind.owner, Owner::Kinds(_)) {
        properties.insert(
            OWNER.to_owned(),
            json!({
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "The key of the node that owns this one."
            }),
        );
        required.push(json!(OWNER));
    }
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": format!(
            "{} One node is one {}: its payload holds one property, named like the kind, whose value is the object of the node's fields, and the origin of the node. It also holds record, the record the node came from, exactly when it came from a record.",
            kind.description, kind.name
        ),
        "type": "object",
        "allOf": [
            { "$ref": format!("gts://{NODE_BASE}") },
            {
                "type": "object",
                "properties": {
                    "payload": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": required,
                        "properties": properties,
                    }
                }
            }
        ]
    })
}

fn origin_schema() -> Value {
    json!({ "$ref": "gts://gts.cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~" })
}

fn learned_schema() -> Value {
    json!({
        "type": "array",
        "maxItems": LEARNED_MAX,
        "description": "Where the node was learned: the tool and the message of each write that touched it, the newest last.",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["tool"],
            "properties": {
                "tool": { "type": "string", "minLength": 1, "maxLength": 500, "description": "The tool the node was learned in." },
                "message": { "type": "string", "minLength": 1, "maxLength": 500, "description": "The message or record it was learned from." }
            }
        }
    })
}

fn record_schema() -> Value {
    let text = |description: &str| {
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 500,
            "pattern": "\\S",
            "description": description,
        })
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["connector", "provenance", "version"],
        "description": "The record a node or link came from, when it came from one: the connector that sent it and the record's identity.",
        "properties": {
            "connector": text("The connector that sent the record."),
            "provenance": text("The record's identity within the connector."),
            "version": text("The record's version."),
        }
    })
}

fn object_schema(properties: &[PropertyDef]) -> Value {
    let members: Map<String, Value> = properties
        .iter()
        .map(|property| (property.name.to_owned(), property.schema.clone()))
        .collect();
    let required: Vec<&str> = properties
        .iter()
        .filter(|property| property.required)
        .map(|property| property.name)
        .collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": members,
    })
}

// -- the default schema ------------------------------------------------------------------------------------

fn text(description: &str, max: usize) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": max,
        "pattern": "\\S",
        "description": description,
    })
}

fn one_of(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values, "description": description })
}

fn date(description: &str) -> Value {
    json!({
        "type": "string",
        "pattern": "^\\d{4}-\\d{2}-\\d{2}$",
        "description": format!("{description} As a date: YYYY-MM-DD."),
    })
}

fn req(name: &'static str, schema: Value) -> PropertyDef {
    PropertyDef {
        name,
        required: true,
        schema,
    }
}

fn opt(name: &'static str, schema: Value) -> PropertyDef {
    PropertyDef {
        name,
        required: false,
        schema,
    }
}

fn space() -> PropertyDef {
    opt(
        "space",
        one_of(
            "Work for the person's work, personal for the private life.",
            &["work", "personal"],
        ),
    )
}

fn status(description: &str) -> PropertyDef {
    opt("status", text(description, 100))
}

fn summary(description: &str) -> PropertyDef {
    opt("summary", text(description, 2000))
}

fn name(description: &str) -> PropertyDef {
    req("name", text(description, 200))
}

/// The kinds a thing is: the nodes the root owns that entries hang on.
const THINGS: [&str; 9] = [
    "me",
    "person",
    "project",
    "product",
    "activity",
    "team",
    "customer",
    "partner",
    "competitor",
];

fn thing(kind: &'static str, description: &'static str, properties: Vec<PropertyDef>) -> NodeKind {
    NodeKind {
        name: kind,
        description,
        owner: Owner::Root,
        reference: false,
        single: false,
        key: "name",
        properties,
    }
}

fn entry(
    kind: &'static str,
    description: &'static str,
    key: &'static str,
    properties: Vec<PropertyDef>,
) -> NodeKind {
    NodeKind {
        name: kind,
        description,
        owner: Owner::Kinds(THINGS.to_vec()),
        reference: false,
        single: false,
        key,
        properties,
    }
}

fn reference(
    kind: &'static str,
    description: &'static str,
    key: &'static str,
    properties: Vec<PropertyDef>,
) -> NodeKind {
    NodeKind {
        name: kind,
        description,
        owner: Owner::Root,
        reference: true,
        single: false,
        key,
        properties,
    }
}

fn link(
    name: &'static str,
    description: &'static str,
    from: &[&'static str],
    to: &[&'static str],
    fields: Vec<PropertyDef>,
) -> LinkKind {
    LinkKind {
        name,
        description,
        from: from.to_vec(),
        to: to.to_vec(),
        fields,
    }
}

/// The kinds a learned-from link starts at.
const LEARNED_KINDS: [&str; 9] = THINGS;
/// The kinds that point at a source or a ticket from an entry.
const ENTRIES: [&str; 4] = ["commitment", "decision", "question", "metric"];

/// The default schema: the linked profile.
///
/// The person keeps only a few direct properties on the profile root itself: name, age, profession and description.
/// They are the `me` kind, one node, so they are read, changed and forgotten like any other node.
#[must_use]
pub fn default_schema() -> Schema {
    let mut nodes = default_things();
    nodes.extend(default_entries());
    Schema {
        nodes,
        links: default_links(),
    }
}

/// The kinds the profile root owns: the person and the things the person works with.
fn default_things() -> Vec<NodeKind> {
    vec![
        NodeKind {
            name: "me",
            description: "The person the profile is of: the direct properties of the profile root. There is one.",
            owner: Owner::Root,
            reference: false,
            single: true,
            key: "name",
            properties: vec![
                name("The person's own name."),
                opt(
                    "age",
                    json!({ "type": "integer", "minimum": 0, "maximum": 150, "description": "The person's age in years." }),
                ),
                opt(
                    "profession",
                    text("What the person does for a living.", 200),
                ),
                summary("A short description of the person."),
            ],
        },
        thing(
            "person",
            "Another person the person knows: a colleague, a customer's contact, a friend or a relative.",
            vec![
                name("The other person's name."),
                opt(
                    "age",
                    json!({ "type": "integer", "minimum": 0, "maximum": 150, "description": "Their age in years." }),
                ),
                opt("profession", text("What they do for a living.", 200)),
                opt("description", text("A short description of them.", 2000)),
                space(),
            ],
        ),
        thing(
            "project",
            "A project the person works on or follows.",
            vec![
                name("The project's name."),
                status("Where the project stands, in a word or two."),
                opt("stage", text("The stage of the project.", 100)),
                summary("What the project is about."),
                space(),
            ],
        ),
        thing(
            "product",
            "A product the person works on, uses or follows.",
            vec![
                name("The product's name."),
                status("Where the product stands, in a word or two."),
                opt("platform", text("The platform the product runs on.", 200)),
                summary("What the product is."),
            ],
        ),
        thing(
            "activity",
            "Something the person does again and again, such as a weekly meeting or a course.",
            vec![
                name("The activity's name."),
                status("Where the activity stands, in a word or two."),
                opt("how_often", text("How often it takes place.", 100)),
                summary("What the activity is."),
                space(),
            ],
        ),
        thing(
            "team",
            "A team of people the person works with.",
            vec![name("The team's name."), summary("What the team does.")],
        ),
        thing(
            "customer",
            "A customer the person works for or with.",
            vec![
                name("The customer's name."),
                status("Where the relation stands, in a word or two."),
                opt(
                    "contract",
                    text("The customer's contract, as the person states it.", 500),
                ),
            ],
        ),
        thing(
            "partner",
            "A partner the person works with.",
            vec![
                name("The partner's name."),
                status("Where the relation stands, in a word or two."),
                opt(
                    "contract",
                    text("The partner's contract, as the person states it.", 500),
                ),
            ],
        ),
        thing(
            "competitor",
            "A competitor of a product.",
            vec![
                name("The competitor's name."),
                opt("price", text("What the competitor charges.", 200)),
                opt(
                    "overlap",
                    text(
                        "Where the competitor overlaps with the person's products.",
                        500,
                    ),
                ),
            ],
        ),
    ]
}

/// The entries that hang on the things, and the reference nodes.
fn default_entries() -> Vec<NodeKind> {
    vec![
        entry(
            "commitment",
            "Something the person or somebody else has to do: an action, a promise or a follow-up.",
            "text",
            vec![
                req(
                    "kind",
                    one_of(
                        "What sort of commitment it is.",
                        &["action", "promise", "follow-up"],
                    ),
                ),
                req("text", text("What has to be done.", 1000)),
                opt("due", date("When it is due.")),
                opt(
                    "done",
                    json!({ "type": "boolean", "description": "Whether it is done." }),
                ),
            ],
        ),
        entry(
            "decision",
            "A decision that was made.",
            "text",
            vec![
                req("text", text("What was decided.", 1000)),
                opt("date", date("When it was decided.")),
            ],
        ),
        entry(
            "question",
            "A question that is open or was answered.",
            "text",
            vec![
                req("text", text("The question.", 1000)),
                opt("asked_at", date("When it was asked.")),
                opt("answer", text("The answer, once there is one.", 1000)),
            ],
        ),
        entry(
            "metric",
            "A number the person tracks.",
            "label",
            vec![
                req("label", text("What the number measures.", 200)),
                req(
                    "value",
                    json!({ "type": ["string", "number"], "description": "The value, with its unit if it has one." }),
                ),
                opt("note", text("A note on the value.", 500)),
            ],
        ),
        reference(
            "ticket",
            "A ticket in a tracker, where a commitment is followed.",
            "id",
            vec![
                req("id", text("The ticket's id in its tracker.", 200)),
                opt("tracker", text("The tracker the ticket is in.", 200)),
            ],
        ),
        reference(
            "source",
            "Where something was learned: a tool, and a document or message in it.",
            "label",
            vec![
                req("tool", text("The tool the source is in.", 200)),
                req("label", text("What the source is called.", 500)),
                opt(
                    "link",
                    json!({
                        "type": "string",
                        "pattern": "^https?://\\S+$",
                        "maxLength": 2000,
                        "description": "A link to the source.",
                    }),
                ),
            ],
        ),
    ]
}

/// The links between the nodes, and from them to where they were learned.
fn default_links() -> Vec<LinkKind> {
    vec![
        link(
            "works_on",
            "A person works on a project.",
            &["person", "me"],
            &["project"],
            vec![
                opt("role", text("The person's role in the project.", 200)),
                opt("task", text("What the person does in the project.", 500)),
                opt("since", date("Since when the person works on it.")),
            ],
        ),
        link(
            "member_of",
            "A person is a member of a team.",
            &["person", "me"],
            &["team"],
            vec![],
        ),
        link(
            "lead",
            "A team is led by a person.",
            &["team"],
            &["person", "me"],
            vec![],
        ),
        link(
            "knows",
            "A person knows another person.",
            &["person", "me"],
            &["person"],
            vec![opt("relation", text("How they know each other.", 200))],
        ),
        link(
            "account_of",
            "A person is the contact of a customer.",
            &["person"],
            &["customer"],
            vec![],
        ),
        link(
            "about",
            "A project is about a product.",
            &["project"],
            &["product"],
            vec![],
        ),
        link(
            "done_by",
            "A project is done by a team.",
            &["project"],
            &["team"],
            vec![],
        ),
        link(
            "related_to",
            "A project is related to another project.",
            &["project"],
            &["project"],
            vec![],
        ),
        link(
            "serves",
            "An activity serves a project.",
            &["activity"],
            &["project"],
            vec![],
        ),
        link(
            "with",
            "An activity is with a customer.",
            &["activity"],
            &["customer"],
            vec![],
        ),
        link(
            "owned_by",
            "A product is owned by a team.",
            &["product"],
            &["team"],
            vec![],
        ),
        link(
            "uses",
            "A product uses another product.",
            &["product"],
            &["product"],
            vec![],
        ),
        link(
            "supplies",
            "A partner supplies a product.",
            &["partner"],
            &["product"],
            vec![],
        ),
        link(
            "competes_with",
            "A competitor competes with a product.",
            &["competitor"],
            &["product"],
            vec![],
        ),
        link(
            "owner",
            "A commitment is owned by the person who has to do it.",
            &["commitment"],
            &["person", "me"],
            vec![],
        ),
        link(
            "counterpart",
            "The person a commitment is to.",
            &["commitment"],
            &["person", "me"],
            vec![],
        ),
        link(
            "decided_by",
            "A decision was made by a person.",
            &["decision"],
            &["person", "me"],
            vec![],
        ),
        link(
            "asked_by",
            "A question was asked by a person.",
            &["question"],
            &["person", "me"],
            vec![],
        ),
        link(
            "tracked_in",
            "A commitment is tracked in a ticket.",
            &["commitment"],
            &["ticket"],
            vec![],
        ),
        link(
            "came_from",
            "An entry was learned from a source.",
            &ENTRIES,
            &["source"],
            vec![],
        ),
        link(
            "learned_from",
            "A thing was learned from a source.",
            &LEARNED_KINDS,
            &["source"],
            vec![],
        ),
    ]
}

#[cfg(test)]
#[path = "linked_schema_tests.rs"]
mod linked_schema_tests;

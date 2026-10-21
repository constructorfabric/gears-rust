//! The plan: the steps that change one subject's profile.
//!
//! The Planner builds plans in this shape. The Sensitive-Data Checks and Admission read a plan only
//! through [`Plan::values`], [`Origin::written_by_a_person`], and each step's
//! entity kind, confidence and [`Change::stores_values`].

use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The graph storage key of a node. The model never sees one; the planner
/// maps its numbered values back to node keys and gives each new entity a new
/// one.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeKey(pub String);

/// Who a plan comes from. The connector of a record is the source of the trust
/// check.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A record a connector sent: the connector identity, the record's
    /// identity (provenance and version), its type, and when the connector
    /// observed it, which the profile writer stores as the fact's
    /// `produced_at`.
    Connector {
        connector: String,
        provenance: String,
        version: String,
        type_id: String,
        observed_at: OffsetDateTime,
    },
    /// What the subject told an agent, through the manage-facts tool.
    Agent { agent: Uuid },
    /// A reviewer's corrected value.
    Reviewer { reviewer: Uuid },
    /// A tenant administrator's added or edited fact.
    Administrator { administrator: Uuid },
}

impl Origin {
    /// A plan a person wrote by hand: no model proposed it, so it carries no
    /// confidence and no connector to trust.
    #[must_use]
    pub fn written_by_a_person(&self) -> bool {
        matches!(self, Self::Reviewer { .. } | Self::Administrator { .. })
    }

    /// The name of the origin in audit events.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connector { .. } => "connector",
            Self::Agent { .. } => "agent",
            Self::Reviewer { .. } => "reviewer",
            Self::Administrator { .. } => "administrator",
        }
    }

    /// The identity of who the plan comes from, in audit events.
    #[must_use]
    pub fn actor(&self) -> String {
        match self {
            Self::Connector { connector, .. } => connector.clone(),
            Self::Agent { agent: id }
            | Self::Reviewer { reviewer: id }
            | Self::Administrator { administrator: id } => id.to_string(),
        }
    }
}

/// One property of an entity: its plain key and its value, as the JSON the
/// node stores. An entry such as a skill is an object of its fields.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: Value,
}

impl Property {
    pub fn new(name: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// What one step does to the profile.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A new entity with its properties.
    AddEntity {
        key: NodeKey,
        properties: Vec<Property>,
    },
    /// A new property on an existing entity.
    AddProperty { entity: NodeKey, property: Property },
    /// A new value for an existing property; the old value goes in the same
    /// step.
    ReplaceProperty { entity: NodeKey, property: Property },
    /// A new entity in place of an old one; the old entity goes in the same
    /// step.
    ReplaceEntity {
        old: NodeKey,
        key: NodeKey,
        properties: Vec<Property>,
    },
    /// Remove an entity.
    RemoveEntity { entity: NodeKey },
    /// Remove one property of an entity.
    RemoveProperty { entity: NodeKey, name: String },
}

impl Change {
    /// The values this change would store, in order.
    #[must_use]
    pub fn properties(&self) -> &[Property] {
        match self {
            Self::AddEntity { properties, .. } | Self::ReplaceEntity { properties, .. } => {
                properties
            }
            Self::AddProperty { property, .. } | Self::ReplaceProperty { property, .. } => {
                std::slice::from_ref(property)
            }
            Self::RemoveEntity { .. } | Self::RemoveProperty { .. } => &[],
        }
    }

    pub(crate) fn properties_mut(&mut self) -> Option<&mut Vec<Property>> {
        match self {
            Self::AddEntity { properties, .. } | Self::ReplaceEntity { properties, .. } => {
                Some(properties)
            }
            _ => None,
        }
    }

    /// The value of the `index`th property the change would store.
    pub(crate) fn value_mut(&mut self, index: usize) -> Option<&mut Value> {
        match self {
            Self::AddEntity { properties, .. } | Self::ReplaceEntity { properties, .. } => {
                properties.get_mut(index).map(|p| &mut p.value)
            }
            Self::AddProperty { property, .. } | Self::ReplaceProperty { property, .. } => {
                (index == 0).then_some(&mut property.value)
            }
            Self::RemoveEntity { .. } | Self::RemoveProperty { .. } => None,
        }
    }

    /// Whether the change stores a value, as opposed to only removing one.
    #[must_use]
    pub fn stores_values(&self) -> bool {
        !matches!(
            self,
            Self::RemoveEntity { .. } | Self::RemoveProperty { .. }
        )
    }

    /// Whether the change takes an old value away in the same step. A blocked
    /// new value then drops the whole step, so the old value stays.
    #[must_use]
    pub fn replaces(&self) -> bool {
        matches!(
            self,
            Self::ReplaceProperty { .. } | Self::ReplaceEntity { .. }
        )
    }
}

/// One step of a plan.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The entity kind of the entity the step touches.
    pub entity_kind: String,
    pub change: Change,
    /// The confidence the model gives the step, from 0 to 1. A plan a person
    /// wrote has none.
    pub confidence: Option<f32>,
}

/// The steps that change one subject's profile, built in memory and never
/// written as such: only the admitted steps reach the profile.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub origin: Origin,
    /// The version of the profile the plan was built on; none for a subject
    /// without a profile yet.
    pub profile_version: Option<u64>,
    pub steps: Vec<Step>,
}

/// Where a text piece sits in a plan: the step, the property within it, and
/// the piece within the property's value, counted as [`text_pieces`] walks it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueRef {
    pub step: usize,
    pub property: usize,
    pub piece: usize,
}

impl ValueRef {
    /// The property whose value the piece belongs to.
    #[must_use]
    pub fn property_ref(self) -> PropertyRef {
        PropertyRef {
            step: self.step,
            property: self.property,
        }
    }
}

/// Where a property sits in a plan: the step and the property within it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PropertyRef {
    pub step: usize,
    pub property: usize,
}

/// One text piece a plan would store, as the sensitive-data checks see it: a
/// string or a number inside a value. The keys of an object are its type's
/// field names and are not checked.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanValue {
    pub at: ValueRef,
    pub text: String,
}

impl Plan {
    /// Every text piece the plan would store. Removals store nothing and have
    /// none.
    #[must_use]
    pub fn values(&self) -> Vec<PlanValue> {
        let mut values = Vec::new();
        for (step, s) in self.steps.iter().enumerate() {
            for (property, p) in s.change.properties().iter().enumerate() {
                values.extend(text_pieces(&p.value).into_iter().enumerate().map(
                    |(piece, text)| PlanValue {
                        at: ValueRef {
                            step,
                            property,
                            piece,
                        },
                        text,
                    },
                ));
            }
        }
        values
    }
}

/// One text piece of a value: where it sits, as a JSON pointer into the value
/// (RFC 6901), and its text.
struct Piece {
    pointer: String,
    text: String,
}

/// The one walk that numbers the text pieces of a value, depth first: every
/// string, and every number as its text. Booleans and nulls carry no text.
/// [`text_pieces`] and [`text_piece_mut`] both use it, so a piece index means
/// the same piece in both.
fn pieces(value: &Value) -> Vec<Piece> {
    fn walk(value: &Value, pointer: &mut String, out: &mut Vec<Piece>) {
        match value {
            Value::String(text) => out.push(Piece {
                pointer: pointer.clone(),
                text: text.clone(),
            }),
            Value::Number(number) => out.push(Piece {
                pointer: pointer.clone(),
                text: number.to_string(),
            }),
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&index.to_string());
                    walk(item, pointer, out);
                    pointer.truncate(len);
                }
            }
            Value::Object(fields) => {
                for (key, field) in fields {
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(field, pointer, out);
                    pointer.truncate(len);
                }
            }
            Value::Bool(_) | Value::Null => {}
        }
    }
    let mut out = Vec::new();
    walk(value, &mut String::new(), &mut out);
    out
}

/// The text pieces of a value, in the order of the one walk that numbers them.
#[must_use]
pub fn text_pieces(value: &Value) -> Vec<String> {
    pieces(value).into_iter().map(|piece| piece.text).collect()
}

/// The number of characters in all text pieces of a value together, counted
/// without copying them, and only as far as `limit`: the count stops once it
/// passes it.
#[must_use]
pub fn text_chars_up_to(value: &Value, limit: usize) -> usize {
    fn walk(value: &Value, limit: usize, count: &mut usize) {
        if *count > limit {
            return;
        }
        match value {
            Value::String(text) => *count += text.chars().count(),
            Value::Number(number) => *count += number.to_string().len(),
            Value::Array(items) => items.iter().for_each(|item| walk(item, limit, count)),
            Value::Object(fields) => fields.values().for_each(|field| walk(field, limit, count)),
            Value::Bool(_) | Value::Null => {}
        }
    }
    let mut count = 0;
    walk(value, limit, &mut count);
    count
}

/// One text piece, open for redaction: a string can be edited, a number
/// cannot. A caller gets no handle that could change the shape of the value,
/// so the numbering of the other pieces stays put.
#[domain_model]
#[derive(Debug)]
pub(crate) enum PieceMut<'v> {
    Text(&'v mut String),
    Number,
}

/// The `index`th text piece of a value, as the one walk numbers it.
pub(crate) fn text_piece_mut(value: &mut Value, index: usize) -> Option<PieceMut<'_>> {
    let pointer = pieces(value).into_iter().nth(index)?.pointer;
    match value.pointer_mut(&pointer)? {
        Value::String(text) => Some(PieceMut::Text(text)),
        Value::Number(_) => Some(PieceMut::Number),
        _ => None,
    }
}

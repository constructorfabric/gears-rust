//! The plan: the steps that change one subject's profile.
//!
//! This shape is agreed with Story 5.2 (the Planner, ROL-17080), which builds
//! plans on it. The Sensitive-Data Checks and Admission read a plan only
//! through [`Plan::values`], [`Origin::written_by_a_person`], and each step's
//! entity kind, confidence and [`Change::stores_values`].

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

/// One property of an entity: its plain key and its value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub name: String,
    pub value: String,
}

impl Property {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
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
    pub(crate) fn value_mut(&mut self, index: usize) -> Option<&mut String> {
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

/// Where a value sits in a plan: the step and the property within it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueRef {
    pub step: usize,
    pub property: usize,
}

/// One value a plan would store, as the sensitive-data checks see it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanValue {
    pub at: ValueRef,
    pub text: String,
}

impl Plan {
    /// Every value the plan would store. Removals store nothing and have none.
    #[must_use]
    pub fn values(&self) -> Vec<PlanValue> {
        self.steps
            .iter()
            .enumerate()
            .flat_map(|(step, s)| {
                s.change
                    .properties()
                    .iter()
                    .enumerate()
                    .map(move |(property, p)| PlanValue {
                        at: ValueRef { step, property },
                        text: p.value.clone(),
                    })
            })
            .collect()
    }
}

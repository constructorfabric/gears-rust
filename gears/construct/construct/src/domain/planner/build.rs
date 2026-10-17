use serde_json::Value;
use uuid::Uuid;

use super::draft::{DraftStep, PlanDraft};
use crate::domain::plan::{Change, NodeKey, Origin, Plan, Property, Step};

impl PlanDraft<'_> {
    /// The plan the draft's steps make. Each value is one entity of its category with one property. Each new value
    /// gets a new node key in the tenant, also a value that returns after a delete, because a deleted key cannot be
    /// reused.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-plan:p1
    #[must_use]
    pub fn finish(self, tenant_id: Uuid, subject_id: Uuid, origin: Origin) -> Plan {
        let new_key = || NodeKey(format!("construct:{tenant_id}:{}", Uuid::new_v4()));
        let steps = self
            .steps
            .into_iter()
            .map(|step| match step {
                DraftStep::Add {
                    category,
                    property,
                    value,
                    confidence,
                } => Step {
                    entity_kind: category.name().to_owned(),
                    change: Change::AddEntity {
                        key: new_key(),
                        properties: vec![stored(property, value)],
                    },
                    confidence: Some(confidence),
                },
                DraftStep::Replace {
                    old,
                    value,
                    confidence,
                } => Step {
                    entity_kind: old.category.name().to_owned(),
                    change: Change::ReplaceEntity {
                        old: old.node_key,
                        key: new_key(),
                        properties: vec![stored(old.property, value)],
                    },
                    confidence: Some(confidence),
                },
                DraftStep::Remove { old, confidence } => Step {
                    entity_kind: old.category.name().to_owned(),
                    change: Change::RemoveEntity {
                        entity: old.node_key,
                    },
                    confidence: Some(confidence),
                },
            })
            .collect();
        Plan {
            tenant_id,
            subject_id,
            origin,
            profile_version: self.version,
            steps,
        }
    }
}

/// A property as a plan stores it: a text value as it is, any other value as its JSON.
fn stored(name: String, value: Value) -> Property {
    let value = match value {
        Value::String(text) => text,
        other => other.to_string(),
    };
    Property { name, value }
}

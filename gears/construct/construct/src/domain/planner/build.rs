use serde_json::Value;
use uuid::Uuid;

use super::draft::{DraftStep, NodeRef, PlanDraft};
use crate::domain::plan::{Change, NodeKey, Origin, Plan, Property, Step};

impl PlanDraft<'_> {
    /// The plan the draft's steps make. Each node is one entity of its category with one property, whose value is
    /// the object of the node's fields. Each added node gets a new node key in the tenant. A replaced node keeps its
    /// key, so its links and the nodes it owns stay. A link is a step of its link kind.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-plan:p1
    #[must_use]
    pub fn finish(self, tenant_id: Uuid, subject_id: Uuid, origin: Origin) -> Plan {
        let added: Vec<NodeKey> = self
            .steps
            .iter()
            .filter(|step| matches!(step, DraftStep::Add { .. }))
            .map(|_| NodeKey::new_in(tenant_id))
            .collect();
        let resolve = |node: &NodeRef| match node {
            NodeRef::Stored(key) => key.clone(),
            NodeRef::New(index) => added[*index].clone(),
        };
        let mut next_added = added.iter();
        let steps = self
            .steps
            .into_iter()
            .map(|step| match step {
                DraftStep::Add {
                    category,
                    value,
                    owner,
                    confidence,
                } => Step {
                    entity_kind: category.name().to_owned(),
                    change: Change::AddEntity {
                        key: next_added
                            .next()
                            .cloned()
                            .unwrap_or_else(|| NodeKey::new_in(tenant_id)),
                        owner: owner.as_ref().map(resolve),
                        properties: vec![stored(category.name(), value)],
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
                        old: old.node_key.clone(),
                        key: old.node_key,
                        owner: old.owner,
                        properties: vec![stored(old.category.name(), value)],
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
                DraftStep::Link {
                    kind,
                    from,
                    to,
                    fields,
                    confidence,
                } => Step {
                    entity_kind: kind.clone(),
                    change: Change::AddLink {
                        properties: vec![stored(&kind, fields)],
                        kind,
                        from: resolve(&from),
                        to: resolve(&to),
                    },
                    confidence: Some(confidence),
                },
                DraftStep::Unlink { link, confidence } => Step {
                    entity_kind: link.kind.clone(),
                    change: Change::RemoveLink {
                        kind: link.kind,
                        from: link.from,
                        to: link.to,
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

/// A property as a plan stores it: the value as JSON, as the node stores it, so
/// the checks see its text pieces and a redaction keeps it valid JSON.
fn stored(name: &str, value: Value) -> Property {
    Property {
        name: name.to_owned(),
        value,
    }
}

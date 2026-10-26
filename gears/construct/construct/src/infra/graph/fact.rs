use graph_storage_sdk::models::{NodeSpec, NodeView};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::error::DomainError;
use crate::domain::plan::{Origin, Plan, Property, Step};
use crate::domain::profile::{Category, Fact};

/// What each source puts in a fact's origin: who produced it, how, and the
/// model when an agent did.
const METHOD_MCP: &str = "mcp.manage_facts";
const METHOD_REVIEW: &str = "review.correction";
const METHOD_ADMIN: &str = "admin.edit";

/// The category whose plain name the plan's step carries.
fn category(entity_kind: &str) -> Result<Category, DomainError> {
    Category::from_name(entity_kind)
        .ok_or_else(|| DomainError::internal(format!("unknown entity kind `{entity_kind}`")))
}

fn timestamp(at: OffsetDateTime) -> Result<String, DomainError> {
    at.format(&Rfc3339)
        .map_err(|_| DomainError::internal("a time that has no RFC 3339 form"))
}

/// The fact's origin, graph storage's provenance attribute: the subject that
/// produced the fact, the method, the model for an agent, when the source
/// asserted it, and the step's confidence when the model gave one.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-writer-fact-payload:p1
fn origin(plan: &Plan, step: &Step, now: OffsetDateTime) -> Result<Value, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-origin
    let (produced_by, method, model, produced_at) = match &plan.origin {
        Origin::Connector {
            connector,
            type_id,
            observed_at,
            ..
        } => (connector.clone(), type_id.as_str(), None, *observed_at),
        Origin::Agent { agent } => (
            plan.subject_id.to_string(),
            METHOD_MCP,
            Some(agent.to_string()),
            now,
        ),
        Origin::Reviewer { reviewer } => (reviewer.to_string(), METHOD_REVIEW, None, now),
        Origin::Administrator { administrator } => {
            (administrator.to_string(), METHOD_ADMIN, None, now)
        }
    };
    let mut origin = json!({
        "produced_by": { "subject_id": produced_by },
        "method": method,
        "produced_at": timestamp(produced_at)?,
    });
    if let Some(model) = model {
        origin["model"] = json!(model);
    }
    if let Some(confidence) = step.confidence {
        origin["confidence"] = json!(confidence);
    }
    Ok(origin)
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-origin
}

/// The node of one new fact: its category's person type, a payload of the one
/// property, its origin and, for a record's fact, the record. The node claims
/// its new key with `expected_version = Some(0)`, so a key in use is a
/// conflict, never an overwrite.
///
/// # Errors
///
/// When the step names no known category, or a time has no RFC 3339 form.
pub fn fact_node(
    plan: &Plan,
    step: &Step,
    key: &str,
    property: &Property,
    now: OffsetDateTime,
) -> Result<NodeSpec, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
    let mut payload = Map::new();
    payload.insert(property.name.clone(), property.value.clone());
    payload.insert("origin".to_owned(), origin(plan, step, now)?);
    if let Origin::Connector {
        connector,
        provenance,
        version,
        ..
    } = &plan.origin
    {
        payload.insert(
            "record".to_owned(),
            json!({ "connector": connector, "provenance": provenance, "version": version }),
        );
    }
    Ok(NodeSpec {
        node_key: key.to_owned(),
        type_id: category(&step.entity_kind)?.type_id().to_owned(),
        name: None,
        payload: Some(Value::Object(payload)),
        expected_version: Some(0),
    })
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
}

/// The fact a stored node holds, or `None` for a node that is not a fact of a
/// person type.
#[must_use]
pub fn fact_of(node: &NodeView) -> Option<Fact> {
    let category = Category::from_type_id(&node.type_id)?;
    let payload = node.payload.as_ref()?.as_object()?;
    let (property, value) = payload
        .iter()
        .find(|(name, _)| *name != "origin" && *name != "record")?;
    Some(Fact {
        node_key: crate::domain::plan::NodeKey(node.node_key.clone()),
        category,
        property: property.clone(),
        value: value.clone(),
    })
}

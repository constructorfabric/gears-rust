use graph_storage_sdk::models::{NodeSpec, NodeView};
use serde_json::{Map, Number, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin, Plan, Property, Step};
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

fn timestamp(at: OffsetDateTime, what: &str) -> Result<String, DomainError> {
    at.format(&Rfc3339)
        .map_err(|err| DomainError::internal(format!("{what} {at} has no RFC 3339 form: {err}")))
}

/// A step's confidence as the decimal the model gave, not the binary value an
/// `f32` holds: 0.9 is stored as 0.9, never as 0.8999999761581421.
fn confidence(value: f32) -> Result<Number, DomainError> {
    value
        .to_string()
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .ok_or_else(|| DomainError::internal(format!("confidence {value} is not a number")))
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
        "produced_at": timestamp(produced_at, "produced_at")?,
    });
    if let Some(model) = model {
        origin["model"] = json!(model);
    }
    if let Some(value) = step.confidence {
        origin["confidence"] = Value::Number(confidence(value)?);
    }
    Ok(origin)
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-origin
}

/// The one property of a fact node's change.
fn single(properties: &[Property]) -> Result<&Property, DomainError> {
    match properties {
        [property] => Ok(property),
        _ => Err(DomainError::internal(format!(
            "a fact node holds one property, the plan gives {}",
            properties.len()
        ))),
    }
}

/// The node of the fact a step stores, none for a removal. The node is its
/// category's person type, with a payload of the one property, its origin
/// and, for a record's fact, the record. It claims its new key with
/// `expected_version = Some(0)`, so a key in use is a conflict, never an
/// overwrite.
///
/// # Errors
///
/// When the step changes one property of a node rather than a whole fact, its
/// fact has not exactly one property, it names no known category, or a time
/// has no RFC 3339 form. Each is a defect of the plan; no message carries a
/// value.
pub fn fact_node(
    plan: &Plan,
    step: &Step,
    now: OffsetDateTime,
) -> Result<Option<NodeSpec>, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
    let (key, properties): (&NodeKey, &[Property]) = match &step.change {
        Change::AddEntity { key, properties }
        | Change::ReplaceEntity {
            key, properties, ..
        } => (key, properties),
        Change::RemoveEntity { .. } => return Ok(None),
        // One node holds one fact, so a property is never changed on its own:
        // the planner replaces the fact's node.
        Change::AddProperty { .. } => return Err(property_change("add-property")),
        Change::ReplaceProperty { .. } => return Err(property_change("replace-property")),
        Change::RemoveProperty { .. } => return Err(property_change("remove-property")),
    };
    let property = single(properties)?;
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
    Ok(Some(NodeSpec {
        node_key: key.0.clone(),
        type_id: category(&step.entity_kind)?.type_id().to_owned(),
        name: None,
        payload: Some(Value::Object(payload)),
        expected_version: Some(0),
    }))
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
}

fn property_change(kind: &str) -> DomainError {
    DomainError::internal(format!(
        "the writer stores whole facts, not a {kind} change"
    ))
}

/// The fact a stored node holds, or `None` for a node that is not a fact of a
/// person type, or holds no fact property.
#[must_use]
pub fn fact_of(node: &NodeView) -> Option<Fact> {
    let category = Category::from_type_id(&node.type_id)?;
    let payload = node.payload.as_ref()?.as_object()?;
    let (property, value) = payload
        .iter()
        .find(|(name, _)| *name != "origin" && *name != "record")?;
    Some(Fact {
        node_key: NodeKey(node.node_key.clone()),
        category,
        property: property.clone(),
        value: value.clone(),
    })
}

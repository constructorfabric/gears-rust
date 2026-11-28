use construct_sdk::linked_schema::{LEARNED, LEARNED_MAX, OWNER, SchemaPrefix};
use graph_storage_sdk::models::{NodeSpec, NodeView};
use serde_json::{Map, Number, Value, json};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin, Plan, Property, Step};
use crate::domain::profile::{Category, Fact};
use crate::domain::profile_read::{LearnedFrom, OriginKind, StoredFact};

/// What each source puts in a fact's origin: who produced it, how, and the
/// model when an agent did.
const METHOD_MCP: &str = "mcp.manage_facts";
const METHOD_REVIEW: &str = "review.correction";
const METHOD_ADMIN: &str = "admin.edit";

/// The members of a fact's payload besides its one fact property.
const ORIGIN: &str = "origin";
const RECORD: &str = "record";
const ENVELOPE: [&str; 4] = [ORIGIN, RECORD, OWNER, LEARNED];

/// The category whose plain name the plan's step carries.
fn category(entity_kind: &str) -> Result<Category, DomainError> {
    Category::from_name(entity_kind)
        .ok_or_else(|| DomainError::internal(format!("unknown entity kind `{entity_kind}`")))
}

/// `at` in UTC, so every stored time has the same offset and compares as text.
fn timestamp(at: OffsetDateTime, what: &str) -> Result<String, DomainError> {
    at.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
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
        // A subject's plan and retention's only remove facts, so they never
        // store one.
        Origin::Subject { .. } | Origin::Retention => {
            return Err(DomainError::internal(
                "a subject's own plan or retention's stores no fact; it only removes",
            ));
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

/// A message as a node remembers it: at most the 500 characters its schema takes.
fn clipped(message: &str) -> String {
    message.chars().take(500).collect()
}

/// Where a plan's origin says a write was learned: the tool, and the message or record when there is one.
fn learned_from(plan: &Plan) -> LearnedFrom {
    match &plan.origin {
        Origin::Connector {
            connector,
            provenance,
            ..
        } => LearnedFrom {
            tool: connector.clone(),
            message: Some(clipped(provenance)),
        },
        Origin::Agent { .. } => LearnedFrom {
            tool: METHOD_MCP.to_owned(),
            message: None,
        },
        Origin::Reviewer { .. } => LearnedFrom {
            tool: METHOD_REVIEW.to_owned(),
            message: None,
        },
        Origin::Administrator { .. } => LearnedFrom {
            tool: METHOD_ADMIN.to_owned(),
            message: None,
        },
        Origin::Subject { .. } | Origin::Retention => LearnedFrom {
            tool: "construct".to_owned(),
            message: None,
        },
    }
}

/// The places a node was learned from, as its payload lists them.
pub(super) fn learned_of(payload: &Map<String, Value>) -> Vec<LearnedFrom> {
    payload
        .get(LEARNED)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(LearnedFrom {
                        tool: item.get("tool")?.as_str()?.to_owned(),
                        message: item
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The node of the fact a step stores, none for a removal. The node is its
/// category's type for the person, with a payload of the one property, its
/// origin, where it was learned and, for a record's fact, the record. It
/// claims its new key with `expected_version = Some(0)`, so a key in use is a
/// conflict, never an overwrite. A node replaced in place keeps its key and
/// the places it was learned from, and adds this write's.
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
    prefix: &SchemaPrefix,
    stored: Option<&NodeView>,
) -> Result<Option<NodeSpec>, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
    let (key, owner, properties, in_place): (&NodeKey, Option<&NodeKey>, &[Property], bool) =
        match &step.change {
            Change::AddEntity {
                key,
                owner,
                properties,
            } => (key, owner.as_ref(), properties, false),
            Change::ReplaceEntity {
                old,
                key,
                owner,
                properties,
            } => (key, owner.as_ref(), properties, old == key),
            Change::RemoveEntity { .. } | Change::AddLink { .. } | Change::RemoveLink { .. } => {
                return Ok(None);
            }
            // One node holds one fact, so a property is never changed on its own:
            // the planner replaces the fact's node.
            Change::AddProperty { .. } => return Err(property_change("add-property")),
            Change::ReplaceProperty { .. } => return Err(property_change("replace-property")),
            Change::RemoveProperty { .. } => return Err(property_change("remove-property")),
        };
    let property = single(properties)?;
    // The envelope would overwrite it, and the node would hold no fact.
    if ENVELOPE.contains(&property.name.as_str()) {
        return Err(DomainError::internal(format!(
            "a fact property cannot be named `{}`",
            property.name
        )));
    }
    let mut payload = Map::new();
    payload.insert(property.name.clone(), property.value.clone());
    payload.insert(ORIGIN.to_owned(), origin(plan, step, now)?);
    if let Some(owner) = owner {
        payload.insert(OWNER.to_owned(), json!(owner.0));
    }
    let mut learned = in_place
        .then(|| stored.and_then(|node| node.payload.as_ref()?.as_object().map(learned_of)))
        .flatten()
        .unwrap_or_default();
    learned.push(learned_from(plan));
    let skip = learned.len().saturating_sub(LEARNED_MAX);
    payload.insert(
        LEARNED.to_owned(),
        Value::Array(
            learned
                .iter()
                .skip(skip)
                .map(|place| {
                    let mut item = Map::new();
                    item.insert("tool".to_owned(), json!(place.tool));
                    if let Some(message) = &place.message {
                        item.insert("message".to_owned(), json!(message));
                    }
                    Value::Object(item)
                })
                .collect(),
        ),
    );
    if let Some(record) = record_of(plan) {
        payload.insert(RECORD.to_owned(), record);
    }
    Ok(Some(NodeSpec {
        node_key: key.0.clone(),
        type_id: category(&step.entity_kind)?.type_id(prefix),
        name: None,
        payload: Some(Value::Object(payload)),
        expected_version: if in_place { None } else { Some(0) },
    }))
    // @cpt-end:cpt-cf-construct-algo-profile-writer-write:p1:inst-write-fact
}

/// The record a plan came from, as a node or link payload holds it.
#[must_use]
pub fn record_of(plan: &Plan) -> Option<Value> {
    if let Origin::Connector {
        connector,
        provenance,
        version,
        ..
    } = &plan.origin
    {
        // The record's schema takes 500 characters of the connector and the version, and the 1024 intake takes of the
        // provenance, which is kept whole: the record's identity uses all of it.
        return Some(json!({
            "connector": clipped(connector),
            "provenance": provenance,
            "version": clipped(version),
        }));
    }
    None
}

/// The payload of the edge a link step stores: its fields, its origin and the record.
///
/// # Errors
///
/// When the step's time has no RFC 3339 form, or the step has no fields.
pub fn link_payload(plan: &Plan, step: &Step, now: OffsetDateTime) -> Result<Value, DomainError> {
    let fields = step
        .change
        .properties()
        .first()
        .map(|property| property.value.clone())
        .ok_or_else(|| DomainError::internal("a link step holds no fields"))?;
    let mut payload = Map::new();
    payload.insert(construct_sdk::linked_schema::FIELDS.to_owned(), fields);
    payload.insert(ORIGIN.to_owned(), origin(plan, step, now)?);
    if let Some(record) = record_of(plan) {
        payload.insert(RECORD.to_owned(), record);
    }
    Ok(Value::Object(payload))
}

fn property_change(kind: &str) -> DomainError {
    DomainError::internal(format!(
        "the writer stores whole facts, not a {kind} change"
    ))
}

/// The fact a stored fact node holds.
///
/// # Errors
///
/// When the node is of no person type, has no payload, or does not hold
/// exactly one fact property besides its origin and record. Graph storage's
/// validation does not rule out the last, so it is a defect of the stored
/// node, reported with its key and never with a value.
pub fn fact_of(node: &NodeView) -> Result<Fact, DomainError> {
    let defect = |what: &str| DomainError::internal(format!("fact {} {what}", node.node_key));
    let category =
        Category::from_type_id(&node.type_id).ok_or_else(|| defect("is of no node kind"))?;
    let payload = node
        .payload
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| defect("has no payload"))?;
    let mut properties = payload
        .iter()
        .filter(|(name, _)| !ENVELOPE.contains(&name.as_str()));
    let (Some((property, value)), None) = (properties.next(), properties.next()) else {
        return Err(defect("does not hold exactly one fact property"));
    };
    Ok(Fact {
        node_key: NodeKey(node.node_key.clone()),
        category,
        property: property.clone(),
        value: value.clone(),
        owner: payload
            .get(OWNER)
            .and_then(Value::as_str)
            .map(|owner| NodeKey(owner.to_owned())),
    })
}

/// Who a stored fact came from, from its payload: a record's fact names its
/// record, and the other sources name their method. `None` for a payload that
/// names neither, which only a defect writes.
fn origin_kind(payload: &Map<String, Value>) -> Option<OriginKind> {
    if payload.contains_key(RECORD) {
        return Some(OriginKind::Connector);
    }
    match payload
        .get(ORIGIN)
        .and_then(|origin| origin.get("method"))
        .and_then(Value::as_str)
    {
        Some(METHOD_MCP) => Some(OriginKind::Agent),
        Some(METHOD_REVIEW) => Some(OriginKind::Reviewer),
        Some(METHOD_ADMIN) => Some(OriginKind::Administrator),
        _ => None,
    }
}

/// The fact a stored node holds, as [`fact_of`] reads it, with its origin
/// kind, when graph storage stored it, and when its source asserted it. A
/// `produced_at` that does not parse is read as absent and logged.
///
/// # Errors
///
/// Every defect [`fact_of`] reports, and an origin that names no known
/// source; each names the node's key and never a value.
pub fn stored_fact_of(node: &NodeView) -> Result<StoredFact, DomainError> {
    let Fact {
        node_key,
        category,
        property,
        value,
        owner,
    } = fact_of(node)?;
    let payload = node
        .payload
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| DomainError::internal(format!("fact {} has no payload", node.node_key)))?;
    let origin = origin_kind(payload).ok_or_else(|| {
        DomainError::internal(format!(
            "fact {} has an origin of no known source",
            node.node_key
        ))
    })?;
    let produced_at = payload
        .get(ORIGIN)
        .and_then(|origin| origin.get("produced_at"))
        .and_then(|at| produced_at(&node.node_key, at));
    Ok(StoredFact {
        node_key,
        category,
        property,
        value,
        owner,
        learned: learned_of(payload),
        origin,
        stored_at: node.envelope.created_at,
        produced_at,
    })
}

/// A stored `produced_at`, none when it is not an RFC 3339 string; that is
/// logged with why, as only a defect writes it.
fn produced_at(node_key: &str, at: &Value) -> Option<OffsetDateTime> {
    let Some(text) = at.as_str() else {
        tracing::warn!(%node_key, "a stored fact's produced_at is not a string; it is served without it");
        return None;
    };
    OffsetDateTime::parse(text, &Rfc3339)
        .inspect_err(|error| {
            tracing::warn!(%node_key, %error, "a stored fact's produced_at is not RFC 3339; it is served without it");
        })
        .ok()
}

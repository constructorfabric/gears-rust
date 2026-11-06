//! Reading a resolved event type's governing metadata.
//!
//! An event type is a derived GTS type schema owned by the gear that emits it,
//! not a record the broker stores: what governs the type lives in
//! `x-gts-traits`, which `types-registry` materializes along the inheritance
//! chain as the type's effective traits. Ingest needs exactly one of those
//! traits, so it is read straight off that materialized trait block - the trait
//! key string and the wording of a failure keep a single home here, and
//! [`crate::domain::projection`] reads the same trait through this function
//! when it composes the API's event-type shape.

use gts::GtsInstanceId;
use types_registry_sdk::TypeSchema;

use crate::domain::error::{DomainError, ErrorCode};

/// The topic that events of this type are published to.
///
/// An event carries no `topic` field, so this is the only route from an event
/// to its stream - ingest resolves it here before selecting a partition.
///
/// A topic is an *instance* of the topic base type, carrying the stream's own
/// data and no trait metadata. The identifier therefore does not end in `~`, and
/// a type-shaped one is rejected.
///
/// # Errors
///
/// [`DomainError::Internal`] when the schema carries no effective traits, or
/// the trait is absent or is not a GTS instance identifier. `topic` is
/// `required` in the base event type's `x-gts-traits-schema` and narrowed to the
/// topic base type, so `types-registry` cannot admit an event type that fails
/// either check: a resolved schema reaching ingest without a usable topic is a
/// broken provisioning invariant, not producer input the publisher could
/// correct.
pub fn topic(event_type: &TypeSchema) -> Result<GtsInstanceId, DomainError> {
    let named = effective_traits(event_type)?
        .get("topic")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            DomainError::Internal(format!(
                "{} resolves to no `topic` trait value",
                event_type.type_id.as_ref()
            ))
        })?;

    GtsInstanceId::try_new(&named).map_err(|err| {
        DomainError::Internal(format!(
            "{} names `{named}` as its topic, which is not a GTS instance id: {err}",
            event_type.type_id.as_ref()
        ))
    })
}

/// The JSON Pointer an event type declares as its partition key.
///
/// The base declares a default, so a type that states nothing still resolves one.
///
/// # Errors
///
/// [`DomainError::Internal`] when the schema carries no effective traits, or
/// the trait is absent, which means the resolved chain never carried the base's
/// trait schema - a broken provisioning invariant rather than producer input.
pub fn partition_key(event_type: &TypeSchema) -> Result<String, DomainError> {
    effective_traits(event_type)?
        .get("partition_key")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            DomainError::Internal(format!(
                "{} resolves to no `partition_key` trait value",
                event_type.type_id.as_ref()
            ))
        })
}

/// Checks that an event type's partition-key pointer names a member its resolved
/// schema declares.
///
/// A pointer naming nothing would fail identically on every publish of the type,
/// so it is an admission failure rather than a message failure. The resolved
/// schema needed to check it is already in hand at registration.
///
/// # Errors
///
/// [`DomainError::Validation`] when the pointer is not a JSON Pointer into the
/// event, or names a member the resolved schema does not declare - the registering
/// gear can correct either. [`DomainError::Internal`] when the trait itself is
/// unreadable, or the schema carries no effective traits or resolved schema.
pub fn validate_partition_key(event_type: &TypeSchema) -> Result<(), DomainError> {
    let pointer = partition_key(event_type)?;
    // `InvalidSpec` is the code every rejected specification carries
    // (`infra::specification::bulk_load`); there is no narrower documented
    // code for a partition-key pointer.
    let rejected = |detail: &str| DomainError::Validation {
        code: ErrorCode::InvalidSpec,
        message: format!(
            "{} declares partition key `{pointer}`, which {detail}",
            event_type.type_id.as_ref()
        ),
    };

    let Some(path) = pointer.strip_prefix('/') else {
        return Err(rejected("is not a JSON Pointer into the event"));
    };
    if path.is_empty() {
        return Err(rejected("names the whole event rather than a member of it"));
    }

    let resolved = resolved_schema(event_type)?;
    let mut here = serde_json::json!({ "properties": schema_properties(resolved) });
    for segment in path.split('/') {
        // RFC 6901 escapes: `~1` is `/`, `~0` is `~`, and in that order.
        let name = segment.replace("~1", "/").replace("~0", "~");
        let Some(member) = schema_properties(&here).get(&name).cloned() else {
            return Err(rejected(&format!("names no declared member `{name}`")));
        };
        here = member;
    }
    Ok(())
}

/// The trait values `types-registry` materialized for an event type, merged
/// along its chain with the base's defaults applied.
///
/// # Errors
///
/// [`DomainError::Internal`] when the document is absent: a broken internal
/// invariant, since the default projection of a Type Schema listing selects it.
pub fn effective_traits(event_type: &TypeSchema) -> Result<&serde_json::Value, DomainError> {
    event_type
        .effective_traits
        .as_ref()
        .ok_or_else(|| missing(event_type, "effective_traits"))
}

/// The event type's schema with every ancestor inlined, as `types-registry`
/// materialized it.
///
/// # Errors
///
/// [`DomainError::Internal`] when the document is absent: a broken internal
/// invariant, since the default projection of a Type Schema listing selects it.
pub fn resolved_schema(event_type: &TypeSchema) -> Result<&serde_json::Value, DomainError> {
    event_type
        .resolved_schema
        .as_ref()
        .ok_or_else(|| missing(event_type, "resolved_schema"))
}

fn missing(event_type: &TypeSchema, document: &str) -> DomainError {
    DomainError::Internal(format!(
        "{} carries no materialized `{document}`",
        event_type.type_id.as_ref()
    ))
}

/// The property map a schema object declares, merging the branches of an `allOf`
/// so a member introduced by a narrowing is found alongside the base's own.
fn schema_properties(schema: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = serde_json::Map::new();
    if let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        merged.extend(properties.clone());
    }
    if let Some(branches) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        for branch in branches {
            merged.extend(schema_properties(branch));
        }
    }
    merged
}

#[cfg(test)]
#[path = "event_type_tests.rs"]
mod event_type_tests;

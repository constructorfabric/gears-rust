//! Row → SDK-model mapping plus the small pure helpers the stores share.
//!
//! Every conversion that can fail on malformed stored data surfaces as
//! [`UsageCollectorPluginError::Internal`] — a row already in the database that
//! cannot be reconstituted is a plugin invariant break, not a caller error.
//!
//! `UsageTypeGtsId` is the SDK newtype over `gts::GtsInstanceId`; it is built
//! from a stored `&str` via [`gts_id_from_str`] (validating `UsageTypeGtsId::new`)
//! and read back via [`gts_id_str`] (`AsRef<str>`). The constructor is fallible,
//! so [`gts_id_from_str`] returns a `Result` (deviation from the infallible
//! signature in the task skeleton — `UsageTypeGtsId::new` validates against the
//! reserved GTS base).

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use usage_collector_sdk::{
    IdempotencyKey, MetadataKey, ResourceRef, SubjectRef, UsageCollectorPluginError, UsageKind,
    UsageRecord, UsageRecordStatus, UsageType, UsageTypeGtsId,
};

use super::entity::{UsageRecordRow, UsageTypeRow};

/// Borrow the raw GTS instance id string out of a [`UsageTypeGtsId`] (for
/// binding).
#[must_use]
pub fn gts_id_str(gts_id: &UsageTypeGtsId) -> &str {
    gts_id.as_ref()
}

/// Reconstruct a validated [`UsageTypeGtsId`] from a stored string.
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] when the stored value is not
/// a valid usage-type GTS id (a stored-data invariant break).
pub fn gts_id_from_str(raw: &str) -> Result<UsageTypeGtsId, UsageCollectorPluginError> {
    UsageTypeGtsId::new(raw).map_err(|e| {
        UsageCollectorPluginError::internal(format!("stored gts_id `{raw}` invalid: {e}"))
    })
}

/// Parse a stored `status` string into [`UsageRecordStatus`].
///
/// Mirrors the SDK serde wire shape (`#[serde(rename_all = "lowercase")]`),
/// matching the DDL `CHECK (status IN ('active', 'inactive'))`.
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] for any other value.
pub fn parse_status(raw: &str) -> Result<UsageRecordStatus, UsageCollectorPluginError> {
    match raw {
        "active" => Ok(UsageRecordStatus::Active),
        "inactive" => Ok(UsageRecordStatus::Inactive),
        other => Err(UsageCollectorPluginError::internal(format!(
            "stored status `{other}` is not 'active'/'inactive'"
        ))),
    }
}

/// SQL string form of a [`UsageRecordStatus`] (inverse of [`parse_status`]).
#[must_use]
pub fn status_to_sql(status: UsageRecordStatus) -> &'static str {
    match status {
        UsageRecordStatus::Active => "active",
        UsageRecordStatus::Inactive => "inactive",
    }
}

/// Parse a stored `kind` string into [`UsageKind`] via the SDK `FromStr`
/// (`counter` / `gauge`).
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] for any other value.
pub fn parse_kind(raw: &str) -> Result<UsageKind, UsageCollectorPluginError> {
    raw.parse::<UsageKind>().map_err(|e| {
        UsageCollectorPluginError::internal(format!("stored kind `{raw}` invalid: {e}"))
    })
}

/// SQL string form of a [`UsageKind`] (inverse of [`parse_kind`]), matching the
/// DDL `CHECK (kind IN ('counter', 'gauge'))`.
#[must_use]
pub fn kind_to_sql(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Counter => "counter",
        UsageKind::Gauge => "gauge",
    }
}

/// Convert a `jsonb` object of string → string into a typed metadata map.
///
/// `Null` maps to an empty map (defensive — the column defaults to `'{}'`).
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] when the value is not a JSON
/// object, a value is not a JSON string, or a key fails [`MetadataKey::new`].
pub fn metadata_jsonb_to_map(
    value: JsonValue,
) -> Result<BTreeMap<MetadataKey, String>, UsageCollectorPluginError> {
    let obj = match value {
        JsonValue::Null => return Ok(BTreeMap::new()),
        JsonValue::Object(map) => map,
        other => {
            return Err(UsageCollectorPluginError::internal(format!(
                "stored metadata is not a JSON object: {other}"
            )));
        }
    };

    let mut out = BTreeMap::new();
    for (key, val) in obj {
        let value_str = match val {
            JsonValue::String(s) => s,
            other => {
                return Err(UsageCollectorPluginError::internal(format!(
                    "stored metadata value for key `{key}` is not a string: {other}"
                )));
            }
        };
        // `key` is already owned (the loop consumes `obj` by value), so move it
        // into the validation; the error `e` carries the offending key.
        let metadata_key = MetadataKey::new(key).map_err(|e| {
            UsageCollectorPluginError::internal(format!("stored metadata key invalid: {e}"))
        })?;
        out.insert(metadata_key, value_str);
    }
    Ok(out)
}

/// Serialize a typed metadata map into a `jsonb` object of string → string.
#[must_use]
pub fn metadata_map_to_jsonb(map: &BTreeMap<MetadataKey, String>) -> JsonValue {
    let obj = map
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), JsonValue::String(v.clone())))
        .collect::<serde_json::Map<String, JsonValue>>();
    JsonValue::Object(obj)
}

/// Map a [`UsageRecordRow`] into a validated [`UsageRecord`].
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] when any stored component
/// fails its SDK newtype validation (`gts_id`, `resource_ref`, `subject_ref`,
/// `idempotency_key`, `status`, `metadata`).
pub fn record_row_to_model(row: UsageRecordRow) -> Result<UsageRecord, UsageCollectorPluginError> {
    let gts_id = gts_id_from_str(&row.gts_id)?;

    let resource_ref = ResourceRef::new(row.resource_id, row.resource_type).map_err(|e| {
        UsageCollectorPluginError::internal(format!("stored resource_ref invalid: {e}"))
    })?;

    let subject_ref = match row.subject_id {
        Some(subject_id) => Some(SubjectRef::new(subject_id, row.subject_type).map_err(|e| {
            UsageCollectorPluginError::internal(format!("stored subject_ref invalid: {e}"))
        })?),
        None => None,
    };

    let idempotency_key = IdempotencyKey::new(row.idempotency_key).map_err(|e| {
        UsageCollectorPluginError::internal(format!("stored idempotency_key invalid: {e}"))
    })?;

    let metadata = metadata_jsonb_to_map(row.metadata)?;
    let status = parse_status(&row.status)?;

    Ok(UsageRecord {
        id: row.id,
        gts_id,
        tenant_id: row.tenant_id,
        resource_ref,
        subject_ref,
        metadata,
        value: row.value,
        idempotency_key,
        corrects_id: row.corrects_id,
        status,
        created_at: row.created_at,
    })
}

/// Map a [`UsageTypeRow`] into a validated [`UsageType`].
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] when the stored `gts_id`,
/// `kind`, or any `metadata_fields` entry fails its SDK newtype validation.
pub fn type_row_to_model(row: UsageTypeRow) -> Result<UsageType, UsageCollectorPluginError> {
    let gts_id = gts_id_from_str(&row.gts_id)?;
    let kind = parse_kind(&row.kind)?;

    let mut metadata_fields = std::collections::BTreeSet::new();
    for field in row.metadata_fields {
        // `field` is already owned (the loop consumes `metadata_fields` by
        // value), so move it into the validation; `e` carries the reason.
        let key = MetadataKey::new(field).map_err(|e| {
            UsageCollectorPluginError::internal(format!(
                "stored metadata_fields entry invalid: {e}"
            ))
        })?;
        metadata_fields.insert(key);
    }

    Ok(UsageType {
        gts_id,
        kind,
        metadata_fields,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mapper_tests.rs"]
mod mapper_tests;

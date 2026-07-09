//! Session domain primitives.
//!
//! Re-exports the SDK `Session`, `LifecycleState`, and `SessionType` types
//! so the rest of the service crate has a single canonical import path,
//! and provides:
//! - `From<sessions::Model> for Session` / `From<Session> for sessions::ActiveModel`
//!   bridges across the DB boundary (Phase 1 entity schema).
//! - Reserved-metadata helpers (`memory_strategy`, `retention_policy`,
//!   `share_expires_at`) — the ONLY sanctioned reader/writer of those keys.
//! - `public_metadata` — a view that strips reserved keys before DTO
//!   serialization (Phase 14 owns the DTO; this helper is the upstream
//!   half of the contract).
//! - `ensure_can_transition` — wraps `LifecycleState::can_transition_to`
//!   in a `ChatEngineError::Conflict`.
//
// @cpt-cf-chat-engine-domain-session:p2

pub use chat_engine_sdk::models::{LifecycleState, Session, SessionType};

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::memory_strategy::MemoryStrategy;
use crate::domain::retention::RetentionPolicy;

/// Reserved `Session.metadata` key holding the per-session memory strategy.
/// Clients MUST NOT write this key directly; use the helpers in this module.
pub const METADATA_KEY_MEMORY_STRATEGY: &str = "memory_strategy";

/// Reserved `Session.metadata` key holding the per-session retention policy.
pub const METADATA_KEY_RETENTION_POLICY: &str = "retention_policy";

/// Reserved `Session.metadata` key holding the share-link expiration time.
pub const METADATA_KEY_SHARE_EXPIRES_AT: &str = "share_expires_at";

/// Every reserved key, in declaration order. Used by `public_metadata`.
pub const RESERVED_METADATA_KEYS: &[&str] = &[
    METADATA_KEY_MEMORY_STRATEGY,
    METADATA_KEY_RETENTION_POLICY,
    METADATA_KEY_SHARE_EXPIRES_AT,
];

/// Reads `session.metadata["memory_strategy"]` and decodes it into a
/// [`MemoryStrategy`]. Returns `None` when the key is absent or the JSON
/// shape is unparseable.
#[must_use]
pub fn get_memory_strategy(s: &Session) -> Option<MemoryStrategy> {
    metadata_value(s, METADATA_KEY_MEMORY_STRATEGY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Writes `session.metadata["memory_strategy"]`. Creates the metadata JSON
/// object if it didn't exist; replaces any prior value.
pub fn set_memory_strategy(s: &mut Session, v: MemoryStrategy) {
    if let Ok(encoded) = serde_json::to_value(&v) {
        set_metadata_value(s, METADATA_KEY_MEMORY_STRATEGY, encoded);
    }
}

/// Reads `session.metadata["retention_policy"]` and decodes it into a
/// [`RetentionPolicy`]. Returns `None` when the key is absent or
/// unparseable.
#[must_use]
pub fn get_retention_policy(s: &Session) -> Option<RetentionPolicy> {
    metadata_value(s, METADATA_KEY_RETENTION_POLICY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Writes `session.metadata["retention_policy"]`. Creates the metadata
/// JSON object if it didn't exist; replaces any prior value.
pub fn set_retention_policy(s: &mut Session, v: RetentionPolicy) {
    if let Ok(encoded) = serde_json::to_value(&v) {
        set_metadata_value(s, METADATA_KEY_RETENTION_POLICY, encoded);
    }
}

/// Reads `session.metadata["share_expires_at"]` as an RFC-3339 timestamp.
/// Returns `None` when the key is absent or the value is not a valid
/// RFC-3339 string.
#[must_use]
pub fn get_share_expires_at(s: &Session) -> Option<OffsetDateTime> {
    metadata_value(s, METADATA_KEY_SHARE_EXPIRES_AT)
        .and_then(|v| v.as_str())
        .and_then(|raw| OffsetDateTime::parse(raw, &Rfc3339).ok())
}

/// Writes `session.metadata["share_expires_at"]` as an RFC-3339 timestamp.
/// Passing `None` removes the key (use this when revoking a share).
pub fn set_share_expires_at(s: &mut Session, v: Option<OffsetDateTime>) {
    match v {
        Some(ts) => {
            if let Ok(encoded) = ts.format(&Rfc3339) {
                set_metadata_value(s, METADATA_KEY_SHARE_EXPIRES_AT, Value::String(encoded));
            }
        }
        None => {
            remove_metadata_value(s, METADATA_KEY_SHARE_EXPIRES_AT);
        }
    }
}

/// Returns a cloned view of `session.metadata` with every reserved key
/// stripped. Used by Phase 14 DTO mapping so reserved internals don't leak
/// to clients.
///
/// Returns `None` when the underlying metadata is `None` *or* when stripping
/// reserved keys leaves the object empty (callers should treat an empty
/// public view the same as "no metadata").
#[must_use]
pub fn public_metadata(s: &Session) -> Option<Value> {
    let Some(Value::Object(map)) = s.metadata.as_ref() else {
        return s.metadata.clone();
    };
    let filtered: Map<String, Value> = map
        .iter()
        .filter(|(k, _)| !RESERVED_METADATA_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(Value::Object(filtered))
    }
}

/// Wraps [`LifecycleState::can_transition_to`] and converts a `false` result
/// into a [`ChatEngineError::Conflict`]. Service code MUST use this helper
/// before issuing any state-changing DB write so the error surface stays
/// uniform.
pub fn ensure_can_transition(from: LifecycleState, to: LifecycleState) -> Result<()> {
    if from.can_transition_to(&to) {
        Ok(())
    } else {
        Err(ChatEngineError::invalid_transition(from, to))
    }
}

// ---------- internal helpers ----------

fn metadata_value<'a>(s: &'a Session, key: &str) -> Option<&'a Value> {
    s.metadata
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|map| map.get(key))
}

fn set_metadata_value(s: &mut Session, key: &str, value: Value) {
    match s.metadata.as_mut().and_then(|v| v.as_object_mut()) {
        Some(map) => {
            map.insert(key.to_string(), value);
        }
        None => {
            let mut map = Map::new();
            map.insert(key.to_string(), value);
            s.metadata = Some(Value::Object(map));
        }
    }
}

fn remove_metadata_value(s: &mut Session, key: &str) {
    if let Some(map) = s.metadata.as_mut().and_then(|v| v.as_object_mut()) {
        map.remove(key);
        if map.is_empty() {
            s.metadata = None;
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

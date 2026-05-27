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

use sea_orm::ActiveValue::{NotSet, Set};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::memory_strategy::MemoryStrategy;
use crate::domain::retention::RetentionPolicy;
use crate::infra::db::entity::session as session_entity;

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

impl From<session_entity::Model> for Session {
    fn from(model: session_entity::Model) -> Self {
        let lifecycle_state = LifecycleState::from_str_value(&model.lifecycle_state)
            .unwrap_or(LifecycleState::Active);
        Session {
            session_id: model.session_id,
            tenant_id: model.tenant_id.into(),
            user_id: model.user_id.into(),
            client_id: model.client_id,
            session_type_id: model.session_type_id,
            enabled_capabilities: model.enabled_capabilities,
            metadata: model.metadata,
            lifecycle_state,
            share_token: model.share_token,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<Session> for session_entity::ActiveModel {
    fn from(s: Session) -> Self {
        session_entity::ActiveModel {
            session_id: Set(s.session_id),
            tenant_id: Set(s.tenant_id.into_inner()),
            user_id: Set(s.user_id.into_inner()),
            client_id: Set(s.client_id),
            session_type_id: Set(s.session_type_id),
            enabled_capabilities: Set(s.enabled_capabilities),
            metadata: Set(s.metadata),
            lifecycle_state: Set(s.lifecycle_state.as_str().to_string()),
            share_token: Set(s.share_token),
            // `deleted_at` / `scheduled_hard_delete_at` are owned by the
            // soft-delete service (Phase 12) — leave untouched here so an
            // accidental `From` round-trip from a non-deleted session does
            // not wipe out the columns.
            deleted_at: NotSet,
            scheduled_hard_delete_at: NotSet,
            created_at: Set(s.created_at),
            updated_at: Set(s.updated_at),
        }
    }
}

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
mod tests {
    use super::*;
    use chat_engine_sdk::models::{TenantId, UserId};
    use uuid::Uuid;

    fn sample() -> Session {
        Session {
            session_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: None,
            lifecycle_state: LifecycleState::Active,
            share_token: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn memory_strategy_roundtrip() {
        let mut s = sample();
        assert!(get_memory_strategy(&s).is_none());
        set_memory_strategy(&mut s, MemoryStrategy::SlidingWindow { window_size: 7 });
        assert!(matches!(
            get_memory_strategy(&s),
            Some(MemoryStrategy::SlidingWindow { window_size: 7 })
        ));
    }

    #[test]
    fn retention_policy_roundtrip() {
        let mut s = sample();
        set_retention_policy(&mut s, RetentionPolicy::CountBased { max_message_count: 50 });
        assert!(matches!(
            get_retention_policy(&s),
            Some(RetentionPolicy::CountBased { max_message_count: 50 })
        ));
    }

    #[test]
    fn share_expires_at_roundtrip() {
        let mut s = sample();
        let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        set_share_expires_at(&mut s, Some(ts));
        assert_eq!(get_share_expires_at(&s), Some(ts));
        set_share_expires_at(&mut s, None);
        assert!(get_share_expires_at(&s).is_none());
    }

    #[test]
    fn public_metadata_strips_reserved_keys() {
        let mut s = sample();
        s.metadata = Some(serde_json::json!({
            "memory_strategy": {"type": "full"},
            "retention_policy": {"type": "none"},
            "share_expires_at": "1970-01-01T00:00:00Z",
            "client_field": "visible",
        }));
        let public = public_metadata(&s).expect("non-reserved field remains");
        assert_eq!(public, serde_json::json!({ "client_field": "visible" }));
    }

    #[test]
    fn public_metadata_returns_none_when_only_reserved() {
        let mut s = sample();
        s.metadata = Some(serde_json::json!({
            "memory_strategy": {"type": "full"},
        }));
        assert!(public_metadata(&s).is_none());
    }

    #[test]
    fn ensure_can_transition_accepts_valid() {
        assert!(ensure_can_transition(LifecycleState::Active, LifecycleState::Archived).is_ok());
    }

    #[test]
    fn ensure_can_transition_rejects_invalid() {
        let err = ensure_can_transition(LifecycleState::HardDeleted, LifecycleState::Active)
            .unwrap_err();
        assert!(matches!(err, ChatEngineError::Conflict { .. }));
    }
}

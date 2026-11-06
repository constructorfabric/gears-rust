//! Immutable task entries (audit records of execution).

use chrono::{DateTime, Utc};
use gts::GtsTypeId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::PageInfo;

/// JSON object payload of an entry, validated against the entry type's schema.
pub type EntryPayload = serde_json::Map<String, serde_json::Value>;

/// Add an immutable entry. A worker sends its `lease_token` (action `claim`);
/// an API consumer omits it (action `create`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CreateEntryRequest {
    /// Worker path only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_token: Option<Uuid>,
    /// GTS entry type ID (`gts.cf.core.tasks.entry.v1~…~`).
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub entry_type: GtsTypeId,
    /// At most 64 KiB.
    #[schema(value_type = Object)]
    pub payload: EntryPayload,
}

impl CreateEntryRequest {
    /// Build an API-consumer entry (no lease token).
    #[must_use]
    pub const fn new(entry_type: GtsTypeId, payload: EntryPayload) -> Self {
        Self {
            lease_token: None,
            entry_type,
            payload,
        }
    }
}

/// An immutable entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[allow(
    clippy::struct_field_names,
    reason = "field names are the wire names of openapi.yaml"
)]
#[non_exhaustive]
pub struct Entry {
    /// Entry ID.
    pub id: Uuid,
    /// The task.
    pub task_id: Uuid,
    /// GTS entry type ID.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub entry_type: GtsTypeId,
    /// Creating subject.
    pub creator: String,
    /// Payload.
    #[schema(value_type = Object)]
    pub payload: EntryPayload,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

impl Entry {
    /// Build an entry.
    #[must_use]
    pub fn new(
        id: Uuid,
        task_id: Uuid,
        entry_type: GtsTypeId,
        creator: impl Into<String>,
        payload: EntryPayload,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            task_id,
            entry_type,
            creator: creator.into(),
            payload,
            created_at,
        }
    }
}

/// One page of entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct EntryPage {
    /// The entries, oldest first.
    pub items: Vec<Entry>,
    /// Paging cursors.
    pub page_info: PageInfo,
}

impl EntryPage {
    /// Build a page.
    #[must_use]
    pub const fn new(items: Vec<Entry>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Internal wire-format event envelope. Matches `event.v1.schema.json` in the design.
/// Broker-stamped fields (`partition`, `sequence`, `sequence_time`, `offset`, `offset_time`)
/// are `None` on publish payloads; the broker populates them on receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,

    // Broker-stamped (readOnly on the wire; absent on publish)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_time: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_time: Option<DateTime<Utc>>,

    // Publisher-only (writeOnly; stripped on read)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ProducerMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProducerMeta {
    pub version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_hint: Option<u32>,
}

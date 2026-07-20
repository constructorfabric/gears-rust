//! `sqlx` row structs mirroring the `usage_records` hypertable and the
//! `usage_type_catalog` table (see `migrations/0001_init.sql`).
//!
//! These carry the raw storage-typed columns; [`super::mapper`] turns a row
//! into the validated SDK model (and back where needed). Column types match the
//! DDL: `numeric` → `rust_decimal::Decimal`, `timestamptz` →
//! `time::OffsetDateTime`, `jsonb` → `serde_json::Value`, `text[]` →
//! `Vec<String>`, nullable `text` / `uuid` → `Option<…>`.

use rust_decimal::Decimal;
use time::OffsetDateTime;
use uuid::Uuid;

/// One row of the `usage_records` hypertable.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UsageRecordRow {
    /// `id` — deterministic gateway-derived record id (part of the composite PK).
    pub id: Uuid,
    /// `tenant_id` — owning tenant.
    pub tenant_id: Uuid,
    /// `gts_id` — usage-type foreign key (raw GTS instance id string).
    pub gts_id: String,
    /// `value` — signed `numeric` measurement.
    pub value: Decimal,
    /// `created_at` — `timestamptz` (hypertable time dimension + PK).
    pub created_at: OffsetDateTime,
    /// `resource_id` — resource attribution leaf.
    pub resource_id: String,
    /// `resource_type` — resource attribution leaf.
    pub resource_type: String,
    /// `subject_id` — optional subject attribution leaf.
    pub subject_id: Option<String>,
    /// `subject_type` — optional subject attribution leaf.
    pub subject_type: Option<String>,
    /// `idempotency_key` — caller-supplied dedup key.
    pub idempotency_key: String,
    /// `corrects_id` — optional compensation target.
    pub corrects_id: Option<Uuid>,
    /// `status` — `'active'` / `'inactive'`.
    pub status: String,
    /// `metadata` — `jsonb` object of declared metadata keys → string values.
    pub metadata: serde_json::Value,
    /// `ingested_at` — server insert timestamp (`DEFAULT now()`).
    pub ingested_at: OffsetDateTime,
}

/// One row of the `usage_type_catalog` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UsageTypeRow {
    /// `gts_id` — catalog primary key (raw GTS instance id string).
    pub gts_id: String,
    /// `kind` — `'counter'` / `'gauge'`.
    pub kind: String,
    /// `metadata_fields` — declared metadata keys (`text[]`).
    pub metadata_fields: Vec<String>,
}

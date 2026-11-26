//! Conversions between the contract types and the `qe_quotas` row.
//!
//! Writes narrow contract values into their columns and fail on a value that
//! does not fit (a `u64` cap above `i64::MAX`); reads parse the columns back and fail on a row that does not
//! read as the contract type, which is corruption, never caller input. A
//! validity window with neither bound reads back as no window; both forms
//! contain every instant.

use std::str::FromStr;

use gts::GtsTypeId;
use quota_enforcement_sdk::{
    CapPatch, EnforcementMode, MetricId, PeriodType, Quota, QuotaDraft, QuotaId, QuotaPatch,
    QuotaSource, QuotaStatus, QuotaType, SubjectRef, TenantId, ValidityWindow, ValidityWindowPatch,
};
use sea_orm::ActiveValue;
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::infra::storage::entity::quota;

/// Stored form of [`QuotaStatus::Active`].
pub const STATUS_ACTIVE: &str = "active";
/// Stored form of [`QuotaStatus::Deactivated`].
pub const STATUS_DEACTIVATED: &str = "deactivated";

/// A value does not cross the column boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MappingError {
    /// A caller-supplied cap above `i64::MAX`.
    #[error("cap {cap} exceeds the supported range 0..=i64::MAX")]
    CapOutOfRange {
        /// The cap.
        cap: u64,
    },
    /// A stored column does not parse as its contract type.
    #[error("column {column} does not read back as the contract type: {detail}")]
    Column {
        /// The column.
        column: &'static str,
        /// What failed.
        detail: String,
    },
}

/// The stored status name.
#[must_use]
pub const fn status_name(status: QuotaStatus) -> &'static str {
    match status {
        QuotaStatus::Active => STATUS_ACTIVE,
        QuotaStatus::Deactivated => STATUS_DEACTIVATED,
    }
}

fn column(column: &'static str, detail: impl std::fmt::Display) -> MappingError {
    MappingError::Column {
        column,
        detail: detail.to_string(),
    }
}

fn cap_to_column(cap: u64) -> Result<i64, MappingError> {
    i64::try_from(cap).map_err(|_| MappingError::CapOutOfRange { cap })
}

fn thresholds_to_json(thresholds: &[u8]) -> Result<String, MappingError> {
    serde_json::to_string(thresholds).map_err(|e| column("notification_thresholds", e))
}

fn metadata_to_json(metadata: &Map<String, Value>) -> Result<String, MappingError> {
    serde_json::to_string(metadata).map_err(|e| column("metadata", e))
}

/// The thresholds a stored JSON column holds.
///
/// # Errors
///
/// [`MappingError::Column`] when the text is not a JSON array of `u8`.
pub fn thresholds_of(json: &str) -> Result<Vec<u8>, MappingError> {
    serde_json::from_str(json).map_err(|e| column("notification_thresholds", e))
}

/// The row a new Quota is inserted as: `active`, `record_version = 1`.
///
/// # Errors
///
/// [`MappingError::CapOutOfRange`] when the cap does not fit its column.
pub fn draft_to_row(
    id: QuotaId,
    draft: &QuotaDraft,
    now: OffsetDateTime,
) -> Result<quota::ActiveModel, MappingError> {
    let cap = draft.cap.map(cap_to_column).transpose()?;
    let window = draft.validity_window.unwrap_or_default();
    Ok(quota::ActiveModel {
        id: ActiveValue::Set(id.as_uuid()),
        tenant_id: ActiveValue::Set(draft.tenant_id.as_uuid()),
        subject_type: ActiveValue::Set(draft.subject.subject_type.as_ref().to_owned()),
        subject_id: ActiveValue::Set(draft.subject.subject_id.clone()),
        metric: ActiveValue::Set(draft.metric.as_str().to_owned()),
        quota_type: ActiveValue::Set(draft.quota_type.as_gts_id().to_owned()),
        period: ActiveValue::Set(draft.period.map(|p| p.as_gts_id().to_owned())),
        enforcement_mode: ActiveValue::Set(draft.enforcement_mode.as_gts_id().to_owned()),
        cap: ActiveValue::Set(cap),
        notification_thresholds: ActiveValue::Set(thresholds_to_json(
            &draft.notification_thresholds,
        )?),
        validity_start: ActiveValue::Set(window.start),
        validity_end: ActiveValue::Set(window.end),
        fail_open_hint: ActiveValue::Set(draft.fail_open_hint),
        metadata: ActiveValue::Set(metadata_to_json(&draft.metadata)?),
        source: ActiveValue::Set(draft.source.as_gts_id().to_owned()),
        status: ActiveValue::Set(STATUS_ACTIVE.to_owned()),
        record_version: ActiveValue::Set(1),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        holds_pending: ActiveValue::Set(false),
    })
}

fn parse_enum<T: FromStr>(col: &'static str, raw: &str) -> Result<T, MappingError>
where
    T::Err: std::fmt::Display,
{
    T::from_str(raw).map_err(|e| column(col, e))
}

/// The contract type a stored row reads as.
///
/// # Errors
///
/// [`MappingError::Column`] naming the first column that does not parse.
pub fn row_to_quota(row: quota::Model) -> Result<Quota, MappingError> {
    let status = match row.status.as_str() {
        STATUS_ACTIVE => QuotaStatus::Active,
        STATUS_DEACTIVATED => QuotaStatus::Deactivated,
        other => return Err(column("status", format!("unknown status `{other}`"))),
    };
    let cap = row
        .cap
        .map(|c| u64::try_from(c).map_err(|_| column("cap", format!("negative cap {c}"))))
        .transpose()?;
    let record_version = u32::try_from(row.record_version)
        .map_err(|_| column("record_version", row.record_version))?;
    let metadata: Map<String, Value> =
        serde_json::from_str(&row.metadata).map_err(|e| column("metadata", e))?;
    let validity_window = match (row.validity_start, row.validity_end) {
        (None, None) => None,
        (start, end) => Some(ValidityWindow { start, end }),
    };
    Ok(Quota {
        id: QuotaId::new(row.id),
        tenant_id: TenantId::new(row.tenant_id),
        subject: SubjectRef {
            subject_type: GtsTypeId::try_new(&row.subject_type)
                .map_err(|e| column("subject_type", e))?,
            subject_id: row.subject_id,
        },
        metric: MetricId::parse(&row.metric).map_err(|e| column("metric", e))?,
        quota_type: parse_enum::<QuotaType>("quota_type", &row.quota_type)?,
        period: row
            .period
            .as_deref()
            .map(|p| parse_enum::<PeriodType>("period", p))
            .transpose()?,
        enforcement_mode: parse_enum::<EnforcementMode>("enforcement_mode", &row.enforcement_mode)?,
        cap,
        notification_thresholds: thresholds_of(&row.notification_thresholds)?,
        validity_window,
        fail_open_hint: row.fail_open_hint,
        metadata,
        source: parse_enum::<QuotaSource>("source", &row.source)?,
        status,
        record_version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Column-level form of a [`QuotaPatch`]: every `Some` is a column the update
/// sets; `cap: Some(None)` unbinds the cap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuotaUpdate {
    /// New cap column value; `Some(None)` unbinds the cap.
    #[allow(
        clippy::option_option,
        reason = "the column is nullable: the outer option is presence, the inner the value"
    )]
    pub cap: Option<Option<i64>>,
    /// New thresholds JSON.
    pub notification_thresholds: Option<String>,
    /// New `(validity_start, validity_end)`.
    pub validity: Option<(Option<OffsetDateTime>, Option<OffsetDateTime>)>,
    /// New metadata JSON.
    pub metadata: Option<String>,
    /// New enforcement mode id.
    pub enforcement_mode: Option<String>,
    /// New fail-open hint.
    pub fail_open_hint: Option<bool>,
}

impl QuotaUpdate {
    /// The cap the merged row holds.
    #[must_use]
    pub fn merged_cap(&self, current: &quota::Model) -> Option<i64> {
        self.cap.unwrap_or(current.cap)
    }

    /// The thresholds JSON the merged row holds.
    #[must_use]
    pub fn merged_thresholds<'a>(&'a self, current: &'a quota::Model) -> &'a str {
        self.notification_thresholds
            .as_deref()
            .unwrap_or(&current.notification_thresholds)
    }
}

/// The columns a patch sets.
///
/// # Errors
///
/// [`MappingError::CapOutOfRange`] for a bounded cap above `i64::MAX`.
pub fn patch_to_update(patch: &QuotaPatch) -> Result<QuotaUpdate, MappingError> {
    let cap = match patch.cap {
        None => None,
        Some(CapPatch::Unbounded) => Some(None),
        Some(CapPatch::Bounded(cap)) => Some(Some(cap_to_column(cap)?)),
    };
    let validity = match patch.validity_window {
        None => None,
        Some(ValidityWindowPatch::Clear) => Some((None, None)),
        Some(ValidityWindowPatch::Set(window)) => Some((window.start, window.end)),
    };
    Ok(QuotaUpdate {
        cap,
        notification_thresholds: patch
            .notification_thresholds
            .as_deref()
            .map(thresholds_to_json)
            .transpose()?,
        validity,
        metadata: patch.metadata.as_ref().map(metadata_to_json).transpose()?,
        enforcement_mode: patch.enforcement_mode.map(|m| m.as_gts_id().to_owned()),
        fail_open_hint: patch.fail_open_hint,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "quota_mapping_tests.rs"]
mod quota_mapping_tests;

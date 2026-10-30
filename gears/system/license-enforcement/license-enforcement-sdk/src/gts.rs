//! GTS types that license-enforcement registers.

use toolkit_gts::gts_type_schema;
use uuid::Uuid;

/// Status of a recipient tenant.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecipientStatus {
    /// Active.
    Active,
    /// Suspended.
    Suspended,
    /// Deleted.
    Deleted,
}

/// Recipient projection — `gts.cf.core.license_enforcement.recipient.v1~`.
///
/// The object that recipient rules address. At `delegate` the gear builds it
/// from the recipient tenant that `tenant-resolver` returns.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.license_enforcement.recipient.v1~"),
    description = "Recipient tenant of a delegation, as recipient rules see it.",
    properties = "gts_type,id,tenant_type,status"
)]
pub struct RecipientProjectionV1 {
    /// Always this type id. Wire key `type`.
    #[serde(rename = "type")]
    pub gts_type: gts::GtsTypeId,
    /// Tenant id.
    pub id: Uuid,
    /// Tenant type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_type: Option<String>,
    /// Tenant status.
    pub status: RecipientStatus,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;

//! Allowlisted upstream evidence (DESIGN `#contract-03-3-7`: "allowlisted local dependency and
//! evidence identifiers, never a raw provider payload").
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;
use uuid::Uuid;
/// Allowlisted local identifiers copied from an upstream answer; never a raw provider payload
/// (DESIGN `#contract-03-3-7`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UpstreamDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The plan the evidence concerns.
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The pricing revision the line requested.
    pub revision_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The pricing revision in effect on the assessment date.
    pub current_revision_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The SKU the evidence concerns.
    pub sku_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The SKU version the evidence concerns.
    pub sku_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The provider's own reason code, an allowlisted string.
    pub upstream_code: Option<String>,
}

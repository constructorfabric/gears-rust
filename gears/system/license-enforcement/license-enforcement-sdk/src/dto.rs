//! Request and response types of [`LicenseEnforcementApi`](crate::LicenseEnforcementApi).
//!
//! Every mutation carries an `idempotency_token`. A replay with the same token
//! and payload returns the first result; a different payload is a conflict.
//! `expected_version`, when set, must equal the current version.

use std::collections::HashMap;

use license_resolver_sdk::{Resource, Subject};
use serde::{Deserialize, Serialize};
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

use crate::ids::{DelegationId, EvidenceId, GrantId};
use crate::models::{
    Conditions, DelegatedPermissions, Grant, GrantTerms, Pack, ResourceRefInput, SubjectRefInput,
    TerminationReason, WindowInput,
};
use crate::reason::DenialReason;

/// Issue one root grant through the `issue` intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueGrantRequest {
    /// Tenant that will hold the grant.
    pub holder_tenant_id: Uuid,
    /// Grant terms.
    pub grant: GrantTerms,
    /// Idempotency token.
    pub idempotency_token: String,
}

/// Issue a pack through the `issue` intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssuePackRequest {
    /// Tenant that will hold the pack.
    pub holder_tenant_id: Uuid,
    /// One entry per member grant.
    pub grants: Vec<GrantTerms>,
    /// Idempotency token.
    pub idempotency_token: String,
}

/// Submit evidence to the linked plugin through the `import` intake. `Debug`
/// does not print the bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportEvidenceRequest {
    /// Evidence bytes, standard base64.
    pub bytes: String,
    /// Idempotency token.
    pub idempotency_token: String,
}

impl std::fmt::Debug for ImportEvidenceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportEvidenceRequest")
            .field(
                "bytes",
                &format_args!("<{} base64 chars>", self.bytes.len()),
            )
            .field("idempotency_token", &self.idempotency_token)
            .finish()
    }
}

/// What an issue or an import stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IssueResult {
    /// Stored evidence; absent when the evidence only updated plugin material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<EvidenceId>,
    /// The pack, when the roots form one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<Pack>,
    /// Created or updated roots.
    pub grants: Vec<Grant>,
}

/// Body of a verb without parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VerbRequest {
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

/// Body of a verb on a delegation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegationVerbRequest {
    /// Idempotency token.
    pub idempotency_token: String,
}

/// Body of `renew`: extend the validity in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenewRequest {
    /// New window.
    pub window: WindowInput,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

/// Body of `bind`: the holder binds a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BindRequest {
    /// The subject.
    pub subject: SubjectRefInput,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

/// Body of `replace` through the `issue` intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRequest {
    /// Successor grants.
    pub successors: Vec<GrantTerms>,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

/// Which unbound grants of the holder to delegate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum DelegationSelection {
    /// These grants.
    Ids {
        /// Grant ids.
        grant_ids: Vec<GrantId>,
    },
    /// `count` unbound grants on this resource.
    Resource {
        /// The resource.
        resource: ResourceRefInput,
        /// Number of grants.
        count: u32,
    },
}

/// Delegate unbound grants to a descendant tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateRequest {
    /// Recipient tenant; must be a descendant of the holder.
    pub recipient_tenant_id: Uuid,
    /// Grants to delegate.
    pub selection: DelegationSelection,
    /// Own window of the sub-grants, inside the parent window. Absent: inherited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowInput>,
    /// Conditions added to the parent conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    /// Permissions of the sub-grants, not wider than the parent. Absent: the
    /// parent permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<DelegatedPermissions>,
    /// Idempotency token.
    pub idempotency_token: String,
}

/// A delegation and its sub-grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Delegation {
    /// Delegation id.
    pub id: DelegationId,
    /// Tenant that delegated.
    pub grantor_tenant_id: Uuid,
    /// Tenant that holds the sub-grants.
    pub recipient_tenant_id: Uuid,
    /// The sub-grants.
    pub sub_grants: Vec<Grant>,
}

/// `OData` query of a list, as the client sends it. On the server the gear
/// binds these keys with the platform `OData` extractor, which also accepts
/// `$top` and `$skiptoken` and rejects unknown `$` options. `$filter` and
/// `$orderby` address the fields of [`GrantFilterField`] or [`PackFilterField`].
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
    toolkit::QueryParams,
)]
pub struct ListQuery {
    /// `OData` filter expression.
    #[serde(rename = "$filter", default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// `OData` order, for example `resource_type asc`.
    #[serde(rename = "$orderby", default, skip_serializing_if = "Option::is_none")]
    pub orderby: Option<String>,
    /// Page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// `next_cursor` of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Filter fields of `GET /grants`. Only feeds `#[derive(ODataFilterable)]`.
#[derive(ODataFilterable)]
#[allow(dead_code)]
struct GrantQuery {
    /// Resource contract type.
    #[odata(filter(kind = "String"))]
    pub resource_type: String,
    /// Stored state, as its wire string.
    #[odata(filter(kind = "String"))]
    pub state: String,
    /// Type of the bound subject.
    #[odata(filter(kind = "String"))]
    pub subject_type: String,
    /// Id of the bound subject.
    #[odata(filter(kind = "String"))]
    pub subject_id: String,
    /// Pack of the grant.
    #[odata(filter(kind = "Uuid"))]
    pub pack_id: Uuid,
    /// Parent grant.
    #[odata(filter(kind = "Uuid"))]
    pub parent_grant_id: Uuid,
}

pub use GrantQueryFilterField as GrantFilterField;

/// Filter fields of `GET /packs`. Only feeds `#[derive(ODataFilterable)]`.
#[derive(ODataFilterable)]
#[allow(dead_code)]
struct PackQuery {
    /// A Resource contract type of a member.
    #[odata(filter(kind = "String"))]
    pub resource_type: String,
}

pub use PackQueryFilterField as PackFilterField;

/// Explain the check of `subject` on `resource` in the caller's tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckExplainRequest {
    /// Subject of the check.
    #[schema(value_type = Object)]
    pub subject: Subject,
    /// Resource of the check.
    #[schema(value_type = Object)]
    pub resource: Resource,
}

/// The decision that a check would give, with its diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CheckExplanation {
    /// The decision.
    pub granted: bool,
    /// Why the check denies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub reason: Option<DenialReason>,
    /// Termination reason of a `terminated` denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<TerminationReason>,
    /// The matched grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<GrantId>,
    /// The decision passed in a grace period.
    #[serde(default)]
    pub grace: bool,
    /// Further diagnostics.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub diagnostics: HashMap<String, serde_json::Value>,
}

mod _api_dto_markers {
    use super::{
        BindRequest, CheckExplainRequest, CheckExplanation, DelegateRequest, Delegation,
        DelegationVerbRequest, ImportEvidenceRequest, IssueGrantRequest, IssuePackRequest,
        IssueResult, ListQuery, RenewRequest, ReplaceRequest, VerbRequest,
    };
    use crate::models::{Grant, Pack};
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for IssueGrantRequest {}
    impl RequestApiDto for IssuePackRequest {}
    impl RequestApiDto for ImportEvidenceRequest {}
    impl RequestApiDto for VerbRequest {}
    impl RequestApiDto for DelegationVerbRequest {}
    impl RequestApiDto for RenewRequest {}
    impl RequestApiDto for BindRequest {}
    impl RequestApiDto for ReplaceRequest {}
    impl RequestApiDto for DelegateRequest {}
    impl RequestApiDto for ListQuery {}
    impl RequestApiDto for CheckExplainRequest {}

    impl ResponseApiDto for IssueResult {}
    impl ResponseApiDto for Grant {}
    impl ResponseApiDto for Pack {}
    impl ResponseApiDto for Delegation {}
    impl ResponseApiDto for CheckExplanation {}
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dto_tests.rs"]
mod dto_tests;

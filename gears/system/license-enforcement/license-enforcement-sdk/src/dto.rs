//! Request and response types of [`LicenseEnforcementApi`](crate::LicenseEnforcementApi).
//!
//! Every mutation carries a non-empty `idempotency_token`. A replay with the same token
//! and payload returns the first result; a different payload is a conflict.
//! `expected_version`, when set, must equal the current version.

use std::collections::HashMap;

use license_resolver_sdk::{Resource, Subject};
use serde::{Deserialize, Serialize};
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

use crate::ids::{DelegationId, EvidenceId, GrantId};
use crate::models::{
    Conditions, DelegatedPermissions, Grant, GrantTerms, Pack, SubjectRefInput, TerminationReason,
    WindowInput,
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
#[non_exhaustive]
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

impl IssueResult {
    /// What an issue or an import stored.
    #[must_use]
    pub fn new(evidence_id: Option<EvidenceId>, pack: Option<Pack>, grants: Vec<Grant>) -> Self {
        Self {
            evidence_id,
            pack,
            grants,
        }
    }
}

/// Body of a verb without parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VerbRequest {
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

impl VerbRequest {
    /// A verb under `idempotency_token`, without a version precondition.
    #[must_use]
    pub fn new(idempotency_token: impl Into<String>) -> Self {
        Self {
            idempotency_token: idempotency_token.into(),
            expected_version: None,
        }
    }
}

/// Body of a verb on a delegation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DelegationVerbRequest {
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version of the delegation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

impl DelegationVerbRequest {
    /// A verb under `idempotency_token`, without a version precondition.
    #[must_use]
    pub fn new(idempotency_token: impl Into<String>) -> Self {
        Self {
            idempotency_token: idempotency_token.into(),
            expected_version: None,
        }
    }
}

/// Body of `renew`: extend the validity in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RenewRequest {
    /// New window.
    pub window: WindowInput,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

impl RenewRequest {
    /// A renew to `window` under `idempotency_token`, without a version
    /// precondition.
    #[must_use]
    pub fn new(window: WindowInput, idempotency_token: impl Into<String>) -> Self {
        Self {
            window,
            idempotency_token: idempotency_token.into(),
            expected_version: None,
        }
    }
}

/// Body of `bind`: the holder binds a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BindRequest {
    /// The subject.
    pub subject: SubjectRefInput,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

impl BindRequest {
    /// A bind of `subject` under `idempotency_token`, without a version
    /// precondition.
    #[must_use]
    pub fn new(subject: SubjectRefInput, idempotency_token: impl Into<String>) -> Self {
        Self {
            subject,
            idempotency_token: idempotency_token.into(),
            expected_version: None,
        }
    }
}

/// A grant of a replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Successor {
    /// Terms of the grant.
    pub terms: GrantTerms,
    /// The replaced grant whose binding this grant takes. A replaced grant
    /// with a binding must be named by one successor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<GrantId>,
}

/// Body of `replace` through the `issue` intake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ReplaceRequest {
    /// The grants of the replacement.
    pub successors: Vec<Successor>,
    /// Idempotency token.
    pub idempotency_token: String,
    /// Required current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
}

impl Successor {
    /// A successor with `terms` that takes the binding of `replaces`, if any.
    #[must_use]
    pub fn new(terms: GrantTerms, replaces: Option<GrantId>) -> Self {
        Self { terms, replaces }
    }
}

impl ReplaceRequest {
    /// A replacement by `successors` under `idempotency_token`, without a
    /// version precondition.
    #[must_use]
    pub fn new(successors: Vec<Successor>, idempotency_token: impl Into<String>) -> Self {
        Self {
            successors,
            idempotency_token: idempotency_token.into(),
            expected_version: None,
        }
    }
}

/// Delegate unbound grants to a descendant tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DelegateRequest {
    /// Recipient tenant; must be a descendant of the holder.
    pub recipient_tenant_id: Uuid,
    /// Unbound grants of the holder to delegate.
    pub grant_ids: Vec<GrantId>,
    /// Conditions added to the parent conditions. Not supported yet: a
    /// request with them is refused with `unimplemented`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    /// Permissions of the sub-grants, not wider than the parent. Absent: the
    /// parent permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<DelegatedPermissions>,
    /// The version that each of `grant_ids` must have; a grant left out is
    /// not checked. A key outside `grant_ids` is refused.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub expected_versions: std::collections::BTreeMap<GrantId, u64>,
    /// Idempotency token.
    pub idempotency_token: String,
}

impl DelegateRequest {
    /// Delegate `grant_ids` to `recipient_tenant_id` as they are: the parent
    /// window and permissions, no version precondition.
    #[must_use]
    pub fn new(
        recipient_tenant_id: Uuid,
        grant_ids: Vec<GrantId>,
        idempotency_token: impl Into<String>,
    ) -> Self {
        Self {
            recipient_tenant_id,
            grant_ids,
            conditions: None,
            permissions: None,
            expected_versions: std::collections::BTreeMap::new(),
            idempotency_token: idempotency_token.into(),
        }
    }
}

/// A delegation as one of its parties sees it: the grantor with its own
/// grants, the recipient with the sub-grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[non_exhaustive]
pub struct Delegation {
    /// Delegation id.
    pub id: DelegationId,
    /// Tenant that delegated.
    pub grantor_tenant_id: Uuid,
    /// Tenant that holds the sub-grants.
    pub recipient_tenant_id: Uuid,
    /// The grants of the caller in the delegation, by its side.
    pub grants: DelegationGrants,
    /// Version of the delegation: it grows at every verb addressed to the
    /// delegation (accept, reject, return, recall), not at a verb or a change
    /// of one sub-grant.
    pub version: u64,
}

impl Delegation {
    /// A delegation as `grants` show it to one of its parties.
    #[must_use]
    pub fn new(
        id: DelegationId,
        grantor_tenant_id: Uuid,
        recipient_tenant_id: Uuid,
        grants: DelegationGrants,
        version: u64,
    ) -> Self {
        Self {
            id,
            grantor_tenant_id,
            recipient_tenant_id,
            grants,
            version,
        }
    }
}

/// The grants that one party of a delegation sees. Neither party sees the
/// grants of the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "side", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DelegationGrants {
    /// The grantor: its own grants that it delegated.
    Given {
        /// The delegated grants, in the order of the delegation.
        grants: Vec<Grant>,
    },
    /// The recipient: the sub-grants that it holds.
    Received {
        /// The sub-grants, in the order of the delegation.
        grants: Vec<Grant>,
    },
    /// A side that this version of the SDK does not know. The server never
    /// sends it.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// `OData` query of a list, as the client sends it. On the server the gear
/// binds these keys with the platform `OData` extractor, which also accepts
/// `$top` and `$skiptoken` and rejects unknown `$` options. `$filter`
/// addresses the fields of [`GrantFilterField`] or [`PackFilterField`]; a type
/// field takes only `eq`, `ne`, and `in`, and compares without minor versions.
/// `$orderby` addresses `id` only.
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
#[non_exhaustive]
pub struct ListQuery {
    /// `OData` filter expression.
    #[serde(rename = "$filter", default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// `OData` order, for example `id desc`.
    #[serde(rename = "$orderby", default, skip_serializing_if = "Option::is_none")]
    pub orderby: Option<String>,
    /// Page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// `next_cursor` of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// `true` lists the items of the tenants below the caller too. The PDP
    /// must allow the `read_subtree` action; a grantor does not get it.
    /// Otherwise the list covers the caller's tenant alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_subtree: bool,
}

/// Filter fields of `GET /grants`. Only feeds `#[derive(ODataFilterable)]`.
#[derive(ODataFilterable)]
#[allow(dead_code)]
struct GrantQuery {
    /// Grant id. The default order and the cursor tiebreaker.
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    /// Resource contract type.
    #[odata(filter(kind = "String"))]
    pub resource_type: String,
    /// Stored state, as its wire string.
    #[odata(filter(kind = "String"))]
    pub state: String,
    /// Type of the bound subject.
    #[odata(filter(kind = "String"))]
    pub binding_subject_type: String,
    /// Id of the bound subject.
    #[odata(filter(kind = "String"))]
    pub binding_subject_id: String,
    /// Kind of the binding: `none`, `subject`, or `delegated`.
    #[odata(filter(kind = "String"))]
    pub binding_kind: String,
    /// Tenant that holds the sub-grant of a delegated grant.
    #[odata(filter(kind = "Uuid"))]
    pub binding_tenant_id: Uuid,
    /// Tenant that holds the grant. Below the caller's tenant only with
    /// [`ListQuery::include_subtree`].
    #[odata(filter(kind = "Uuid"))]
    pub holder_tenant_id: Uuid,
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
#[non_exhaustive]
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

impl CheckExplanation {
    /// The check grants through `grant_id`, in a grace period when `grace`.
    #[must_use]
    pub fn granted(grant_id: GrantId, grace: bool) -> Self {
        Self {
            granted: true,
            reason: None,
            termination_reason: None,
            grant_id: Some(grant_id),
            grace,
            diagnostics: HashMap::new(),
        }
    }

    /// The check denies for `reason`.
    #[must_use]
    pub fn denied(reason: DenialReason, termination_reason: Option<TerminationReason>) -> Self {
        Self {
            granted: false,
            reason: Some(reason),
            termination_reason,
            grant_id: None,
            grace: false,
            diagnostics: HashMap::new(),
        }
    }
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

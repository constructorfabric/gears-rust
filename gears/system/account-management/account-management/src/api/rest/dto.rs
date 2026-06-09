//! Wire DTOs for the AM REST surface. Endpoint families and canonical shapes
//! live in `docs/account-management-v1.yaml`. The `MetadataEntry.created_at`
//! omission remark stays here because it explains why this layer diverges
//! from a structural mirror of the SDK.
//!
//! The metadata entry projection deliberately omits `created_at`. The
//! upstream [`account_management_sdk::MetadataEntry`] does not expose
//! it (its gear docs document the omission explicitly — only
//! `updated_at` crosses the public contract for cache validation).

use gts::GtsTypeId;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use account_management_sdk::{
    CreateTenantRequest, IdpNewUser, IdpUser, MetadataEntry, Tenant, TenantStatus,
    UpdateTenantRequest,
};
use toolkit_security::SecurityContext;

use crate::domain::conversion::model::{
    ConversionRequest, ConversionSide, ConversionStatus, TargetMode,
};
use crate::domain::conversion::service::{
    ConversionCaller, ConversionRequestParentProjection, RequestConversionInput,
};

/// One metadata entry. `tenant_id` is echoed from the path so consumers carry
/// the full `(tenant_id, type_id)` identity tuple; the SDK projection drops
/// `tenant_id` by design.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TenantMetadataEntryDto {
    pub tenant_id: Uuid,
    pub type_id: String,
    pub value: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl TenantMetadataEntryDto {
    /// Consumes `entry`: `type_id` and `value` move without an extra clone.
    #[must_use]
    pub(crate) fn from_entry(tenant_id: Uuid, entry: MetadataEntry) -> Self {
        Self {
            tenant_id,
            type_id: entry.type_id.into(),
            value: entry.value,
            updated_at: entry.updated_at,
        }
    }
}

/// Request body for `PUT /tenants/{tenant_id}/metadata/{type_id}`.
///
/// The wire shape is the JSON payload to upsert, transmitted in-place
/// as `TenantMetadataValue` (`type: object, additionalProperties: true`
/// per `OpenAPI`). No metadata fields cross the request envelope -- the
/// chained `type_id` is the path parameter, not part of the body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(transparent)]
pub struct PutTenantMetadataDto {
    /// GTS-schema-validated metadata payload. The service rejects
    /// `null` (`DomainError::Validation`) before any state read.
    pub value: Value,
}

/// Effective-value resolution result for the inheritance-aware walk-up
/// surface.
///
/// `resolved == false` carries no `value` — the terminal empty state
/// of an `OverrideOnly` schema, a `self_managed` barrier, or a walk
/// that reached the root without a hit. Distinct from
/// `metadata_entry_not_found` (which only fires on the single-entry
/// `GET` path); `resolve` always returns HTTP 200 per FEATURE §3.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ResolvedTenantMetadataDto {
    pub tenant_id: Uuid,
    pub type_id: String,
    pub resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl ResolvedTenantMetadataDto {
    /// Build the resolved-response DTO from the service-level
    /// `Option<MetadataEntry>`. `None` collapses to `resolved=false`
    /// (empty walk); `Some` collapses to `resolved=true` with the
    /// entry's payload.
    #[must_use]
    pub(crate) fn from_resolution(
        tenant_id: Uuid,
        type_id: String,
        resolution: Option<MetadataEntry>,
    ) -> Self {
        match resolution {
            Some(entry) => Self {
                tenant_id,
                type_id,
                resolved: true,
                value: Some(entry.value),
            },
            None => Self {
                tenant_id,
                type_id,
                resolved: false,
                value: None,
            },
        }
    }
}

/// Mirrors `UserCreateRequest`. Trim/structural validation lives in
/// `UserService::create_user` (via GTS schema) — this DTO only pins the
/// wire shape.
///
/// `password` is the atomic initial credential delivered together with the
/// user create request: when present, the `IdP` plugin sets it during creation
/// so the caller can immediately password-grant. Omit it to create the user
/// without a credential (admin-reset or invite-link flow).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UserCreateRequestDto {
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<NewUserPasswordDto>,
}

/// Mirrors [`NewUserPassword`](account_management_sdk::NewUserPassword).
/// `value` is redacted in `Debug`; do not log this struct directly.
#[derive(Clone)]
#[toolkit_macros::api_dto(request)]
pub struct NewUserPasswordDto {
    pub value: String,
    #[serde(default)]
    pub temporary: bool,
}

impl std::fmt::Debug for NewUserPasswordDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewUserPasswordDto")
            .field("value", &"<redacted>")
            .field("temporary", &self.temporary)
            .finish()
    }
}

impl UserCreateRequestDto {
    /// Lower the wire DTO into the SDK-layer
    /// [`IdpNewUser`](account_management_sdk::IdpNewUser) the service
    /// consumes. Optional fields propagate as `Option::None` when the
    /// client omits them; the service does NOT inject defaults.
    #[must_use]
    pub(crate) fn into_idp_new_user(self) -> IdpNewUser {
        let mut payload = IdpNewUser::new(self.username);
        if let Some(email) = self.email {
            payload = payload.with_email(email);
        }
        if let Some(display_name) = self.display_name {
            payload = payload.with_display_name(display_name);
        }
        if let Some(first_name) = self.first_name {
            payload = payload.with_first_name(first_name);
        }
        if let Some(last_name) = self.last_name {
            payload = payload.with_last_name(last_name);
        }
        if let Some(pw) = self.password {
            payload = payload.with_password(pw.value, pw.temporary);
        }
        payload
    }
}

/// Mirrors User. Tenant-minimal projection per
/// adr-idp-user-identity-source-of-truth.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct UserDto {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
}

impl UserDto {
    #[must_use]
    pub(crate) fn from_idp_user(user: IdpUser) -> Self {
        Self {
            id: user.id,
            username: user.username,
            email: user.email,
            display_name: user.display_name,
            first_name: user.first_name,
            last_name: user.last_name,
        }
    }
}

/// Authenticated subject identity for `GET /account-management/v1/me`.
///
/// Projection of [`SecurityContext`]: the caller's subject id, optional
/// subject type, and single home tenant (`subject_tenant_id`). Pure
/// reflection of the validated token context — AM performs no
/// tenant-existence check here. `subject_type` is the optional `IdP` claim
/// and is omitted from the wire when absent rather than serialised as
/// `null`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
// Wire field names mirror the SecurityContext accessor vocabulary
// (`subject_id`, `subject_type`, `subject_tenant_id`). The shared
// `subject_` prefix is load-bearing for codegen clients — renaming
// would break the wire contract, so the lint is suppressed here.
#[allow(clippy::struct_field_names)]
pub struct MeDto {
    pub subject_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    pub subject_tenant_id: Uuid,
}

impl MeDto {
    /// Project the request's [`SecurityContext`] into the wire shape.
    #[must_use]
    pub(crate) fn from_security_context(ctx: &SecurityContext) -> Self {
        Self {
            subject_id: ctx.subject_id(),
            subject_type: ctx.subject_type().map(str::to_owned),
            subject_tenant_id: ctx.subject_tenant_id(),
        }
    }
}

// ---- Tenant hierarchy DTOs --------------------------------------

/// Wire-shape enum for `Tenant.status` on the REST surface.
///
/// Mirrors the SDK-visible variants of
/// [`account_management_sdk::TenantStatus`] one-to-one (the
/// AM-internal `provisioning` variant is never surfaced). Defined
/// locally rather than reusing the SDK enum because the upstream
/// type lives in `tenant-resolver-sdk` without a `utoipa::ToSchema`
/// derive — surfacing the SDK enum verbatim through
/// `#[schema(value_type = String)]` would erase the enum constraint
/// from the served `OpenAPI`, leaving codegen clients with `String`
/// instead of `active | suspended | deleted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum TenantStatusDto {
    Active,
    Suspended,
    Deleted,
}

impl From<TenantStatus> for TenantStatusDto {
    fn from(value: TenantStatus) -> Self {
        match value {
            TenantStatus::Active => Self::Active,
            TenantStatus::Suspended => Self::Suspended,
            TenantStatus::Deleted => Self::Deleted,
        }
    }
}

/// AM-internal projection. Wider than `tenant_resolver_sdk::TenantInfo` for
/// admin/UI consumers (carries lifecycle timestamps + depth).
///
/// Internal columns (raw `tenant_type_uuid`, retention/claim columns) never
/// cross this boundary. `tenant_type: None` only on a transient registry
/// blip — reads do not block on it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TenantDto {
    pub id: Uuid,
    pub name: String,
    pub status: TenantStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_type: Option<String>,
    pub parent_id: Option<Uuid>,
    pub self_managed: bool,
    pub depth: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Soft-delete tombstone. `Some(_)` exactly when `status == deleted`;
    /// omitted on the wire otherwise. Mirrors the SDK / storage column
    /// `Tenant.deleted_at`; the retention sweep becomes eligible at
    /// `deleted_at + retention_window` and is owned by the reaper.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub deleted_at: Option<OffsetDateTime>,
}

impl TenantDto {
    #[must_use]
    pub(crate) fn from_sdk_tenant(tenant: Tenant) -> Self {
        Self {
            id: tenant.id.0,
            name: tenant.name,
            status: tenant.status.into(),
            tenant_type: tenant.tenant_type,
            parent_id: tenant.parent_id.map(|t| t.0),
            self_managed: tenant.self_managed,
            depth: tenant.depth,
            created_at: tenant.created_at,
            updated_at: tenant.updated_at,
            deleted_at: tenant.deleted_at,
        }
    }
}

/// Request body for `POST /tenants`.
///
/// Mirrors `TenantCreateRequest` in the `OpenAPI` spec. `name`,
/// `parent_id`, and `tenant_type` (chained GTS identifier) are
/// required; `self_managed` defaults to `false` and
/// `provisioning_metadata` is an opaque payload forwarded to the
/// `IdP` plugin. The new child's UUID is **not** part of the request
/// — AM allocates it server-side so two parallel clients cannot
/// collide on the same id.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
// yaml `TenantCreateRequest.additionalProperties: false` — reject
// unknown JSON members at the wire boundary so callers that reuse
// the SDK's `CreateTenantRequest` JSON (which has a `child_id`
// field) fail fast with a clean 400 instead of receiving a 201 for
// a server-allocated UUID different from the one they sent.
#[serde(deny_unknown_fields)]
pub struct TenantCreateRequestDto {
    pub name: String,
    pub parent_id: Uuid,
    pub tenant_type: String,
    #[serde(default)]
    pub self_managed: bool,
    /// Opaque provider-specific provisioning context forwarded to the
    /// `IdP` plugin. Typed as a JSON object (not `Value`) so the wire
    /// contract — yaml `type: [object, 'null']` — is enforced by serde
    /// at the boundary: arrays / scalars are rejected with a clean
    /// 400 before the request ever touches the service or the plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning_metadata: Option<Map<String, Value>>,
}

impl TenantCreateRequestDto {
    /// Lower the wire DTO into the SDK-layer
    /// [`CreateTenantRequest`](account_management_sdk::CreateTenantRequest)
    /// the service consumes. The new child UUID is generated here —
    /// the wire contract does not let clients pick it so two parallel
    /// requests cannot collide on the same id; the service then
    /// derives the canonical type `UUIDv5` from the chained
    /// `tenant_type` string internally.
    #[must_use]
    pub(crate) fn into_sdk_create_request(self) -> CreateTenantRequest {
        let mut req = CreateTenantRequest::new(
            Uuid::new_v4(),
            self.parent_id,
            self.name,
            GtsTypeId::new(&self.tenant_type),
        )
        .with_self_managed(self.self_managed);
        if let Some(metadata) = self.provisioning_metadata {
            // The SDK takes `Value`; wrap the typed map back into the
            // owned-object variant losslessly.
            req = req.with_provisioning_metadata(Value::Object(metadata));
        }
        req
    }
}

/// PATCH body for `/tenants/{tenant_id}`. Only `name` is mutable; status
/// transitions go through `/suspend`, `/unsuspend`, `DELETE`. Empty patch →
/// service returns `code=validation`. `deny_unknown_fields` keeps the wire
/// envelope locked.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
// yaml `TenantUpdateRequest.additionalProperties: false` — reject
// unknown JSON members at the wire boundary so a client that
// PATCHes the full edited tenant object (`{"name":..., "parent_id":
// ...}`) sees an explicit 400 on the immutable field rather than
// having the immutable mutation silently dropped server-side. Also
// the regression guard for the pre-fix `status: Option<…>` field
// that silently dropped lifecycle-transition payloads.
#[serde(deny_unknown_fields)]
pub struct TenantUpdateRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl TenantUpdateRequestDto {
    /// Lower the wire DTO into the SDK-layer
    /// [`UpdateTenantRequest`](account_management_sdk::UpdateTenantRequest) patch
    /// shape the service consumes. Optional fields propagate as
    /// `Option::None` when the client omits them; no defaults are
    /// injected at this layer.
    #[must_use]
    pub(crate) fn into_sdk_tenant_update(self) -> UpdateTenantRequest {
        let mut patch = UpdateTenantRequest::new();
        if let Some(name) = self.name {
            patch = patch.with_name(name);
        }
        patch
    }
}

// ---- Conversion-request DTOs ------------------------------------

/// Wire-shape enum for the conversion-request lifecycle status on
/// every read surface (`OwnConversionRequestDto`,
/// `ChildConversionRequestDto`). Mirrors
/// [`ConversionStatus`] one-to-one. Defined locally on the wire so the
/// served `OpenAPI` carries the explicit `pending | approved |
/// cancelled | rejected | expired` enum rather than `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum ConversionStatusDto {
    Pending,
    Approved,
    Cancelled,
    Rejected,
    Expired,
}

impl From<ConversionStatus> for ConversionStatusDto {
    fn from(value: ConversionStatus) -> Self {
        match value {
            ConversionStatus::Pending => Self::Pending,
            ConversionStatus::Approved => Self::Approved,
            ConversionStatus::Cancelled => Self::Cancelled,
            ConversionStatus::Rejected => Self::Rejected,
            ConversionStatus::Expired => Self::Expired,
        }
    }
}

/// Admissible terminal transitions for PATCH (approved | cancelled |
/// rejected). `pending` is reached only via POST initiation; `expired` is
/// reaper-owned, so a caller `PATCH`ing to it would falsify `actor_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
pub enum ConversionPatchStatusDto {
    Approved,
    Cancelled,
    Rejected,
}

/// Wire-shape enum for `target_mode` on every conversion-request
/// surface. Mirrors [`TargetMode`] one-to-one. Defined locally for the
/// same `OpenAPI`-enum-preservation reason as the other wire enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub enum TargetModeDto {
    Managed,
    SelfManaged,
}

impl From<TargetMode> for TargetModeDto {
    fn from(value: TargetMode) -> Self {
        match value {
            TargetMode::Managed => Self::Managed,
            TargetMode::SelfManaged => Self::SelfManaged,
        }
    }
}

impl From<TargetModeDto> for TargetMode {
    fn from(value: TargetModeDto) -> Self {
        match value {
            TargetModeDto::Managed => Self::Managed,
            TargetModeDto::SelfManaged => Self::SelfManaged,
        }
    }
}

/// Wire-shape enum for `initiator_side` on every conversion-request
/// response. Mirrors [`ConversionSide`] one-to-one. Response-only — a
/// caller never supplies `initiator_side` on the request body (the
/// service derives it from the URL collection per the `caller`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ConversionSideDto {
    Child,
    Parent,
}

impl From<ConversionSide> for ConversionSideDto {
    fn from(value: ConversionSide) -> Self {
        match value {
            ConversionSide::Child => Self::Child,
            ConversionSide::Parent => Self::Parent,
        }
    }
}

/// Request body for
/// `POST /tenants/{tenant_id}/conversions` (initiator = converting
/// tenant). `target_mode` is REQUIRED — the service rejects any value
/// other than the strict binary inverse of the tenant's current
/// `self_managed` flag with `code=validation`. `comment` is optional
/// (1..=1000 chars when supplied; empty strings are a contract bug and
/// surface as `code=validation`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct RequestOwnConversionDto {
    pub target_mode: TargetModeDto,
    /// Optional caller-supplied rationale. The service rejects
    /// `Some("")` and any value over 1000 chars with
    /// `code=validation` BEFORE the DB write. The `min_length` /
    /// `max_length` constraints below advertise the same contract
    /// to the served `OpenAPI` so codegen clients see the bound
    /// statically rather than discovering it via a runtime 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(min_length = 1, max_length = 1000)]
    pub comment: Option<String>,
}

impl RequestOwnConversionDto {
    /// Lower the wire DTO into [`RequestConversionInput`]. The
    /// converting tenant's id is read from `caller.scope_id()` so the
    /// wire body cannot carry one that disagrees with the URL — a
    /// misrouted caller cannot route past the URL coherence gate.
    #[must_use]
    pub(crate) fn into_service_input(self, caller: ConversionCaller) -> RequestConversionInput {
        RequestConversionInput {
            tenant_id: caller.scope_id(),
            caller,
            target_mode: self.target_mode.into(),
            comment: self.comment,
        }
    }
}

/// Request body for `POST /tenants/{tenant_id}/child-conversions`
/// (initiator = parent acting on a direct child). Same field set as
/// [`RequestOwnConversionDto`] plus the explicit `child_tenant_id`
/// because the URL binds the parent — the converting child has to be
/// identified in the body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct RequestChildConversionDto {
    pub child_tenant_id: Uuid,
    pub target_mode: TargetModeDto,
    /// Optional caller-supplied rationale. Same `1..=1000` chars
    /// contract as [`RequestOwnConversionDto::comment`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(min_length = 1, max_length = 1000)]
    pub comment: Option<String>,
}

impl RequestChildConversionDto {
    /// Lower the wire DTO into [`RequestConversionInput`]. The
    /// parent-side `caller` is built from the URL; the converting
    /// child's `tenant_id` comes from the body and is reconciled by
    /// the service-layer `require_caller_scope_or_not_found` guard
    /// against the URL-bound parent's `parent_id`.
    #[must_use]
    pub(crate) fn into_service_input(self, caller: ConversionCaller) -> RequestConversionInput {
        RequestConversionInput {
            tenant_id: self.child_tenant_id,
            caller,
            target_mode: self.target_mode.into(),
            comment: self.comment,
        }
    }
}

/// Request body for
/// `PATCH /tenants/{tenant_id}/conversions/{request_id}` and
/// `PATCH /tenants/{tenant_id}/child-conversions/{request_id}`.
///
/// `status` is required and narrowed to the three admissible terminal
/// transitions ([`ConversionPatchStatusDto`]). `comment` is optional
/// and follows the same length contract as the POST body.
///
/// The handler routes each admissible status to the matching service
/// method (`approve` / `cancel` / `reject`) — the wire DTO does NOT
/// know about the service handle. See
/// [`crate::api::rest::handlers::conversions`] for the dispatch.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct ConversionPatchDto {
    pub status: ConversionPatchStatusDto,
    /// Optional caller-supplied rationale. Same `1..=1000` chars
    /// contract as [`RequestOwnConversionDto::comment`]; persisted
    /// to the per-transition column matching the chosen `status`
    /// (`approved_comment` / `cancelled_comment` / `rejected_comment`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(min_length = 1, max_length = 1000)]
    pub comment: Option<String>,
}

/// Response projection for the child-side conversion REST surface
/// (`/conversions` family). Full conversion-row projection — the
/// converting tenant has no cross-barrier projection rules because the
/// request lives inside its own scope.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct OwnConversionRequestDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub child_tenant_name: String,
    pub target_mode: TargetModeDto,
    pub initiator_side: ConversionSideDto,
    pub status: ConversionStatusDto,
    pub requested_by: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_comment: Option<String>,
}

impl OwnConversionRequestDto {
    /// Build a wire DTO from the SDK projection. Consumes the upstream
    /// `ConversionRequest` so the audit-comment strings move without
    /// an extra clone on the hot path.
    #[must_use]
    pub(crate) fn from_conversion(row: ConversionRequest) -> Self {
        Self {
            id: row.id,
            tenant_id: row.tenant_id,
            parent_id: row.parent_id,
            child_tenant_name: row.child_tenant_name,
            target_mode: row.target_mode.into(),
            initiator_side: row.initiator_side.into(),
            status: row.status.into(),
            requested_by: row.requested_by,
            approved_by: row.approved_by,
            cancelled_by: row.cancelled_by,
            rejected_by: row.rejected_by,
            created_at: row.requested_at,
            expires_at: row.expires_at,
            resolved_at: row.resolved_at,
            requested_comment: row.requested_comment,
            approved_comment: row.approved_comment,
            cancelled_comment: row.cancelled_comment,
            rejected_comment: row.rejected_comment,
        }
    }
}

/// Response projection for the parent-side conversion REST surface
/// (`/child-conversions` family). Minimal cross-barrier projection per
/// `dod-managed-self-managed-modes-parent-side-minimal-surface`: every
/// field is derivable from the conversion row itself or the converting
/// tenant's own row (`child_tenant_name`); no closure / metadata /
/// inventory data leaks across the parent-child barrier.
///
/// `request_id` is the conversion row's id (the parent-side projection
/// renames it to make the cross-barrier minimal contract explicit on
/// the wire — the field set is what the parent is allowed to see).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ChildConversionRequestDto {
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub child_tenant_name: String,
    pub target_mode: TargetModeDto,
    pub initiator_side: ConversionSideDto,
    pub status: ConversionStatusDto,
    pub requested_by: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_comment: Option<String>,
}

impl ChildConversionRequestDto {
    /// Build a parent-side wire DTO from the service-level minimal
    /// projection. Consumes the upstream projection so the
    /// `child_tenant_name` and audit-comment strings move without an
    /// extra clone on the hot path.
    #[must_use]
    pub(crate) fn from_parent_projection(projection: ConversionRequestParentProjection) -> Self {
        Self {
            request_id: projection.request_id,
            tenant_id: projection.tenant_id,
            child_tenant_name: projection.child_tenant_name,
            target_mode: projection.target_mode.into(),
            initiator_side: projection.initiator_side.into(),
            status: projection.status.into(),
            requested_by: projection.requested_by,
            approved_by: projection.approved_by,
            cancelled_by: projection.cancelled_by,
            rejected_by: projection.rejected_by,
            created_at: projection.created_at,
            expires_at: projection.expires_at,
            resolved_at: projection.resolved_at,
            requested_comment: projection.requested_comment,
            approved_comment: projection.approved_comment,
            cancelled_comment: projection.cancelled_comment,
            rejected_comment: projection.rejected_comment,
        }
    }
}

#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;

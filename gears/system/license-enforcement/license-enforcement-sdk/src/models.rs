//! The grant model (DESIGN §3.1).
//!
//! Request types reject unknown fields, so a misspelled field fails instead of
//! being dropped. Response types accept unknown fields, and their enums that
//! can grow read an unknown value as `Unknown`, so an addition does not break
//! older clients. A shape that both carry has one type per side: `ResourceRef`
//! in a [`Grant`], [`ResourceRefInput`] in [`GrantTerms`].

use gts::GtsTypeId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::{DelegationId, EvidenceId, GrantId, PackId};

/// A licensed resource: a Resource contract type and an optional instance id.
///
/// Without `id` the grant covers every instance of the type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResourceRef {
    /// Resource contract type, derived from `gts.cf.core.lic.res.v1~`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub gts_type: GtsTypeId,
    /// Instance id; absent for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// A concrete subject: a Subject contract type and an instance id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SubjectRef {
    /// Subject contract type, derived from `gts.cf.core.lic.subj.v1~`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub gts_type: GtsTypeId,
    /// Subject instance id.
    pub id: String,
}

/// Request form of [`ResourceRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceRefInput {
    /// Resource contract type, derived from `gts.cf.core.lic.res.v1~`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub gts_type: GtsTypeId,
    /// Instance id; absent for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl From<ResourceRefInput> for ResourceRef {
    fn from(r: ResourceRefInput) -> Self {
        Self {
            gts_type: r.gts_type,
            id: r.id,
        }
    }
}

/// Request form of [`SubjectRef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubjectRefInput {
    /// Subject contract type, derived from `gts.cf.core.lic.subj.v1~`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub gts_type: GtsTypeId,
    /// Subject instance id.
    pub id: String,
}

impl From<SubjectRefInput> for SubjectRef {
    fn from(s: SubjectRefInput) -> Self {
        Self {
            gts_type: s.gts_type,
            id: s.id,
        }
    }
}

/// The binding slot of a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Binding {
    /// Unbound.
    None,
    /// Bound to a subject.
    Subject {
        /// The bound subject.
        subject: SubjectRef,
    },
    /// Bound to the live sub-grant that delegation created from this grant.
    Delegated {
        /// The sub-grant.
        grant_id: GrantId,
    },
    /// A kind that this version of the SDK does not know. The server never
    /// sends it.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// A validity window. `grace_until`, when present, ends a grace period that
/// starts at `expires_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Window {
    /// Start of validity, inclusive.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub not_before: OffsetDateTime,
    /// End of validity, exclusive.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: OffsetDateTime,
    /// End of the grace period, exclusive.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub grace_until: Option<OffsetDateTime>,
}

/// Request form of [`Window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowInput {
    /// Start of validity, inclusive.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub not_before: OffsetDateTime,
    /// End of validity, exclusive.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: OffsetDateTime,
    /// End of the grace period, exclusive.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub grace_until: Option<OffsetDateTime>,
}

impl From<WindowInput> for Window {
    fn from(w: WindowInput) -> Self {
        Self {
            not_before: w.not_before,
            expires_at: w.expires_at,
            grace_until: w.grace_until,
        }
    }
}

/// Validity of a grant: its own window, or the window of its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Validity {
    /// Own window.
    Own {
        /// The window.
        window: Window,
    },
    /// The window of the parent (sub-grants only). The gear copies it at
    /// `delegate` and keeps it equal at `renew` and supersession.
    Inherited {
        /// The effective window of the parent.
        window: Window,
    },
}

impl Validity {
    /// The effective window.
    #[must_use]
    pub fn window(&self) -> &Window {
        match self {
            Self::Own { window } | Self::Inherited { window } => window,
        }
    }
}

/// Holder permissions of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Permissions {
    /// The holder may `bind` and `unbind`.
    pub may_bind: bool,
    /// Maximum delegation depth below this grant; 0 means not delegable.
    pub max_depth: u32,
    /// The grant awaits the acceptance of its holder when it is created.
    pub requires_acceptance: bool,
}

/// Request form of [`Permissions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionsInput {
    /// The holder may `bind` and `unbind`.
    pub may_bind: bool,
    /// Maximum delegation depth below this grant; 0 means not delegable.
    pub max_depth: u32,
    /// The grant awaits the acceptance of its holder when it is created.
    pub requires_acceptance: bool,
}

impl From<PermissionsInput> for Permissions {
    fn from(p: PermissionsInput) -> Self {
        Self {
            may_bind: p.may_bind,
            max_depth: p.max_depth,
            requires_acceptance: p.requires_acceptance,
        }
    }
}

/// Holder permissions of a sub-grant that the grantor sets at `delegate`. The
/// gear sets `max_depth` to one less than the parent's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegatedPermissions {
    /// The holder may `bind` and `unbind`. Not wider than the parent.
    pub may_bind: bool,
    /// The sub-grant awaits the acceptance of its holder.
    pub requires_acceptance: bool,
}

/// Stored lifecycle state of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GrantState {
    /// Awaits the acceptance of its holder.
    AwaitingAcceptance,
    /// Active.
    Active,
    /// Suspended by the vendor.
    Suspended,
    /// Terminated; no verb reverses it.
    Terminated,
}

/// Why a grant was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminationReason {
    /// Revoked by the vendor.
    Revoked,
    /// Recalled by the grantor.
    Recalled,
    /// Returned by the holder.
    Returned,
    /// Rejected by the holder.
    Rejected,
    /// Replaced by a successor.
    Replaced,
    /// A reason that this version of the SDK does not know. The server never
    /// sends it.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Conditions over the subject and resource `metadata` of a check, or recipient
/// rules over the recipient projection.
///
/// Opaque to the SDK: the linked evaluator defines the form, validates it at
/// write time, and evaluates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(transparent)]
#[schema(value_type = Object)]
pub struct Conditions(pub serde_json::Value);

/// Recipient rules use the vocabulary of [`Conditions`].
pub type RecipientRules = Conditions;

/// The terms of a root grant that the vendor sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantTerms {
    /// The licensed resource.
    pub resource: ResourceRefInput,
    /// Subject contract type that the grant covers, or the base of the bound
    /// subject's type.
    #[schema(value_type = String)]
    pub subject_type: GtsTypeId,
    /// A subject fixed at admission. It never changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_subject: Option<SubjectRefInput>,
    /// Validity window.
    pub window: WindowInput,
    /// Conditions over the check `metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    /// Holder permissions.
    pub permissions: PermissionsInput,
    /// Recipient rules evaluated at `delegate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_rules: Option<RecipientRules>,
}

/// A grant: a root or a sub-grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Grant {
    /// Grant id.
    pub id: GrantId,
    /// Tenant that holds the grant.
    pub holder_tenant_id: Uuid,
    /// The licensed resource.
    pub resource: ResourceRef,
    /// Subject contract type that the grant covers.
    #[schema(value_type = String)]
    pub subject_type: GtsTypeId,
    /// The binding slot.
    pub binding: Binding,
    /// The binding was fixed at admission and never changes.
    pub binding_fixed: bool,
    /// Validity.
    pub validity: Validity,
    /// Conditions over the check `metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    /// Holder permissions.
    pub permissions: Permissions,
    /// Recipient rules evaluated at `delegate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_rules: Option<RecipientRules>,
    /// Parent grant; absent for a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_grant_id: Option<GrantId>,
    /// Delegation that created this sub-grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<DelegationId>,
    /// Pack of the grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_id: Option<PackId>,
    /// Evidence of the root.
    pub evidence_id: EvidenceId,
    /// Stored lifecycle state.
    pub state: GrantState,
    /// Set when `state` is terminated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<TerminationReason>,
    /// Increases at every change.
    pub version: u64,
}

/// Subject-grant counts of one resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BindingCounts {
    /// The licensed resource.
    pub resource: ResourceRef,
    /// Live grants without a binding.
    pub unbound: u64,
    /// Live grants bound to a subject.
    pub bound: u64,
    /// Live grants bound to a sub-grant.
    pub delegated: u64,
}

/// A set of grants issued together. Its members never change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Pack {
    /// Pack id.
    pub id: PackId,
    /// Tenant that holds the pack.
    pub holder_tenant_id: Uuid,
    /// Subject-grant counts per resource.
    pub counts: Vec<BindingCounts>,
    /// Increases at every change.
    pub version: u64,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;

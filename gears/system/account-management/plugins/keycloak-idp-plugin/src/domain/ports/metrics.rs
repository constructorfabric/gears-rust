//! `keycloak-idp-plugin` observability ports — typed, segregated metric-emission
//! traits. Mirrors the AM pattern
//! (`gears/system/account-management/account-management/src/domain/ports/metrics.rs`).
//!
//! Each trait owns one cohesive subdomain of the plugin metric catalog
//! declared in [`crate::domain::metrics`]. A single infra adapter
//! ([`crate::infra::metrics::KeycloakIdpPluginMetricsAdapter`]) implements every
//! trait on one OpenTelemetry-backed struct; DI hands each facade the
//! trait(s) it actually needs.
//!
//! ## Design choices
//!
//! * **Trait segregation.** Seven narrow traits instead of one fat trait —
//!   each facade depends on the minimum surface it needs.
//! * **Typed label values.** Every `&str` label that was previously
//!   passed as a literal becomes a typed enum or a sealed newtype with
//!   `pub const` literals. The compiler enforces the closed set; no typo
//!   can leak into a dashboard.
//! * **Bridging existing label helpers.** Sets that mirror existing
//!   per-variant string mappings (`failure_variant_label`,
//!   `version_observed_label`) are modelled as sealed newtypes with
//!   `From<&PluginError>` / `From<&DecodeError>` impls — the mapping
//!   continues to live on the failure type and is not duplicated here.
//! * **Cardinality discipline.** Free `&str` realm-name arguments are
//!   accepted only on ports that need them (`realm_bound`,
//!   `kc_admin_token_refresh`, `credential_refresh`); the adapter
//!   decides whether to emit the label based on the cardinality
//!   watchdog. Call sites never branch on cardinality.

use std::borrow::Cow;

use toolkit_macros::domain_model;

use crate::domain::error::{PluginError, failure_variant_label};
use crate::domain::metadata_codec::{DecodeError, RealmBinding, version_observed_label};

// ════════════════════════════════════════════════════════════════════
//  Closed-set label enums
// ════════════════════════════════════════════════════════════════════

/// `op` label on `keycloak_idp_plugin_user_op_duration_seconds`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOp {
    ProvisionUser,
    DeprovisionUser,
    UpdateUser,
    ListUsers,
}

impl UserOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvisionUser => "provision_user",
            Self::DeprovisionUser => "deprovision_user",
            Self::UpdateUser => "update_user",
            Self::ListUsers => "list_users",
        }
    }
}

/// `tier` label on `keycloak_idp_plugin_kc_admin_token_refresh_total` and
/// `keycloak_idp_plugin_credential_refresh_total` (DESIGN "Observability").
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenTier {
    StaticEnv,
    OpenBao,
}

impl TokenTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaticEnv => "static_env",
            Self::OpenBao => "openbao",
        }
    }
}

/// `outcome` label on `keycloak_idp_plugin_kc_admin_token_refresh_total` and
/// `keycloak_idp_plugin_credential_refresh_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRefreshOutcome {
    Success,
    Error,
}

impl TokenRefreshOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// `op` label on `keycloak_idp_plugin_sa_op_duration_seconds`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaOp {
    Create,
    RotateSecret,
    Revoke,
    List,
    Purge,
}

impl SaOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "sa_create",
            Self::RotateSecret => "sa_rotate_secret",
            Self::Revoke => "sa_revoke",
            Self::List => "sa_list",
            Self::Purge => "sa_purge",
        }
    }
}

/// `op` label on `keycloak_idp_plugin_credstore_write_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredstoreOp {
    Put,
    Delete,
}

impl CredstoreOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Delete => "delete",
        }
    }
}

/// `outcome` label on `keycloak_idp_plugin_credstore_write_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredstoreOutcome {
    Ok,
    Error,
}

impl CredstoreOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// `op` label on `keycloak_idp_plugin_failure_total`. Superset of all plugin-level
/// operations: tenant lifecycle + user lifecycle + service-account ops.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOp {
    ProvisionTenant,
    DeprovisionTenant,
    ProvisionUser,
    DeprovisionUser,
    UpdateUser,
    ListUsers,
    SaCreate,
    SaRotateSecret,
    SaRevoke,
    SaList,
    SaPurge,
}

impl PluginOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvisionTenant => "provision_tenant",
            Self::DeprovisionTenant => "deprovision_tenant",
            Self::ProvisionUser => "provision_user",
            Self::DeprovisionUser => "deprovision_user",
            Self::UpdateUser => "update_user",
            Self::ListUsers => "list_users",
            Self::SaCreate => "sa_create",
            Self::SaRotateSecret => "sa_rotate_secret",
            Self::SaRevoke => "sa_revoke",
            Self::SaList => "sa_list",
            Self::SaPurge => "sa_purge",
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Sealed-newtype label bridges
// ════════════════════════════════════════════════════════════════════

// ── keycloak_idp_plugin_failure_total ───────────────────────────────────────────

/// `failure_variant` label on `keycloak_idp_plugin_failure_total`.
///
/// Sealed newtype: the closed set of literals lives as `pub const`
/// associated constants, and `From<&PluginError>` bridges
/// [`failure_variant_label`] which owns the variant→string mapping. No
/// public constructor — values must come from a constant or the `From`
/// impl, so the cardinality surface stays closed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureVariant(&'static str);

impl FailureVariant {
    pub const CONFIG: Self = Self("config");
    pub const CREDSTORE_READ: Self = Self("credstore_read");
    pub const METADATA_DECODE: Self = Self("metadata_decode");
    pub const KC_REST: Self = Self("kc_rest");
    pub const AMBIGUOUS_CREATED: Self = Self("ambiguous_created");
    pub const CREATED_REALM_EXISTS: Self = Self("created_realm_exists");
    pub const BOOTSTRAP_PERMS_MISSING: Self = Self("bootstrap_perms_missing");
    pub const DEPROVISION_NOT_FOUND: Self = Self("deprovision_not_found");
    pub const DEPROVISION_RETRYABLE: Self = Self("deprovision_retryable");
    pub const DEPROVISION_TERMINAL: Self = Self("deprovision_terminal");
    pub const USER_OP_REJECTED: Self = Self("user_op_rejected");
    pub const USER_OP_UNAVAILABLE: Self = Self("user_op_unavailable");
    pub const USER_OP_UNSUPPORTED: Self = Self("user_op_unsupported");
    pub const USER_OP_NOT_FOUND: Self = Self("user_op_not_found");
    pub const USER_OP_FIELD_NOT_WRITABLE: Self = Self("user_op_field_not_writable");
    pub const SA_INVALID_INPUT: Self = Self("sa_invalid_input");
    pub const SA_NOT_FOUND: Self = Self("sa_not_found");

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl From<&PluginError> for FailureVariant {
    fn from(e: &PluginError) -> Self {
        Self(failure_variant_label(e))
    }
}

// ── keycloak_idp_plugin_metadata_decode_failure_total ───────────────────────────

/// `version_observed` label on `keycloak_idp_plugin_metadata_decode_failure_total`.
///
/// Sealed newtype around `Cow<'static, str>`: static literals for the
/// fixed `MissingVersion` / `Malformed` paths; `Cow::Owned` for the
/// `UnsupportedVersion { observed }` wildcard. Cardinality of the
/// wildcard branch is bounded by the realistic surface of KC realm
/// metadata-version strings (a tiny set in practice).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionObserved(Cow<'static, str>);

impl VersionObserved {
    pub const MISSING: Self = Self(Cow::Borrowed("missing"));
    pub const MALFORMED: Self = Self(Cow::Borrowed("malformed"));

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&DecodeError> for VersionObserved {
    fn from(e: &DecodeError) -> Self {
        Self(version_observed_label(e))
    }
}

// ── keycloak_idp_plugin_kc_admin_request_duration_seconds ─────────────────────────

/// `endpoint_class` label on `keycloak_idp_plugin_kc_admin_request_duration_seconds`.
///
/// Sealed newtype reserved for the KC HTTP layer wiring (a follow-up
/// PR). The literal set is intentionally left at one
/// placeholder; new endpoint classes will be added alongside the
/// emitting call sites.
///
/// **TODO (follow-up — KC HTTP layer)**: when wiring real
/// emitters in `infra/kc_http.rs`, extend this sealed newtype with one
/// `pub const` per endpoint family (e.g. `REALM_GET`, `USERS_LIST`,
/// `GROUPS_PUT`, …). Do **not** introduce a free-`&str` constructor —
/// the sealed shape is what keeps the cardinality surface closed.
/// Mirror the bridge pattern from [`FailureVariant`] if a fast
/// `From<&PathTemplate>` mapping is needed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointClass(&'static str);

impl EndpointClass {
    pub const UNKNOWN: Self = Self("unknown");

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

// ════════════════════════════════════════════════════════════════════
//  Port traits — one per subdomain
// ════════════════════════════════════════════════════════════════════

/// Tenant-lifecycle telemetry — `provision_tenant` latency,
/// realm-binding count, deprovision-missing-metadata trigger.
pub trait TenantLifecycleMetricsPort: Send + Sync + 'static {
    fn provision_tenant_duration(&self, realm_binding: RealmBinding, secs: f64);
    fn realm_bound(&self, realm_binding: RealmBinding, realm_name: &str);
    fn realm_unbound(&self, realm_binding: RealmBinding, realm_name: &str);
    fn deprovision_missing_metadata(&self);
}

/// `outcome` label on `keycloak_idp_plugin_orphan_user_compensation_total`. Two
/// terminal states for the best-effort `delete_user` compensation
/// path; both fire from `UserFacade::provision_user_impl` when
/// `add_user_to_group` fails after `create_user` already landed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanCompensationOutcome {
    Ok,
    Failed,
}

impl OrphanCompensationOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
        }
    }
}

/// User-op telemetry — `provision_user` / `deprovision_user` /
/// `list_users` latency + orphan-user compensation counter.
pub trait UserOpMetricsPort: Send + Sync + 'static {
    fn user_op_duration(&self, op: UserOp, secs: f64);
    /// Increment the orphan-compensation counter labelled by `outcome`.
    /// Dashboards alert on `outcome=failed` — a KC user dangling
    /// without tenant binding is the actionable failure mode that
    /// justifies the compensation path's existence.
    fn orphan_user_compensation(&self, outcome: OrphanCompensationOutcome);
}

/// KC Admin telemetry — single-call latency + token / credential
/// refresh outcomes.
///
/// [`Self::kc_admin_token_refresh`] and [`Self::credential_refresh`] are
/// live: both are emitted from the token-acquisition slow path in
/// [`crate::domain::kc::factory`]. [`Self::kc_admin_request_duration`] is
/// the one **reserved** method — declared without emitters so the
/// follow-up PR wiring the KC HTTP layer does not need to reshape DI.
pub trait KcAdminMetricsPort: Send + Sync + 'static {
    fn kc_admin_request_duration(&self, endpoint_class: EndpointClass, secs: f64);
    fn kc_admin_token_refresh(&self, outcome: TokenRefreshOutcome, tier: TokenTier, realm: &str);
    fn credential_refresh(&self, outcome: TokenRefreshOutcome, tier: TokenTier, realm: &str);
}

/// `CredStore` (`OpenBao`) write telemetry.
pub trait CredstoreMetricsPort: Send + Sync + 'static {
    fn credstore_write(&self, op: CredstoreOp, outcome: CredstoreOutcome);
}

/// Tenant metadata-codec telemetry — malformed / unsupported-version
/// blob counter.
pub trait MetadataCodecMetricsPort: Send + Sync + 'static {
    fn metadata_decode_failure(&self, version_observed: VersionObserved);
}

/// Cross-cutting failure telemetry — every plugin operation can fail
/// and the `(op, failure_variant)` tuple is the stable dimension
/// dashboards key on.
pub trait FailureMetricsPort: Send + Sync + 'static {
    fn failure(&self, op: PluginOp, variant: FailureVariant);
}

/// Service-account op telemetry — create / `rotate_secret` / revoke / list latency.
pub trait SaOpMetricsPort: Send + Sync + 'static {
    fn sa_op_duration(&self, op: SaOp, secs: f64);
}

// ════════════════════════════════════════════════════════════════════
//  No-op default implementation
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;

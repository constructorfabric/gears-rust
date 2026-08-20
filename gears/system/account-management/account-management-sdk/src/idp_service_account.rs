//! `IdP` service-account DTOs.
//!
//! Sibling of [`crate::idp_user`] — hosts the request / response /
//! failure shapes consumed by the service-account half of
//! [`crate::idp::IdpPluginClient`]. The trait itself lives in
//! [`crate::idp`] so a single `Arc<dyn IdpPluginClient>` carries the
//! tenant, user, and service-account methods.
//!
//! A service account is a tenant-scoped **machine identity**: a
//! confidential OAuth client that authenticates with the
//! `client_credentials` grant and carries the owning `tenant_id` plus a
//! service-subject `user_type` in its tokens. Like users (per
//! `cpt-cf-account-management-constraint-no-user-storage`) accounts
//! are NOT modelled in AM storage — every call is a live pass-through
//! to the resolved provider plugin, so a gear restart loses nothing.
//!
//! # Addressing and the `name` correlation key
//!
//! `(tenant_id, client_id)` is the scoped resource address; an address
//! that does not resolve within the tenant yields
//! [`IdpServiceAccountFailure::NotFound`]. Client-id **formats** are
//! adapter conventions, never contract: an `IdP` that assigns opaque
//! client ids conforms. The bridge from a caller-chosen name to the
//! adapter-assigned id is [`IdpServiceAccountSummary::name`], which
//! providers report verbatim — see the uniqueness obligation on
//! [`crate::idp::IdpPluginClient::provision_service_account`].
//!
//! # Failure model and the untrusted-text posture
//!
//! [`IdpServiceAccountFailure`] discriminates the categories AM's
//! service layer maps onto the public error envelope: a rejection with
//! no provider state retained, an absent target, a clean upstream
//! failure, and transport uncertainty that may have retained state.
//!
//! Unlike the tenant / user halves of the contract — where AM forwards
//! a redacted digest of the provider `detail` — the service-account
//! boundary **discards provider text entirely** and answers each
//! category with a fixed, AM-owned message. Character filtering was
//! tried and removed: a credential such as `secret=abc123` is ordinary
//! ASCII and no filter or length cap can tell it apart from operator
//! prose, so any relay launders a leak instead of preventing one. The
//! consequence for implementors: **an adapter MUST log its own
//! diagnostics in-process**, at the point of failure, where it knows
//! which parts of a vendor error are safe to record. Text placed in
//! `detail` is used by AM only for its length; this type's `Display` emits
//! the category label without the text so a future `%failure` log remains
//! safe. A failure whose cause is only described in `detail` is therefore a
//! failure no operator can diagnose unless the adapter logs it safely.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::idp_user::IdpTenantContext;

/// Request shape for
/// [`crate::idp::IdpPluginClient::provision_service_account`].
///
/// `tenant_context.tenant_id` is the tenant that will own the
/// account and whose id lands in the issued tokens' `tenant_id`
/// claim; AM has already validated the scope is `Active` before
/// invoking the contract. There is intentionally no separate
/// `tenant_id` field — see [`crate::idp_user::IdpProvisionUserRequest`]
/// for the duplication-removal rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdpProvisionServiceAccountRequest {
    /// Resolved tenant context (id, name, type, opaque plugin metadata).
    pub tenant_context: IdpTenantContext,
    /// Short caller-chosen name, unique among the tenant's live
    /// accounts. Adapters commonly derive the client id as
    /// `svc-<tenant_id>-<name>`, but that format is a convention, not a
    /// contract; the caller correlates a name to its account through
    /// [`IdpServiceAccountSummary::name`], never by parsing a client
    /// id. Structural rules (charset, length) are the adapter's to
    /// enforce and are reported as
    /// [`IdpServiceAccountFailure::InvalidInput`].
    pub name: String,
    /// Client scopes to attach; validated against the adapter allowlist.
    pub scopes: Vec<String>,
}

impl IdpProvisionServiceAccountRequest {
    #[must_use]
    pub const fn new(tenant_context: IdpTenantContext, name: String, scopes: Vec<String>) -> Self {
        Self {
            tenant_context,
            name,
            scopes,
        }
    }
}

/// Request shape for
/// [`crate::idp::IdpPluginClient::rotate_service_account_secret`].
///
/// `(tenant_context.tenant_id, client_id)` is the scoped address: a
/// `client_id` that is not owned by the tenant MUST be reported as
/// [`IdpServiceAccountFailure::NotFound`], never rotated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdpRotateServiceAccountSecretRequest {
    pub tenant_context: IdpTenantContext,
    pub client_id: String,
}

impl IdpRotateServiceAccountSecretRequest {
    #[must_use]
    pub const fn new(tenant_context: IdpTenantContext, client_id: String) -> Self {
        Self {
            tenant_context,
            client_id,
        }
    }
}

/// Request shape for
/// [`crate::idp::IdpPluginClient::revoke_service_account`].
///
/// Same scoped-address rule as
/// [`IdpRotateServiceAccountSecretRequest`]; AM treats a `NotFound`
/// as success-equivalent so `DELETE` stays retry-safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdpRevokeServiceAccountRequest {
    pub tenant_context: IdpTenantContext,
    pub client_id: String,
}

impl IdpRevokeServiceAccountRequest {
    #[must_use]
    pub const fn new(tenant_context: IdpTenantContext, client_id: String) -> Self {
        Self {
            tenant_context,
            client_id,
        }
    }
}

/// Request shape for
/// [`crate::idp::IdpPluginClient::list_service_accounts`].
///
/// Unpaginated by contract: the listing returns the tenant's whole
/// collection. Provider-side quotas bound a tenant to a handful of
/// accounts, and the listing is the reconciliation path for an
/// [`IdpServiceAccountFailure::Ambiguous`] create — a paginated
/// answer would make "is my name already live here?" a multi-round-trip
/// question with a cursor to invalidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdpListServiceAccountsRequest {
    pub tenant_context: IdpTenantContext,
}

impl IdpListServiceAccountsRequest {
    #[must_use]
    pub const fn new(tenant_context: IdpTenantContext) -> Self {
        Self { tenant_context }
    }
}

/// Live credentials for a service account. The secret is returned
/// ONLY by provision / rotate — the caller persists it immediately
/// (credstore); recovery from loss is a rotate, not a read.
///
/// `client_secret` is a [`secrecy::SecretString`] (workspace MUST-wrap
/// rule for secret-shaped strings): redacted `Debug`, zeroize-on-drop,
/// and not serializable by default — which is also why this type, unlike
/// its request siblings, derives no `Serialize` / `Deserialize`.
#[derive(Debug)]
#[non_exhaustive]
pub struct IdpServiceAccountCredentials {
    /// The adapter-assigned client id, used to address rotate and
    /// revoke. Opaque to callers.
    pub client_id: String,
    /// The `client_credentials` secret.
    pub client_secret: SecretString,
    /// OAuth token endpoint for the `client_credentials` grant.
    pub token_url: String,
    /// The account's subject id — the `sub` claim of issued tokens
    /// (the IdP-side service-account user UUID). Use it for RBAC
    /// bindings.
    pub subject_id: Uuid,
}

impl IdpServiceAccountCredentials {
    #[must_use]
    pub const fn new(
        client_id: String,
        client_secret: SecretString,
        token_url: String,
        subject_id: Uuid,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            token_url,
            subject_id,
        }
    }
}

/// Listing entry — no secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IdpServiceAccountSummary {
    /// The adapter-assigned client id.
    pub client_id: String,
    /// The caller-supplied
    /// [`IdpProvisionServiceAccountRequest::name`] this account was
    /// created with. Providers **MUST** report it verbatim
    /// (byte-identical to the submitted value) for every listed
    /// account.
    ///
    /// This is the only contractual bridge from a name to a `client_id`,
    /// and it is what makes ambiguous-create recovery
    /// adapter-independent: after an `Ambiguous` create the caller lists
    /// the tenant and matches on this field, without depending on any
    /// client-id format convention.
    ///
    /// The match is unique: because provisioning rejects a name already
    /// live in the tenant (see
    /// [`crate::idp::IdpPluginClient::provision_service_account`]), at
    /// most one live entry per tenant carries a given `name`. A caller
    /// that finds two matches is looking at a provider violating that
    /// obligation and must not guess which one to rotate or revoke.
    pub name: String,
    /// Whether the client can currently authenticate.
    pub enabled: bool,
    /// Attached client scopes as reported by the `IdP` — includes
    /// realm-default scopes, not only the requested ones.
    pub scopes: Vec<String>,
}

impl IdpServiceAccountSummary {
    #[must_use]
    pub const fn new(client_id: String, name: String, enabled: bool, scopes: Vec<String>) -> Self {
        Self {
            client_id,
            name,
            enabled,
            scopes,
        }
    }
}

/// Failure discriminant shared by every service-account contract
/// method.
///
/// AM's service layer maps each variant onto the canonical error
/// taxonomy in `cf-gears-account-management::domain::idp`. That boundary
/// answers each category with a fixed, AM-owned message and logs only
/// the category label plus the discarded text's length — never the
/// `detail` or `field` content itself (see the module-level note).
/// Plugin authors therefore do not redact `detail`; they log their own
/// diagnostics in-process instead.
///
/// `#[non_exhaustive]` for the same reason as the sibling `IdP` failure
/// enums: the SDK can classify a further category (say, a provider-side
/// quota refusal) in a minor release. AM's boundary maps an unknown
/// variant conservatively onto an internal error with a loud `error!`,
/// so a new variant surfaces as a mapping gap in operator logs rather
/// than as a silently mis-typed response.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdpServiceAccountFailure {
    /// Rejected with no provider state retained by this call: a name
    /// that violates the adapter's structural rules, a name already
    /// live in the tenant, a scope outside the allowlist, or a
    /// provider-side quota. Permanent — do not retry the same input.
    ///
    /// `field` names the offending request field for the adapter's own
    /// logs. Like `detail` it is untrusted text and does not reach the
    /// caller: AM attributes the violation to the request as a whole.
    InvalidInput {
        detail: String,
        field: Option<String>,
    },
    /// The addressed account is absent on the provider side (404 /
    /// 410) or not owned by the addressed tenant. Success-equivalent
    /// for revoke; a `404` for rotate.
    NotFound { detail: String },
    /// The provider call failed cleanly — no state retained. Retrying
    /// is harmless, though not necessarily productive.
    CleanFailure { detail: String },
    /// Transport uncertainty: the provider may have retained state.
    /// MUST NOT be reported as success. AM surfaces this as `409` with
    /// a message steering the caller into reconciliation (list the
    /// tenant, match the submitted name) rather than the blind retry a
    /// `503` would invite.
    Ambiguous { detail: String },
    /// The provider does not implement service-account management at
    /// all. This is the default-impl return for adapters that ship only
    /// the tenant / user halves of the contract; AM surfaces `501`.
    /// Providers MUST NOT silently no-op a mutating call.
    UnsupportedOperation { detail: String },
}

impl IdpServiceAccountFailure {
    /// Stable, snake-case metric-label form of this variant. Kept on
    /// the SDK type so producers do not duplicate the variant → string
    /// mapping in match arms.
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::InvalidInput { .. } => "invalid_input",
            Self::NotFound { .. } => "not_found",
            Self::CleanFailure { .. } => "clean_failure",
            Self::Ambiguous { .. } => "ambiguous",
            Self::UnsupportedOperation { .. } => "unsupported_operation",
        }
    }

    /// The adapter-supplied diagnostic carried by every variant.
    /// Untrusted text: consumers inside AM must not place it in a
    /// response body or a log field (see the module-level note).
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::InvalidInput { detail, .. }
            | Self::NotFound { detail }
            | Self::CleanFailure { detail }
            | Self::Ambiguous { detail }
            | Self::UnsupportedOperation { detail } => detail,
        }
    }
}

impl core::fmt::Debug for IdpServiceAccountFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `detail` and `field` are both untrusted adapter text. Keep `?failure`,
        // panic, unwrap, and assertion formatting as safe as `%failure`.
        f.write_str(self.as_metric_label())
    }
}

impl core::fmt::Display for IdpServiceAccountFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Adapter detail is untrusted and may contain a freshly minted
        // credential. Keep `%failure` logging safe by exposing only the stable
        // category label; adapters that own the detail log safe diagnostics
        // in-process before returning the failure.
        f.write_str(self.as_metric_label())
    }
}

impl core::error::Error for IdpServiceAccountFailure {}

#[cfg(test)]
#[path = "idp_service_account_tests.rs"]
mod tests;

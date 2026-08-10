//! Plugin root struct + [`IdpPluginClient`] boundary.
//!
//! This module owns the translation between the plugin-internal
//! [`PluginError`] taxonomy (rich; one variant per failure shape) and the
//! typed SDK failure variants per DESIGN §5.5. Every facade call returns a
//! `Result<_, PluginError>`; every boundary method below funnels the failure
//! through a `translate_*_failure` helper before handing it back to AM.
//!
//! See:
//! * DESIGN §4.0 (Plugin Root + Gear Lifecycle).
//! * DESIGN §5.1 (Method mapping).
//! * DESIGN §5.5 (error mapping table).
//! * ADDENDUM §4.4 (Plugin root struct fields).

use std::sync::Arc;

use account_management_sdk::idp::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpPluginClient, IdpProvisionFailure,
    IdpProvisionResult, IdpProvisionTenantRequest,
};
use account_management_sdk::idp_user::{
    IdpDeprovisionUserRequest, IdpListUsersRequest, IdpProvisionUserRequest, IdpUser,
    IdpUserOperationFailure,
};
use async_trait::async_trait;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;

use crate::domain::error::{KcStatusKind, PluginError};
use crate::domain::metadata_codec::MetadataCodec;
use crate::domain::service_principal_facade::ServicePrincipalFacade;
use crate::domain::tenant_facade::TenantFacade;
use crate::domain::user_facade::UserFacade;

/// Plugin root struct. Holds [`Arc`]-shared facades + codec.
///
/// Per ADDENDUM §4.4, the root carries the two facades and the metadata
/// codec — every other resource (Keycloak factory, `CredStore` reader /
/// writer, metrics) is closed over by the facades themselves. The
/// `Arc<dyn IdpPluginClient>` registered in `ClientHub` is the single
/// entry-point AM consumes; the facades remain plugin-private.
pub struct KeycloakIdpPlugin {
    pub tenant_facade: Arc<TenantFacade>,
    pub user_facade: Arc<UserFacade>,
    pub codec: Arc<MetadataCodec>,
    pub sp_facade: Arc<ServicePrincipalFacade>,
}

#[async_trait]
impl IdpPluginClient for KeycloakIdpPlugin {
    async fn provision_tenant(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        match self.tenant_facade.provision_tenant_inner(ctx, req).await {
            Ok(meta) => Ok(IdpProvisionResult::new(Some(self.codec.encode(&meta)))),
            Err(e) => Err(translate_provision_failure(e)),
        }
    }

    async fn deprovision_tenant(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), IdpDeprovisionFailure> {
        self.tenant_facade
            .deprovision_tenant_inner(ctx, req)
            .await
            .map_err(translate_deprovision_failure)
    }

    async fn provision_user(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        self.user_facade
            .provision_user_inner(ctx, req)
            .await
            .map_err(translate_user_op_failure)
    }

    async fn deprovision_user(
        &self,
        ctx: &SecurityContext,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        self.user_facade
            .deprovision_user_inner(ctx, req)
            .await
            .map_err(translate_user_op_failure)
    }

    async fn list_users(
        &self,
        ctx: &SecurityContext,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        self.user_facade
            .list_users_inner(ctx, req)
            .await
            .map_err(translate_user_op_failure)
    }
}

/// Translate a [`PluginError`] surfaced by
/// `TenantFacade::provision_tenant_inner` into the typed SDK failure variant
/// per DESIGN §5.5.
///
/// * `MetadataDecode` → [`IdpProvisionFailure::InvalidInput`] — the
///   caller's `provisioning_metadata` failed to deserialize before any
///   KC call. AM surfaces this as `CanonicalError::InvalidArgument`
///   (HTTP 400) so the caller sees "fix your request" rather than the
///   503 retry shape that `CleanFailure` would produce. SAFE to
///   classify as caller-input because this variant is only emitted on
///   the deprovision path's `parse_input` and never after KC state
///   has been touched.
/// * `Config` → [`IdpProvisionFailure::CleanFailure`] — this variant is
///   too broad to classify as caller-input. It IS emitted by
///   `parse_input` for shape-bad metadata (which IS caller-input), BUT
///   also by post-KC-creation paths (the `SecretRef::new(...)` parses
///   in `TenantFacade::provision_created` / `provision_shared` run
///   AFTER realm + admin-client + role-grants exist in KC, and a
///   `Config` produced there means real vendor state has been
///   created). Classifying THOSE as `InvalidInput` would tell AM to
///   compensate the provisioning row and return 400 to the caller —
///   the reaper would never run, KC state would leak. Keep the
///   `CleanFailure` mapping (the original behaviour) and lose the
///   400-on-bad-metadata signal on the few caller-input subpaths;
///   that's better than the worse failure mode.
/// * 4xx KC responses + `CreatedRealmExists` →
///   [`IdpProvisionFailure::CleanFailure`] (AM proves no `IdP`-side state
///   retained; the `IdP` did reject but vendor state is clean → 503).
/// * `BootstrapPermsMissing` → [`IdpProvisionFailure::UnsupportedOperation`]
///   (operator must intervene on the `IaC` checklist).
/// * 5xx KC + transport / timeout + `AmbiguousCreated` + `CredStoreWrite`
///   (post-KC) → [`IdpProvisionFailure::Ambiguous`] (provisioning reaper
///   compensates asynchronously).
fn translate_provision_failure(e: PluginError) -> IdpProvisionFailure {
    // Plugin-side observability: emit a stage-labelled WARN at the translation
    // boundary BEFORE the AM canonical-error layer redacts vendor detail to a
    // FNV-1a digest. The stage label is plugin-internal (an `AmbiguousStage`
    // variant string, not vendor text), so this leaks no upstream payload —
    // it just gives operators a direct "which saga step failed" signal in the
    // long-retention plugin log without having to bump RUST_LOG to debug.
    // Keeps the AM-side `am.idp` redacted line as the canonical correlation
    // anchor (digest + len); this is a *complementary* breadcrumb upstream of it.
    if let PluginError::AmbiguousCreated { stage, .. } = &e {
        tracing::warn!(
            target: "vp_idp_plugin.provision",
            failure_variant = "ambiguous_created",
            stage = %stage,
            "Created-mode saga failed with Ambiguous outcome — operator must reconcile via reaper"
        );
    }
    match e {
        // ---- InvalidInput — caller-supplied `provisioning_metadata`
        //      failed deserialization BEFORE any KC call. Only emitted
        //      on the deprovision path's `parse_input`; never on the
        //      provision saga (which surfaces `Config`, asserted by a
        //      debug_assert in `TenantFacade::provision_tenant_saga`).
        //      Safe to surface as 400 invalid_argument.
        PluginError::MetadataDecode(_) => IdpProvisionFailure::InvalidInput {
            detail: e.to_string(),
            field: Some("provisioning_metadata".into()),
        },

        // ---- InvalidInput — caller-supplied `provisioning_metadata`
        //      rejected by `parse_input` BEFORE any KC call (VHP-1836:
        //      created+explicit-realm, shared with no parent metadata to
        //      inherit, bad mode/realm shape). Distinct from `Config`
        //      (which is also emitted post-KC), so this dedicated pre-KC
        //      variant is safe to surface as a clean 400.
        PluginError::ProvisionInputRejected { ref detail } => IdpProvisionFailure::InvalidInput {
            detail: detail.clone(),
            field: Some("provisioning_metadata".into()),
        },

        // ---- CleanFailure — IdP rejected (or pre-saga config check
        //      tripped) but no IdP-side side effect retained. NOTE:
        //      `Config` is intentionally NOT classified as
        //      `InvalidInput` even though some of its emit sites
        //      (notably `parse_input` shape checks) ARE caller-input
        //      problems. The `Config` variant is also emitted by post-
        //      KC-creation paths (`SecretRef::new(...)` parses in
        //      `provision_created`/`provision_shared` after realm +
        //      admin-client exist in KC); classifying those as
        //      `InvalidInput` would tell AM to compensate the row and
        //      return 400 to the caller, leaving real KC state behind
        //      with no reaper to clean it up. The conservative
        //      `CleanFailure` mapping loses the precise 400 signal on
        //      the parse_input subpath but preserves the
        //      KC-state-doesn't-leak invariant on the post-saga subpaths.
        PluginError::Config { ref detail } => IdpProvisionFailure::CleanFailure {
            detail: detail.clone(),
        },
        PluginError::CreatedRealmExists { ref realm } => IdpProvisionFailure::CleanFailure {
            detail: format!("mode = created but realm '{realm}' already exists"),
        },
        // 4xx upstream → CleanFailure (request validated server-side without
        // retained state). 5xx falls through to the Ambiguous arm below.
        PluginError::KcRest {
            status: KcStatusKind::Http(s),
            ..
        } if (400..=499).contains(&s) => IdpProvisionFailure::CleanFailure {
            detail: e.to_string(),
        },

        // ---- UnsupportedOperation. ----
        PluginError::BootstrapPermsMissing { .. } => IdpProvisionFailure::UnsupportedOperation {
            detail: e.to_string(),
        },

        // ---- Ambiguous — 5xx, transport, timeout, or anything mid-saga. ----
        // The first three arms collapse into one body because they share the
        // same `detail` source (`e.to_string()`) and the same SDK variant —
        // clippy's `match_same_arms` lint would otherwise flag the duplication.
        PluginError::KcRest {
            status: KcStatusKind::Http(s),
            ..
        } if (500..=599).contains(&s) => IdpProvisionFailure::Ambiguous {
            detail: e.to_string(),
        },
        PluginError::KcRest {
            status: KcStatusKind::Transport | KcStatusKind::Timeout,
            ..
        }
        | PluginError::AmbiguousCreated { .. } => IdpProvisionFailure::Ambiguous {
            detail: e.to_string(),
        },

        // ---- CleanFailure fall-through for remaining internal-only errors
        //      that shouldn't normally surface on the provision path. ----
        PluginError::CredStoreRead { detail } => IdpProvisionFailure::CleanFailure { detail },

        // ---- Should never appear on the provision path; map conservatively. ----
        other => IdpProvisionFailure::CleanFailure {
            detail: other.to_string(),
        },
    }
}

/// Translate a [`PluginError`] surfaced by
/// `TenantFacade::deprovision_tenant_inner` into the typed SDK failure
/// variant per DESIGN §5.5 / §6.2.
///
/// * `DeprovisionNotFound` → [`IdpDeprovisionFailure::NotFound`] (AM treats
///   as success-equivalent).
/// * `DeprovisionRetryable` → [`IdpDeprovisionFailure::Retryable`]
///   (deferred to next retention tick).
/// * `DeprovisionTerminal` / `BootstrapPermsMissing` / `MetadataDecode` →
///   [`IdpDeprovisionFailure::Terminal`] (operator must intervene).
/// * Any other variant that leaks through is also mapped to `Terminal` so
///   the operational signal stays observable.
fn translate_deprovision_failure(e: PluginError) -> IdpDeprovisionFailure {
    match e {
        PluginError::DeprovisionNotFound { .. } => IdpDeprovisionFailure::NotFound {
            detail: e.to_string(),
        },
        PluginError::DeprovisionRetryable { .. } => IdpDeprovisionFailure::Retryable {
            detail: e.to_string(),
        },
        // `DeprovisionTerminal` + `BootstrapPermsMissing` + `MetadataDecode`
        // + every other unexpected leak all map to `Terminal` per the
        // operator-must-intervene contract; combined with the catch-all
        // below into one body so clippy's `match_same_arms` lint stays
        // satisfied without obscuring the intent.
        other => IdpDeprovisionFailure::Terminal {
            detail: other.to_string(),
        },
    }
}

/// Translate a [`PluginError`] surfaced by the user-side facade methods
/// into the typed SDK failure variant per DESIGN §5.5.
///
/// * `UserOpRejected` → [`IdpUserOperationFailure::Rejected`] (validation).
/// * `UserOpDuplicate` → [`IdpUserOperationFailure::DuplicateUser`]
///   (VHP-2158: AM surfaces 409 `already_exists` attributed to the field).
/// * `UserOpPasswordPolicy` → [`IdpUserOperationFailure::PasswordPolicy`]
///   (VHP-2158: AM surfaces the `password`/`PASSWORD_POLICY` violation).
/// * `UserOpUnavailable` / any leaked `KcRest` →
///   [`IdpUserOperationFailure::Unavailable`] (provider unreachable — AM
///   surfaces `idp_unavailable`).
/// * `UserOpUnsupported` → [`IdpUserOperationFailure::UnsupportedOperation`].
/// * Anything else leaking through (e.g. `CredStoreRead`) is treated as
///   `Unavailable` so the existing `idp_unavailable` lane carries the
///   operational signal rather than misreporting as `Rejected`.
fn translate_user_op_failure(e: PluginError) -> IdpUserOperationFailure {
    match e {
        PluginError::UserOpRejected { detail } => IdpUserOperationFailure::Rejected { detail },
        // VHP-2158: classified rejections ride their typed SDK variants so
        // AM can surface 409 already_exists / the structured password
        // field-violation instead of the redacted generic reject.
        PluginError::UserOpDuplicate { field, detail } => {
            IdpUserOperationFailure::DuplicateUser { field, detail }
        }
        PluginError::UserOpPasswordPolicy { detail } => {
            IdpUserOperationFailure::PasswordPolicy { detail }
        }
        PluginError::UserOpUnavailable { detail } => {
            IdpUserOperationFailure::Unavailable { detail }
        }
        PluginError::UserOpUnsupported { detail } => {
            IdpUserOperationFailure::UnsupportedOperation { detail }
        }
        PluginError::KcRest { .. } => IdpUserOperationFailure::Unavailable {
            detail: e.to_string(),
        },
        // Malformed / missing-version metadata is a clean rejection —
        // AM should NOT retry. Mirrors the tenant-side `CleanFailure`
        // mapping. Surface a fresh detail with the underlying decode
        // error message so the operator log line is self-contained.
        PluginError::MetadataDecode(ref de) => IdpUserOperationFailure::Rejected {
            detail: format!("metadata decode failed: {de}"),
        },
        // Operator-must-intervene: collapsing this onto `Unavailable`
        // would have AM retry forever; route to `UnsupportedOperation`
        // symmetric with the tenant-side `translate_deprovision_failure`.
        PluginError::BootstrapPermsMissing { ref realm } => {
            IdpUserOperationFailure::UnsupportedOperation {
                detail: format!(
                    "bootstrap admin lacks view-realm/query-groups in {realm} — see Phase 0 IaC checklist"
                ),
            }
        }
        // `Config` is the misconfigured-input variant; treat as a clean
        // rejection so callers fix their request rather than retry.
        PluginError::Config { ref detail } => IdpUserOperationFailure::Rejected {
            detail: format!("config: {detail}"),
        },
        other => IdpUserOperationFailure::Unavailable {
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
#[path = "idp_impl_tests.rs"]
mod idp_impl_tests;

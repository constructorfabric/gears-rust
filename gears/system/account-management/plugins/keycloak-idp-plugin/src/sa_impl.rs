//! Service-account delegation — thin wrappers over
//! [`ServiceAccountFacade`] plus `PluginError` → SDK failure translation.
//!
//! These are inherent methods rather than a trait impl: the machine-identity
//! methods live on `IdpPluginClient` alongside the tenant and user halves, and
//! Rust permits only one `impl IdpPluginClient for KeycloakIdpPlugin` block —
//! which is in [`crate::idp_impl`]. That block delegates here, so this file
//! keeps owning the translation while the trait surface stays in one place.

use account_management_sdk::idp_service_account::{
    IdpListServiceAccountsRequest, IdpProvisionServiceAccountRequest,
    IdpRevokeServiceAccountRequest, IdpRotateServiceAccountSecretRequest,
    IdpServiceAccountCredentials, IdpServiceAccountFailure, IdpServiceAccountSummary,
};
use toolkit_security::SecurityContext;

use crate::domain::error::PluginError;
use crate::domain::ports::metrics::{FailureVariant, SaOp};
use crate::idp_impl::KeycloakIdpPlugin;

/// Log the failure in-process, then translate it to the SDK variant.
///
/// The in-process log is a contract obligation, not a convenience: AM
/// deliberately discards this adapter's `detail` and `field` and answers the
/// caller with its own fixed message, logging only the failure category and
/// the detail's length (see `IdpServiceAccountFailure`'s contract notes).
/// A cause described only in `detail` is therefore a cause no operator can
/// ever read. This is the only place it is recorded.
///
/// The text is safe to emit here: `PluginError::KcRest` bodies are passed
/// through `redact_secrets` and truncated to 2 KiB at construction, and
/// every other variant carries plugin-authored text.
fn log_and_translate_sa_failure(op: SaOp, e: &PluginError) {
    let variant = FailureVariant::from(e);
    // `Ambiguous` means a mutation may have landed and needs operator
    // reconciliation, so it outranks the rest.
    if matches!(e, PluginError::AmbiguousCreated { .. }) {
        tracing::error!(
            target: "keycloak_idp_plugin.service_account",
            op = op.as_str(),
            failure_variant = variant.as_str(),
            detail = %e,
            "service-account operation failed with an Ambiguous outcome - a vendor mutation may have landed; operator must reconcile"
        );
    } else {
        tracing::warn!(
            target: "keycloak_idp_plugin.service_account",
            op = op.as_str(),
            failure_variant = variant.as_str(),
            detail = %e,
            "service-account operation failed"
        );
    }
}

#[must_use]
pub fn translate_sa_failure(e: PluginError) -> IdpServiceAccountFailure {
    match e {
        PluginError::SaInvalidInput { detail, field } => {
            IdpServiceAccountFailure::InvalidInput { detail, field }
        }
        PluginError::SaNotFound { detail } => IdpServiceAccountFailure::NotFound { detail },
        PluginError::AmbiguousCreated { stage, detail } => IdpServiceAccountFailure::Ambiguous {
            detail: format!("{stage}: {detail}"),
        },
        // Everything else is a pre-mutation failure (reads, pre-flight
        // validation, config): no vendor state was retained. Post-mutation
        // escapes are re-tagged Ambiguous in the facade before reaching here.
        other => IdpServiceAccountFailure::CleanFailure {
            detail: other.to_string(),
        },
    }
}

/// Log then translate — the form every operation funnels through.
fn report_sa_failure(op: SaOp, e: PluginError) -> IdpServiceAccountFailure {
    log_and_translate_sa_failure(op, &e);
    translate_sa_failure(e)
}

impl KeycloakIdpPlugin {
    /// Provision a `client_credentials` account owned by
    /// `req.tenant_context.tenant_id`.
    ///
    /// # Errors
    ///
    /// [`IdpServiceAccountFailure`] translated from the facade's
    /// [`PluginError`]: `InvalidInput` for a rejected name, scope, or quota,
    /// `Ambiguous` when a mutation may have landed, `CleanFailure` otherwise.
    pub async fn sa_provision(
        &self,
        ctx: &SecurityContext,
        req: &IdpProvisionServiceAccountRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        self.sa_facade
            .create_inner(ctx, req)
            .await
            .map_err(|e| report_sa_failure(SaOp::Create, e))
    }

    /// Regenerate the addressed account's secret.
    ///
    /// # Errors
    ///
    /// As [`Self::sa_provision`], plus `NotFound` when the `client_id` does
    /// not resolve within the tenant.
    pub async fn sa_rotate_secret(
        &self,
        ctx: &SecurityContext,
        req: &IdpRotateServiceAccountSecretRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        self.sa_facade
            .rotate_secret_inner(ctx, req.tenant_context.tenant_id, &req.client_id)
            .await
            .map_err(|e| report_sa_failure(SaOp::RotateSecret, e))
    }

    /// Delete the addressed account.
    ///
    /// # Errors
    ///
    /// As [`Self::sa_provision`], plus `NotFound` when the account is
    /// already absent - which AM treats as success-equivalent.
    pub async fn sa_revoke(
        &self,
        ctx: &SecurityContext,
        req: &IdpRevokeServiceAccountRequest,
    ) -> Result<(), IdpServiceAccountFailure> {
        self.sa_facade
            .revoke_inner(ctx, req.tenant_context.tenant_id, &req.client_id)
            .await
            .map_err(|e| report_sa_failure(SaOp::Revoke, e))
    }

    /// List the tenant's accounts, with the caller-supplied name reported
    /// verbatim on each entry.
    ///
    /// # Errors
    ///
    /// As [`Self::sa_provision`]; a listing retains nothing, so an uncertain
    /// transport failure surfaces as `CleanFailure`.
    pub async fn sa_list(
        &self,
        ctx: &SecurityContext,
        req: &IdpListServiceAccountsRequest,
    ) -> Result<Vec<IdpServiceAccountSummary>, IdpServiceAccountFailure> {
        self.sa_facade
            .list_inner(ctx, req.tenant_context.tenant_id)
            .await
            .map_err(|e| report_sa_failure(SaOp::List, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_maps_plugin_errors_to_sa_failures() {
        use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError};
        let cases = [
            (
                PluginError::SaInvalidInput {
                    detail: "d".into(),
                    field: None,
                },
                "invalid_input",
            ),
            (PluginError::SaNotFound { detail: "d".into() }, "not_found"),
            (
                PluginError::AmbiguousCreated {
                    stage: AmbiguousStage::KcClientDelete,
                    detail: "d".into(),
                },
                "ambiguous",
            ),
            (PluginError::Config { detail: "d".into() }, "clean_failure"),
            (
                PluginError::KcRest {
                    method: "GET",
                    path_template: "/admin/realms/{realm}/clients".into(),
                    status: KcStatusKind::Http(401),
                    body_first_2kb: String::new(),
                },
                "clean_failure",
            ),
        ];
        for (e, label) in cases {
            assert_eq!(translate_sa_failure(e).as_metric_label(), label);
        }
    }
}

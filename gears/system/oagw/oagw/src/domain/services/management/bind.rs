use crate::domain::error::{DeniedOn, DomainError};
use crate::domain::model::Upstream;
use crate::domain::plugin::credentials::{CredentialRef, credstore_error_kind};

use super::authz::BIND_UPSTREAM_RESOURCE;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use credstore_sdk::CredStoreClientV1;
use toolkit_security::SecurityContext;

/// Permission action names for ancestor bind checks.
pub(in crate::domain::services) mod bind_actions {
    pub const BIND: &str = "bind";
    pub const OVERRIDE_AUTH: &str = "override_auth";
    pub const OVERRIDE_RATE: &str = "override_rate";
    pub const ADD_PLUGINS: &str = "add_plugins";
}

/// Report an ancestor bind denial on the upstream resource, logging a
/// policy denial with who asked for what (`From<EnforcerError>` logs only
/// evaluation failures).
fn on_upstream<'a>(
    ctx: &'a SecurityContext,
    action: &'static str,
    ancestor: &'a Upstream,
) -> impl FnOnce(authz_resolver_sdk::EnforcerError) -> DomainError + 'a {
    move |e| {
        if let authz_resolver_sdk::EnforcerError::AccessDenied(
            authz_resolver_sdk::AccessDenial::Pdp {
                reason: deny_reason,
            },
        ) = &e
        {
            tracing::info!(
                subject_id = %ctx.subject_id(),
                tenant_id = %ctx.subject_tenant_id(),
                action,
                ancestor_upstream_id = %ancestor.id,
                ancestor_tenant_id = %ancestor.tenant_id,
                policy_reason = deny_reason.as_ref().map(|r| r.error_code.as_str()),
                "OAGW ancestor bind denied"
            );
        }
        DomainError::from(e).on_entity(DeniedOn::Upstream)
    }
}

/// Describes the override fields a descendant is attempting to set.
/// Used by `validate_bind_constraints` so both create and update can share
/// the same validation logic.
#[allow(unknown_lints, de0309_must_have_domain_model)] // short-lived param container, not a domain entity
pub(in crate::domain::services) struct BindOverrides<'a> {
    pub server: &'a crate::domain::model::Server,
    pub auth: Option<&'a crate::domain::model::AuthConfig>,
    pub rate_limit: Option<&'a crate::domain::model::RateLimitConfig>,
    pub plugins: Option<&'a crate::domain::model::PluginsConfig>,
    pub cors: Option<&'a crate::domain::model::CorsConfig>,
}

/// Validate bind constraints when a descendant creates or updates an
/// upstream whose alias matches an ancestor's upstream.
///
/// `ancestors` are every ancestor upstream with the alias, closest first
/// ([`super::ancestor::ancestor_upstreams`]); without one there is nothing to
/// bind to and nothing is checked.
///
/// Permissions (per `cpt-cf-oagw-algo-tenant-permission-check`), on the
/// closest ancestor:
/// - `oagw:upstream:bind` — always required
/// - `oagw:upstream:override_auth` — required when ancestor auth sharing is `inherit`
/// - `oagw:upstream:override_rate` — required when ancestor rate_limit sharing is `inherit`
/// - `oagw:upstream:add_plugins` — required when ancestor plugins sharing is not `private`
///
/// Sharing modes:
/// - `enforce` → blocked (plugins excepted: merge-time protection). For auth
///   and cors on any ancestor: an enforced field is sticky down the chain,
///   whatever the ancestors in between set. For rate_limit on the closest
///   ancestor only; the merge caps the effective limit by every enforcing one
/// - `private` → shadow allowed, no permission
/// - `inherit` → allowed with permission
pub(in crate::domain::services) async fn validate_bind_constraints(
    ctx: &SecurityContext,
    enforcer: &PolicyEnforcer,
    ancestors: &[Upstream],
    overrides: &BindOverrides<'_>,
) -> Result<(), DomainError> {
    use crate::domain::model::SharingMode;

    let Some(ancestor) = ancestors.first() else {
        return Ok(());
    };

    // 1. Check bind permission.
    let access_req = AccessRequest::new()
        .resource_property("owner_tenant_id", ancestor.tenant_id)
        .require_constraints(false);
    enforcer
        .access_scope_with(
            ctx,
            &BIND_UPSTREAM_RESOURCE,
            bind_actions::BIND,
            Some(ancestor.id),
            &access_req,
        )
        .await
        .map_err(on_upstream(ctx, bind_actions::BIND, ancestor))?;

    // 2. An enforced credential is applied on every request, and it may go
    //    only to endpoints its owner configured.
    if let Some(server) = enforced_auth_server(ancestors)
        && !super::merge::server_within(overrides.server, server)
    {
        return Err(DomainError::validation(format!(
            "cannot bind alias '{}' to endpoints the ancestor upstream does not list: its \
             auth has sharing mode 'enforce'",
            ancestor.alias
        )));
    }

    // 3. Check per-field override permissions and sharing mode constraints.

    // Auth override.
    if overrides.auth.is_some() {
        if enforced_by_any(ancestors, |u| u.auth.as_ref().map(|a| a.sharing)) {
            return Err(DomainError::validation(
                "cannot override auth: ancestor upstream has sharing mode 'enforce'",
            ));
        }
        match ancestor.auth.as_ref().map(|a| a.sharing) {
            Some(SharingMode::Private) => {
                // Private = invisible → descendant is providing fresh config (shadow),
                // not overriding.  No permission check needed.
            }
            _ => {
                // Inherit / absent → real override, requires permission.
                enforcer
                    .access_scope_with(
                        ctx,
                        &BIND_UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_AUTH,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await
                    .map_err(on_upstream(ctx, bind_actions::OVERRIDE_AUTH, ancestor))?;
            }
        }
    }

    // Rate-limit override. Only the closest ancestor's enforce refuses it: the
    // merge keeps an enforced limit as a floor down the whole chain, so a
    // descendant's limit never exceeds a farther enforcing ancestor's at
    // runtime. Refusing it here would only block allocated-budget children
    // under an inherit parent, whose semantics under enforce wait on #4971.
    if overrides.rate_limit.is_some() {
        match ancestor.rate_limit.as_ref().map(|r| r.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override rate_limit: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {}
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &BIND_UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_RATE,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await
                    .map_err(on_upstream(ctx, bind_actions::OVERRIDE_RATE, ancestor))?;
            }
        }
    }

    // Plugins override (Enforce handled at merge time, not here).
    if overrides.plugins.is_some() {
        match ancestor.plugins.as_ref().map(|p| p.sharing) {
            Some(SharingMode::Private) => {}
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &BIND_UPSTREAM_RESOURCE,
                        bind_actions::ADD_PLUGINS,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await
                    .map_err(on_upstream(ctx, bind_actions::ADD_PLUGINS, ancestor))?;
            }
        }
    }

    // CORS override (no additional permission required, but Enforce blocks).
    if overrides.cors.is_some()
        && enforced_by_any(ancestors, |u| u.cors.as_ref().map(|c| c.sharing))
    {
        return Err(DomainError::validation(
            "cannot override cors: ancestor upstream has sharing mode 'enforce'",
        ));
    }

    Ok(())
}

/// Whether any of `ancestors` enforces the field `sharing` reads. A disabled
/// ancestor counts too: the merge applies its enforced field again once it is
/// enabled, so a descendant's override of it could never take effect.
fn enforced_by_any(
    ancestors: &[Upstream],
    sharing: impl Fn(&Upstream) -> Option<crate::domain::model::SharingMode>,
) -> bool {
    ancestors
        .iter()
        .any(|u| sharing(u) == Some(crate::domain::model::SharingMode::Enforce))
}

/// How long a write waits for the credential store to answer whether a
/// reference resolves.
const CREDSTORE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Detail of a credential reference that does not resolve. The ref itself is
/// never put in logs or errors (F0008): the caller already knows which one it
/// sent, and the violation's field names the key.
const SECRET_REF_NOT_ACCESSIBLE: &str =
    "credential reference is not accessible to this tenant (not provisioned yet, or not shared)";

/// Validate that a credential reference is accessible to the requesting
/// tenant via `cred_store`. Per `cpt-cf-oagw-principle-cred-isolation`, if
/// the secret is not accessible, the request is rejected (fail-closed).
pub(in crate::domain::services) async fn validate_secret_ref_accessible(
    ctx: &SecurityContext,
    credstore: &dyn CredStoreClientV1,
    credential: CredentialRef<'_>,
) -> Result<(), DomainError> {
    let secret_key = parse_secret_ref(credential)?;
    // cancel-safe: `get` only reads; dropping it at the deadline leaves no
    // state behind in OAGW or the store.
    // An SDK write has no request deadline of its own: a stalled store
    // must not hold it (or a gear's startup provisioning) forever.
    let Ok(found) =
        tokio::time::timeout(CREDSTORE_CHECK_TIMEOUT, credstore.get(ctx, &secret_key)).await
    else {
        tracing::warn!(
            tenant_id = %ctx.subject_tenant_id(),
            subject_id = %ctx.subject_id(),
            field = credential.field,
            timeout = ?CREDSTORE_CHECK_TIMEOUT,
            "cred_store did not answer during secret_ref validation"
        );
        return Err(DomainError::Internal {
            message: "credential validation unavailable: the credential store did not answer"
                .to_owned(),
        });
    };

    match found {
        Ok(Some(_)) => Ok(()),
        // Not a malformed argument: the ref parses, the secret just isn't
        // there (yet) or isn't shared. Distinguished from `Validation` so
        // provisioning callers can defer-and-retry instead of treating it
        // as a deterministic misconfiguration.
        Ok(None) => Err(DomainError::SecretRefNotAccessible {
            field: credential.field,
            detail: SECRET_REF_NOT_ACCESSIBLE.to_owned(),
            instance: String::new(),
        }),
        // Fail-closed: cred_store failure → reject.
        Err(e @ credstore_sdk::CredStoreError::Internal(_)) => Err(credstore_outage(ctx, &e)),
        // Same state as `Ok(None)`, reported as an error by some client
        // implementations (e.g. a metadata row whose value is still being
        // provisioned): not accessible *yet*, so provisioning callers must be
        // able to defer-and-retry rather than fail fast.
        Err(e) if e.is_not_found() => Err(DomainError::SecretRefNotAccessible {
            field: credential.field,
            detail: SECRET_REF_NOT_ACCESSIBLE.to_owned(),
            instance: String::new(),
        }),
        // The credential store is down or absent: nothing about this request
        // is wrong, and every other secret_ref would fail the same way.
        Err(e) if e.is_unavailable() => Err(credstore_outage(ctx, &e)),
        Err(e) => Err(DomainError::validation_for(
            credential.field,
            oagw_sdk::field::VALIDATION,
            format!(
                "{}: credential store refused the reference: {}",
                credential.field,
                credstore_error_kind(&e)
            ),
        )),
    }
}

/// A credential store failure while validating a `secret_ref`: logged and
/// answered as `Internal`.
fn credstore_outage(ctx: &SecurityContext, e: &credstore_sdk::CredStoreError) -> DomainError {
    let kind = credstore_error_kind(e);
    tracing::warn!(
        tenant_id = %ctx.subject_tenant_id(),
        subject_id = %ctx.subject_id(),
        error = kind,
        "cred_store unavailable during secret_ref validation"
    );
    DomainError::Internal {
        message: format!("credential validation unavailable: {kind}"),
    }
}

/// Parse a credential reference (with or without the `cred://` scheme),
/// without asking the credential store whether it exists. The error names
/// the field and not the reference.
pub(in crate::domain::services) fn parse_secret_ref(
    credential: CredentialRef<'_>,
) -> Result<credstore_sdk::SecretRef, DomainError> {
    crate::domain::plugin::credentials::parse_credential_ref(credential.value).map_err(|e| {
        let reason = match &e {
            credstore_sdk::CredStoreError::InvalidSecretRef { reason } => reason.as_str(),
            other => credstore_error_kind(other),
        };
        DomainError::validation_for(
            credential.field,
            oagw_sdk::field::VALIDATION,
            format!(
                "{}: invalid credential reference: {reason}",
                credential.field
            ),
        )
    })
}

/// The server of the ancestor whose enforced auth is in effect: the farthest
/// enabled one of `ancestors` (closest first) that enforces its auth. The
/// proxy merges only enabled ancestors and keeps the first enforced auth it
/// meets from the root down.
fn enforced_auth_server(ancestors: &[Upstream]) -> Option<&crate::domain::model::Server> {
    ancestors
        .iter()
        .rfind(|u| {
            u.enabled
                && u.auth
                    .as_ref()
                    .is_some_and(|a| a.sharing == crate::domain::model::SharingMode::Enforce)
        })
        .map(|u| &u.server)
}

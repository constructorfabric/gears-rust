//! Per-request secret references (ADR-0022, `cpt-cf-oagw-adr-request-secret-refs`).
//!
//! An upstream whose effective auth is the API-key plugin in `request_ref`
//! mode injects the secret a request names in `X-OAGW-Secret-Ref`, or its
//! configured default. The proxy pipeline runs the steps here in the order
//! the ADR fixes: header syntax ([`requested_ref`]), mode ([`select`]), then,
//! after rate admission, the CredStore read under the caller's context and
//! the owner-chain and type guards ([`resolve`]). The key is held back from
//! header rules and transforms: the auth header is stripped before they run
//! ([`InjectedSecret::strip`]), must still be unset after them
//! ([`check_auth_header_unset`]), and the key goes in only after the final
//! header setup ([`InjectedSecret::apply`]).
//!
//! Neither the reference nor the secret value goes into an error, a log line
//! or a metric label.

use std::time::Duration;

use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
use http::{HeaderMap, HeaderName, HeaderValue};
use oagw_sdk::{SECRET_REF_HEADER, field, reason};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::AuthConfig;
use crate::domain::plugin::credentials::{
    ApiKeyAuthConfig, CredentialMode, credstore_error_kind, parse_credential_ref,
};
use crate::domain::tenant_chain::owner_within;

/// Which secret a `request_ref` request injects.
pub(crate) enum SecretSource {
    /// The one the request named.
    Requested(SecretRef),
    /// The upstream's default.
    Default(SecretRef),
}

impl SecretSource {
    fn reference(&self) -> &SecretRef {
        match self {
            Self::Requested(r) | Self::Default(r) => r,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Requested(_) => "requested",
            Self::Default(_) => "default",
        }
    }
}

/// A `request_ref` request's settings and secret, ready for [`resolve`].
pub(crate) struct RequestRefPlan {
    /// The upstream's API-key settings, parsed by the shared parser.
    pub(crate) auth: ApiKeyAuthConfig,
    /// `auth.header` as a header name.
    pub(crate) header: HeaderName,
    /// The secret to read.
    pub(crate) source: SecretSource,
}

/// The value [`resolve`] read, to inject after the final header setup.
/// Holds the secret: it has no `Debug` and its header value is marked
/// sensitive.
pub(crate) struct InjectedSecret {
    header: HeaderName,
    value: HeaderValue,
}

impl InjectedSecret {
    /// Remove every value of the auth header (a client-supplied one
    /// included), before header rules and transforms run, so they never see
    /// a key in it.
    pub(crate) fn strip(&self, headers: &mut HeaderMap) {
        headers.remove(&self.header);
    }

    /// Set the auth header to the key. Called once the outbound headers are
    /// final, so no header rule or transform sees the key.
    pub(crate) fn apply(&self, headers: &mut HeaderMap) {
        headers.insert(self.header.clone(), self.value.clone());
    }
}

fn invalid_argument(reason: &'static str, detail: &str, instance: &str) -> DomainError {
    DomainError::Validation {
        field: SECRET_REF_HEADER,
        reason,
        detail: detail.to_owned(),
        instance: instance.to_owned(),
    }
}

fn not_accessible() -> DomainError {
    DomainError::forbidden_with_reason(
        reason::permission::SECRET_REF_NOT_ACCESSIBLE,
        "the requested secret is missing or not visible to the caller",
    )
}

fn policy_denied(detail: &str) -> DomainError {
    DomainError::forbidden_with_reason(reason::permission::SECRET_REF_POLICY_DENIED, detail)
}

/// The reference the request names in [`SECRET_REF_HEADER`], if any.
/// Exactly one value holding one reference, with or without `cred://`;
/// anything else is `INVALID_SECRET_REF`. Header names are
/// case-insensitive: `http` stores them lowercased.
pub(crate) fn requested_ref(
    headers: &HeaderMap,
    instance: &str,
) -> Result<Option<SecretRef>, DomainError> {
    let invalid = |detail: &str| invalid_argument(field::INVALID_SECRET_REF, detail, instance);
    let mut values = headers.get_all(SECRET_REF_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid("the secret reference header must appear once"));
    }
    let value = value
        .to_str()
        .map_err(|_| invalid("the secret reference header is not visible ASCII"))?
        .trim();
    if value.is_empty() {
        return Err(invalid("the secret reference header is empty"));
    }
    if value.contains(',') {
        return Err(invalid(
            "the secret reference header must hold exactly one reference",
        ));
    }
    parse_credential_ref(value)
        .ok()
        .map(Some)
        .ok_or_else(|| invalid("the secret reference is malformed"))
}

/// Check the request against the upstream's mode (the ADR's mode table).
/// `None` means the request goes the `config` way: the auth plugin, if any,
/// runs as before.
pub(crate) fn select(
    mode: CredentialMode,
    auth: Option<&AuthConfig>,
    requested: Option<SecretRef>,
    instance: &str,
) -> Result<Option<RequestRefPlan>, DomainError> {
    let request_ref_auth = match (mode, auth) {
        (CredentialMode::RequestRef, Some(auth)) => auth,
        (CredentialMode::Config, _) | (CredentialMode::RequestRef, None) => {
            return match requested {
                Some(_) => Err(invalid_argument(
                    field::SECRET_REF_NOT_ALLOWED,
                    "the upstream does not accept a secret reference",
                    instance,
                )),
                None => Ok(None),
            };
        }
    };
    // A config the writes should have refused (a row stored before a check
    // existed) is reported as the API-key plugin reports a bad config: a
    // misconfiguration, 500.
    let bad_config = |detail: &str| DomainError::Internal {
        message: format!("auth plugin misconfigured: invalid apikey auth config: {detail}"),
    };
    let mut auth = ApiKeyAuthConfig::parse(request_ref_auth.config.as_ref())
        .map_err(|e| bad_config(&e.detail))?;
    let header = HeaderName::from_bytes(auth.header.as_bytes())
        .map_err(|_| bad_config("missing or invalid header"))?;
    // The plan carries the chosen source; the default is not read again.
    let source = match (requested, auth.secret_ref.take()) {
        (Some(requested), _) => SecretSource::Requested(requested),
        (None, Some(default)) => SecretSource::Default(default),
        (None, None) => {
            return Err(invalid_argument(
                field::SECRET_REF_REQUIRED,
                "the upstream has no default secret; send a secret reference",
                instance,
            ));
        }
    };
    Ok(Some(RequestRefPlan {
        auth,
        header,
        source,
    }))
}

/// Who is asking and where the request is going, for the guards and for the
/// log lines of the read.
pub(crate) struct ResolveScope<'a> {
    /// The caller's tenant chain, `[self, parent, ..., root]`.
    pub(crate) tenant_chain: &'a [Uuid],
    /// The tenant whose auth config is in effect.
    pub(crate) auth_owner_tenant_id: Uuid,
    /// The effective upstream.
    pub(crate) upstream_id: Uuid,
    /// The matched route.
    pub(crate) route_id: Uuid,
    /// The inbound `x-request-id`, when the client sent one.
    pub(crate) request_id: Option<&'a str>,
}

impl ResolveScope<'_> {
    /// The calling tenant, first on the chain (nil if the chain is empty,
    /// which alias resolution never produces).
    fn caller(&self) -> Uuid {
        self.tenant_chain.first().copied().unwrap_or_default()
    }
}

/// Read the secret under the caller's context, apply the owner-chain and
/// type guards, and build the header value. Never falls back to another
/// secret: a requested reference that fails returns the error. A read that
/// does not answer by `deadline` is an outage.
pub(crate) async fn resolve(
    credstore: &dyn CredStoreClientV1,
    ctx: &SecurityContext,
    plan: &RequestRefPlan,
    scope: &ResolveScope<'_>,
    deadline: tokio::time::Instant,
    instance: &str,
) -> Result<InjectedSecret, DomainError> {
    let unavailable =
        |detail: &str, retry_after: Option<Duration>| DomainError::CredentialStoreUnavailable {
            detail: detail.to_owned(),
            retry_after,
            instance: instance.to_owned(),
        };
    // cancel-safe: `get` only reads. The read shares the request's deadline
    // with endpoint selection and the exchange.
    let response = match tokio::time::timeout_at(
        deadline,
        credstore.get(ctx, plan.source.reference()),
    )
    .await
    {
        Err(_) => {
            tracing::warn!(
                caller_tenant_id = %scope.caller(),
                upstream_id = %scope.upstream_id,
                route_id = %scope.route_id,
                request_id = scope.request_id.unwrap_or(""),
                "credential store did not answer for a request_ref secret \
                 within the request timeout"
            );
            return Err(unavailable("the credential store did not answer", None));
        }
        Ok(Ok(Some(response))) => response,
        Ok(Ok(None)) => {
            tracing::info!(
                caller_tenant_id = %scope.caller(),
                upstream_id = %scope.upstream_id,
                route_id = %scope.route_id,
                request_id = scope.request_id.unwrap_or(""),
                source = plan.source.label(),
                "request_ref secret not accessible: no value for the reference"
            );
            return Err(not_accessible());
        }
        Ok(Err(e)) => return Err(map_credstore_error(&e, scope, unavailable)),
    };
    check_owner_chain(&response, scope)?;
    check_secret_type(&response, plan.auth.request_secret_types.as_deref(), scope)?;

    // A stored value the header cannot carry. Logged with the record, never
    // the value, so an operator can find and fix it.
    let unusable_value = |problem: &'static str| {
        tracing::warn!(
            record_id = %response.id,
            owner_tenant_id = %response.owner_tenant_id,
            caller_tenant_id = %scope.caller(),
            upstream_id = %scope.upstream_id,
            route_id = %scope.route_id,
            request_id = scope.request_id.unwrap_or(""),
            problem,
            "request_ref secret value cannot be injected"
        );
        DomainError::AuthenticationFailed {
            reason: reason::auth::PLUGIN_INTERNAL,
            detail: format!("secret value is {problem}"),
            instance: instance.to_owned(),
        }
    };
    let secret = std::str::from_utf8(response.value.as_bytes())
        .map_err(|_| unusable_value("not valid UTF-8"))?;
    // Prefix and key in one buffer sized up front (no reallocation leaves a
    // copy behind) and zeroed on drop, so no plain heap copy of the key
    // outlives the header value.
    let mut buf =
        zeroize::Zeroizing::new(Vec::with_capacity(plan.auth.prefix.len() + secret.len()));
    buf.extend_from_slice(plan.auth.prefix.as_bytes());
    buf.extend_from_slice(secret.as_bytes());
    let mut value =
        HeaderValue::from_bytes(&buf).map_err(|_| unusable_value("not a valid header value"))?;
    value.set_sensitive(true);
    tracing::info!(
        record_id = %response.id,
        owner_tenant_id = %response.owner_tenant_id,
        secret_type = %response.secret_type,
        upstream_id = %scope.upstream_id,
        route_id = %scope.route_id,
        request_id = scope.request_id.unwrap_or(""),
        source = plan.source.label(),
        "request_ref secret resolved"
    );
    Ok(InjectedSecret {
        header: plan.header.clone(),
        value,
    })
}

/// A credential store error on a `request_ref` read: a missing or refused
/// secret is not accessible; an outage is a 503, with CredStore's own retry
/// hint when it gave one; an error only a write can cause is internal (500,
/// no retry hint), since retrying cannot help. Only the error's kind is
/// logged: its text may quote the reference.
fn map_credstore_error(
    e: &CredStoreError,
    scope: &ResolveScope<'_>,
    unavailable: impl Fn(&str, Option<Duration>) -> DomainError,
) -> DomainError {
    match e {
        CredStoreError::NotFound
        | CredStoreError::AccessDenied
        | CredStoreError::InvalidSecretRef { .. } => {
            tracing::info!(
                caller_tenant_id = %scope.caller(),
                upstream_id = %scope.upstream_id,
                route_id = %scope.route_id,
                request_id = scope.request_id.unwrap_or(""),
                error = credstore_error_kind(e),
                "request_ref secret not accessible"
            );
            not_accessible()
        }
        CredStoreError::ServiceUnavailable { retry_after, .. } => {
            log_credstore_failure(e, scope);
            unavailable("the credential store is unavailable", *retry_after)
        }
        CredStoreError::NoPluginAvailable | CredStoreError::Internal(_) => {
            log_credstore_failure(e, scope);
            unavailable("the credential store is unavailable", None)
        }
        CredStoreError::Conflict
        | CredStoreError::UnsupportedTransition { .. }
        | CredStoreError::TypeViolation { .. } => {
            log_credstore_failure(e, scope);
            DomainError::Internal {
                message: "the credential store returned an unexpected error for a secret read"
                    .to_owned(),
            }
        }
    }
}

/// Warn about a credential store failure on a `request_ref` read, with the
/// request's identifiers and the error's kind (never its text).
fn log_credstore_failure(e: &CredStoreError, scope: &ResolveScope<'_>) {
    tracing::warn!(
        caller_tenant_id = %scope.caller(),
        upstream_id = %scope.upstream_id,
        route_id = %scope.route_id,
        request_id = scope.request_id.unwrap_or(""),
        error = credstore_error_kind(e),
        "credential store failed for a request_ref secret"
    );
}

/// Guard 3: the secret's owner is the auth owner's tenant or below it on the
/// caller's chain (`[caller, ..., auth owner, ..., root]`).
///
/// TODO(credstore-v2): the v2 secret has no `owner_tenant_id` or record id
/// (CredStore ADR-0009). On the switch the read becomes
/// `get_secret_within(ctx, reference, auth_owner_tenant_id)` (#5943), which
/// runs this guard inside CredStore; map its results as ADR-0022 guard 3
/// lists, including the `SECRET_EXPIRED` arm (else it falls to 503) and
/// `BOUND_NOT_ON_CHAIN` (an `Invalid` error: match the reason, warn). The
/// success log then drops the record id and owner tenant.
fn check_owner_chain(
    response: &GetSecretResponse,
    scope: &ResolveScope<'_>,
) -> Result<(), DomainError> {
    if owner_within(
        response.owner_tenant_id.0,
        scope.auth_owner_tenant_id,
        scope.tenant_chain,
    ) {
        return Ok(());
    }
    tracing::warn!(
        record_id = %response.id,
        owner_tenant_id = %response.owner_tenant_id,
        auth_owner_tenant_id = %scope.auth_owner_tenant_id,
        caller_tenant_id = %scope.caller(),
        upstream_id = %scope.upstream_id,
        route_id = %scope.route_id,
        request_id = scope.request_id.unwrap_or(""),
        "request_ref secret owned outside the auth owner's subtree"
    );
    Err(policy_denied(
        "the secret is owned above the tenant whose auth config is in effect",
    ))
}

/// Guard 4: with `request_secret_types`, the secret's type is listed.
fn check_secret_type(
    response: &GetSecretResponse,
    allowed: Option<&[String]>,
    scope: &ResolveScope<'_>,
) -> Result<(), DomainError> {
    match allowed {
        Some(allowed) if !allowed.contains(&response.secret_type) => {
            tracing::warn!(
                record_id = %response.id,
                secret_type = %response.secret_type,
                caller_tenant_id = %scope.caller(),
                upstream_id = %scope.upstream_id,
                route_id = %scope.route_id,
                request_id = scope.request_id.unwrap_or(""),
                "request_ref secret type is not allowed by the upstream"
            );
            Err(policy_denied(
                "the secret's type is not allowed by the upstream",
            ))
        }
        _ => Ok(()),
    }
}

/// Guard 6: the auth header was stripped before header rules and
/// transforms, so a value there now was set by one of them. That is the
/// gateway's misconfiguration, not the caller's: refuse with an internal
/// error (500), not overwrite the value, so it surfaces, and keep
/// `SECRET_REF_POLICY_DENIED` for the caller-side guards 3 and 4. The warn
/// names the upstream and route an operator must fix.
pub(crate) fn check_auth_header_unset(
    headers: &HeaderMap,
    injected: &InjectedSecret,
    upstream_id: Uuid,
    route_id: Uuid,
) -> Result<(), DomainError> {
    if headers.contains_key(&injected.header) {
        tracing::warn!(
            header = %injected.header,
            upstream_id = %upstream_id,
            route_id = %route_id,
            "header rules or transforms set the auth header of a request_ref upstream"
        );
        return Err(auth_header_conflict());
    }
    Ok(())
}

/// The error guard 6 returns: a fixed internal error, so neither the header
/// nor the key appears in it.
pub(crate) fn auth_header_conflict() -> DomainError {
    DomainError::Internal {
        message: "the upstream's header rules or transforms set its auth header".to_owned(),
    }
}

#[cfg(test)]
#[path = "secret_ref_unit_tests.rs"]
mod secret_ref_unit_tests;

//! The gear's own identity: the service principal it presents to the platform
//! wherever it acts on its own behalf, and the security context built from it.
//!
//! The Secret Manager (toward the Credential Store) and the Event Broker client
//! (publishing and consuming the cache broadcast) both present it; neither
//! defines a principal of its own.

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The name the gear's store namespace and principal derive from.
const NAMESPACE_NAME: &[u8] = b"urn:constructorfabric:gears:settings-service";

/// First-party wildcard token scope (`"*"`) presented on the Credential Store
/// path.
///
/// This is an in-process call made on the gear's own behalf, so no bearer
/// token exists to carry scopes — and the authorization resolver's OAuth
/// token-scope enforcer fail-closes on an empty `token_scopes` with
/// `scope_mismatch`, *before* RBAC is consulted at all. Without this the store
/// call is refused before any policy runs, and the refusal arrives as
/// `access denied` from a store that is neither down nor mis-provisioned.
///
/// It widens nothing. The scope check only decides whether evaluation
/// continues; what this gear may actually do in the store remains RBAC's to
/// say, through the narrow Credstore Secret Operator grant held by the derived
/// service principal — which is also why presenting the wildcard cannot grant
/// anything the grant does not already allow. `keycloak-idp-plugin` presents
/// the same scope on the same path, for the same reason.
pub(crate) const FIRST_PARTY_TOKEN_SCOPE: &str = "*";

/// The subject type this gear presents wherever it acts on its own behalf (the
/// Credential Store path, the Event Broker), classifying its principal as a
/// machine.
///
/// The tag is what decides which principal type RBAC is asked about, and the
/// grant this gear holds is held by a service principal. An **absent** tag does
/// not read as "not a person": the authorization plugin maps `None` to `User`
/// outright, mirroring RBAC's own resolver, so leaving it off asks for a grant
/// held by a user with this id — which no one holds — and the store call is
/// denied after passing every other check. The gear's own grant is lost to the
/// omission.
///
/// Like the scope above this widens nothing: naming the type only selects which
/// grant is looked up, and the narrow Credstore Secret Operator role remains
/// the authoritative limit. It is the same spelling `keycloak-idp-plugin`
/// presents on the same path, and the pair — service tag, service-principal
/// grant — is what makes the lookup meet. The interactive vocabulary this gear
/// reads on the way *in* is a separate matter; see
/// [`crate::domain::stepup::INTERACTIVE_SUBJECT_TYPES`].
pub(crate) const SERVICE_SUBJECT_TYPE: &str =
    toolkit_gts::gts_id!("cf.core.security.subject_service.v1~");

/// The name-based namespace the gear derives its identifiers from: the Secret
/// Manager's references and the service principal.
#[must_use]
pub(crate) fn service_namespace() -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, NAMESPACE_NAME)
}

/// The principal this gear presents wherever it acts on its own behalf: the
/// Credential Store and the Event Broker. Fixed, derived from the gear's name.
#[must_use]
pub(crate) fn service_principal() -> Uuid {
    Uuid::new_v5(&service_namespace(), b"principal")
}

/// The gear's own security context in `tenant`: the subject is
/// [`service_principal`] (this gear's, so only this gear reads back what it
/// stored), the subject type says that principal is a machine, because an
/// omitted one is read as a person and asks for a grant nobody holds — see
/// [`SERVICE_SUBJECT_TYPE`] — and the first-party wildcard scope, without which
/// the enforcer refuses the call before RBAC is asked at all — see
/// [`FIRST_PARTY_TOKEN_SCOPE`]. The tenant is the caller's: the target tenant on
/// the store path, the root tenant on the broker path.
///
/// # Errors
/// [`DomainError::Internal`] when the context cannot be built.
pub(crate) fn service_context(tenant: Uuid) -> Result<SecurityContext, DomainError> {
    SecurityContext::builder()
        .subject_id(service_principal())
        .subject_type(SERVICE_SUBJECT_TYPE)
        .subject_tenant_id(tenant)
        .token_scopes(vec![FIRST_PARTY_TOKEN_SCOPE.to_owned()])
        .build()
        .map_err(|err| DomainError::Internal {
            diagnostic: format!("service context: {err}"),
        })
}

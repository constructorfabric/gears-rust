//! GTS (Global Type System) declarations for the service-principal SDK.
//!
//! Declares the **managed-resource** type for a service principal — the type
//! RBAC/PEP authorizes create/list/rotate/revoke operations against. This is a
//! new resource type owned by the service-principal gear (NOT the account-
//! management `user` type): a service principal is a machine identity backed by
//! an `IdP` client + service-account user, with its own lifecycle (incl. secret
//! rotation) and no account-management user record. Folding it into `cf.core.am.*`
//! would let "manage users" silently grant machine-credential minting.
//!
//! Distinct from the service-principal *subject* type
//! (`cf.core.security.subject_service_principal.v1~`): that is what the principal
//! IS when it authenticates; this is what RBAC protects when it is managed. The id
//! deliberately contains `service_principal` but NOT the substring `subject_service`
//! so it cannot collide with substring-based principal-type classification.
//!
//! Registering the `#[gts_type_schema]` struct with the types-registry (link-time
//! inventory) is what makes the type authorizable: RBAC validates a role's
//! `target_type` against the registry and the PDP compiles a tenant-scoped grant
//! into an `InTenantSubtree` constraint on `owner_tenant_id`. Without it every
//! operation is denied (403) because no role can name the type.

use toolkit_gts::{gts_id, gts_type_schema};

/// GTS resource-type id for the service principal as a **managed resource** — the
/// single source of truth for this string. Mirrored by the impl gear's
/// `domain::authz::SERVICE_PRINCIPAL` PEP `ResourceType`, the permission catalog,
/// and the `#[resource_error(...)]` marker (pinned by a unit test there).
pub const SERVICE_PRINCIPAL_RESOURCE_TYPE: &str =
    gts_id!("cf.core.service_principal.service_principal.v1~");

/// GTS type-schema for the service-principal managed resource.
///
/// Registered with an empty property set: authorization only needs the type *id*
/// known to the registry (RBAC `target_type` validation) and the PDP — the
/// tenant-scope binding comes from the impl gear's `ResourceType`
/// (`OWNER_TENANT_ID`), not the schema body. Mirrors `credstore_sdk::SecretV1`.
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.service_principal.service_principal.v1~"),
    description = "Service principal — tenant-scoped machine identity, RBAC/PEP protected",
    properties = "",
    base = true
)]
pub struct ServicePrincipalV1;

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_gts::GTS_ID_PREFIX;

    #[test]
    fn resource_type_id_is_stable() {
        // Pin the human-authored suffix. The leading `gts.` prefix is
        // build-time configurable (GTS_ID_PREFIX) and is exactly what `gts_id!`
        // prepends, so reconstruct the expected id from the prefix constant
        // rather than hard-coding it — hard-coding the prefix would defeat the
        // configuration and is rejected by the DE0904 lint.
        assert_eq!(
            SERVICE_PRINCIPAL_RESOURCE_TYPE,
            format!("{GTS_ID_PREFIX}cf.core.service_principal.service_principal.v1~")
        );
    }

    #[test]
    fn resource_type_id_is_not_a_subject_type() {
        // Guard against colliding with substring-based principal classification,
        // which matches on `subject_service` / `subject_user`.
        assert!(!SERVICE_PRINCIPAL_RESOURCE_TYPE.contains("subject_service"));
    }
}

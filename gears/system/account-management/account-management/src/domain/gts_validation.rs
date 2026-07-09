//! GTS-runtime payload validation.
//!
//! AM resource shapes (`tenant`, `user`) are pinned by JSON Schemas
//! published under `gears/system/account-management/docs/schemas/`
//! and registered in the GTS Types Registry at deployment time. This
//! gear is the single AM-side seam that fetches a resolved schema
//! from `TypesRegistryClient` and runs `jsonschema::validator_for`
//! against the supplied field values — adapting the
//! `cf-resource-group::domain::validation::validate_metadata_via_gts`
//! pattern to AM's needs with one deliberate divergence: AM maps a
//! non-not-found `CanonicalError` to
//! [`DomainError::ServiceUnavailable`] (HTTP 503) rather than
//! `DomainError::validation` (HTTP 400). A registry outage is an
//! infrastructure event, not a payload problem, and the 503 envelope
//! lets clients distinguish "your input is bad" from "our dependency
//! is sick" without reading the detail string.
//!
//! Unlike the resource-group helper, AM validates per-property
//! (`username`, `name`, etc.) rather than against the full instance
//! schema. The published `gts.cf.core.am.user.v1~` and
//! `gts.cf.core.am.tenant.v1~` schemas describe the resource as
//! seen on read (with required identifiers like `id`), but the
//! create-payload shapes deliberately omit those server-assigned
//! fields. Validating per-property keeps the structural bounds
//! authoritative without forcing synthetic `id` placeholders that
//! couple the helper to the schema's `required` list.
//!
//! Service layers MUST call into this gear instead of hardcoding
//! length / format constants at the call site so the schema files
//! remain the single source of truth. AM business rules that are
//! *wider* than the JSON Schema (e.g. "username MUST not be
//! all-whitespace" — which the schema's `minLength: 1` does not
//! reject) stay in the service layer; this gear only enforces the
//! structural contract.
//!
//! # Behaviour when a schema is not registered
//!
//! The two AM-owned schemas diverge intentionally:
//!
//! * **User schema (`gts.cf.core.am.user.v1~`)** — fail-closed.
//!   AM does NOT persist users
//!   (`cpt-cf-account-management-constraint-no-user-storage`), so a
//!   missing schema would otherwise collapse the boundary to
//!   length-only caps and silently disable `format: email` / future
//!   `pattern` rules. The same payload would be accepted before
//!   catalog registration and rejected after — a deployment cliff.
//!   The helper returns
//!   [`DomainError::ServiceUnavailable`] instead, so
//!   `provision_user` is unavailable until the operator seeds the
//!   catalog. The dependency is declared in `gear.rs::deps`; treat
//!   schema-missing as a degraded-dependency signal rather than a
//!   silent passthrough.
//! * **Tenant name schema (`gts.cf.core.am.tenant.v1~`)** —
//!   short-circuits to `Ok(())`. The DB `CHECK (length(name) BETWEEN
//!   1 AND 255)` constraint is the last-line guard and bootstrap
//!   must succeed before the catalog is seeded.
//!
//! Failures fall into four categories (the types-registry client returns
//! [`CanonicalError`](toolkit_canonical_errors::CanonicalError) per ADR 0005):
//! * `validate_new_user_payload_via_gts` + `CanonicalError::NotFound` →
//!   [`DomainError::ServiceUnavailable`] (fail-closed, see above).
//! * `validate_tenant_name_via_gts` + `CanonicalError::NotFound` → `Ok(())`
//!   (DB CHECK + bootstrap-before-catalog ordering).
//! * Any other `CanonicalError` (transport / availability) →
//!   [`DomainError::ServiceUnavailable`] for both helpers.
//! * Schema returned but `jsonschema::validator_for` rejects it →
//!   [`DomainError::Internal`] (catalog drift — operator action
//!   required).
//! * Instance fails validation against the schema →
//!   [`DomainError::Validation`] (HTTP 400 — the client can retry
//!   with a corrected payload).
//!
//! # Plugin-private metadata is opaque to AM
//!
//! AM treats plugin metadata as a fully opaque blob persisted in
//! `tenant_idp_metadata` and echoed back on every subsequent `IdP`
//! call. The plugin owns the shape (input and output) end-to-end;
//! AM does not interpret it.

use account_management_sdk::IdpNewUser;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::TypesRegistryClient;

use crate::domain::error::DomainError;

/// GTS type id of the Account Management user resource. Pinned to the
/// same string the published JSON Schema declares as its `$id`.
pub(crate) const USER_TYPE_ID: &str = "gts.cf.core.am.user.v1~";

/// GTS type id of the Account Management tenant resource. Pinned to
/// the same string the published JSON Schema declares as its `$id`.
pub(crate) const TENANT_TYPE_ID: &str = "gts.cf.core.am.tenant.v1~";

/// Validate the structural fields of a [`IdpNewUser`] against the
/// registered `gts.cf.core.am.user.v1~` schema.
///
/// The schema describes the FULL user projection (`id`, `username`,
/// `email`, `display_name`) and pins
/// `required: ["id", "username"]` because `id` is the IdP-issued
/// authoritative identifier on read. The create payload deliberately
/// omits `id` (the `IdP` assigns it on success), so we cannot validate
/// `IdpNewUser` as a full instance — that would always fail the
/// `required` rule. Instead, fetch the schema once and run each
/// supplied STRING field against the matching property sub-schema; this
/// keeps the structural bounds (`minLength`, `maxLength`, `format`)
/// authoritative without forcing a synthetic `id`.
///
/// Response-side validation of the IdP-returned `IdpUser` is
/// intentionally NOT performed: AM trusts the plugin's published
/// contract, and a fail-closed response-side gate would either
/// break listings on a single drifted row or invent phantom-absent
/// users — both worse than the gap they would close.
///
/// AM-business rules wider than the JSON Schema (e.g. "username MUST
/// not be all-whitespace") stay in the calling service.
///
/// # Errors
///
/// See gear-level docs.
pub async fn validate_new_user_payload_via_gts(
    payload: &IdpNewUser,
    types_registry: &dyn TypesRegistryClient,
) -> Result<(), DomainError> {
    let Some(properties) = lookup_effective_properties(USER_TYPE_ID, types_registry).await? else {
        // Fail-closed: the user schema MUST be registered before
        // `provision_user` is exposed. Falling back to length-only
        // checks would silently disable `format: email` (and any
        // future `pattern` rules) until the catalog is seeded,
        // creating a deployment cliff where the same payload is
        // accepted pre-registration and rejected post-registration.
        // AM has no storage-side gate for users
        // (`cpt-cf-account-management-constraint-no-user-storage`),
        // so the boundary helper is the only structural validator on
        // the path to the `IdP` plugin — degrading it changes the
        // public contract. Surface `ServiceUnavailable` instead and
        // let the operator finish the catalog seed first.
        return Err(DomainError::service_unavailable(format!(
            "user payload validation requires `{USER_TYPE_ID}` to be registered \
             in the Types Registry; provision_user is unavailable until the \
             catalog is seeded"
        )));
    };
    validate_property_value(
        USER_TYPE_ID,
        "username",
        &Value::String(payload.username.clone()),
        &properties,
    )?;
    if let Some(email) = payload.email.as_deref() {
        validate_property_value(
            USER_TYPE_ID,
            "email",
            &Value::String(email.to_owned()),
            &properties,
        )?;
    }
    if let Some(display_name) = payload.display_name.as_deref() {
        validate_property_value(
            USER_TYPE_ID,
            "display_name",
            &Value::String(display_name.to_owned()),
            &properties,
        )?;
    }
    Ok(())
}

/// Validate a tenant `name` against the `name` sub-schema of the
/// registered `gts.cf.core.am.tenant.v1~` (`minLength: 1, maxLength:
/// 255`).
///
/// # Errors
///
/// See gear-level docs.
pub async fn validate_tenant_name_via_gts(
    name: &str,
    types_registry: &dyn TypesRegistryClient,
) -> Result<(), DomainError> {
    let Some(properties) = lookup_effective_properties(TENANT_TYPE_ID, types_registry).await?
    else {
        return Ok(());
    };
    validate_property_value(
        TENANT_TYPE_ID,
        "name",
        &Value::String(name.to_owned()),
        &properties,
    )
}

/// Fetch the effective property map for `type_id`, returning `None`
/// when the schema is not registered (so callers can short-circuit
/// to AM-side guards). See gear-level docs for the failure-mode
/// contract on other registry errors.
async fn lookup_effective_properties(
    type_id: &'static str,
    types_registry: &dyn TypesRegistryClient,
) -> Result<Option<std::collections::BTreeMap<String, Value>>, DomainError> {
    match types_registry.get_type_schema(type_id).await {
        Ok(schema) => Ok(Some(schema.effective_properties())),
        Err(CanonicalError::NotFound { .. }) => Ok(None),
        Err(err) => Err(DomainError::service_unavailable(format!(
            "GTS type schema lookup failed for `{type_id}`: {err}"
        ))),
    }
}

/// Compile the property sub-schema for `field_name` and validate
/// `field_value` against it.
///
/// A field that has no entry in the schema's `properties` map is
/// treated as **catalog drift**, not as "this field is unconstrained".
/// Every caller passes a hardcoded field name (`name`, `username`,
/// `email`, ...) that AM expects the published schema to declare;
/// a missing entry means the deployed schema is stale or
/// misregistered relative to the AM build. Surfacing this as
/// `Internal` (HTTP 500) makes the catalog-vs-code skew visible to
/// operators instead of silently disabling validation for the
/// drifted field and persisting / forwarding out-of-contract data.
fn validate_property_value(
    type_id: &'static str,
    field_name: &str,
    field_value: &Value,
    properties: &std::collections::BTreeMap<String, Value>,
) -> Result<(), DomainError> {
    let Some(field_schema) = properties.get(field_name) else {
        return Err(DomainError::Internal {
            diagnostic: format!(
                "GTS schema `{type_id}` does not declare property `{field_name}` (catalog drift: \
                 deployed schema is stale or misregistered relative to the AM build — the field \
                 is hardcoded by the AM service layer and MUST be present in the published \
                 schema's `properties` map)"
            ),
            cause: None,
        });
    };
    let validator =
        jsonschema::validator_for(field_schema).map_err(|err| DomainError::Internal {
            diagnostic: format!(
                "GTS schema `{type_id}` property `{field_name}` is not a valid JSON Schema \
             (catalog drift): {err}"
            ),
            cause: None,
        })?;
    let errors: Vec<String> = validator
        .iter_errors(field_value)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(DomainError::Validation {
            detail: format!(
                "field `{field_name}` violates `{type_id}` schema: {}",
                errors.join("; ")
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_validation_tests.rs"]
mod gts_validation_tests;

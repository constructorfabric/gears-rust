//! GTS payload validation against AM's registered resource schemas.
//!
//! Schemas under `gears/system/account-management/docs/schemas/` describe read
//! models with server-assigned fields. Validate payload properties individually
//! with `jsonschema::validator_for` so create requests need no synthetic IDs.
//! Services must use these schema bounds; additional business rules, such as
//! rejecting all-whitespace usernames, stay in the service layer.
//!
//! User validation reads through `TypesRegistryApi` under the request's
//! `SecurityContext`. The tenant-name check also serves bootstrap and takes
//! [`TypesRegistryReader`] to distinguish request and platform reads (P33).
//!
//! # Missing schemas and failures
//!
//! - Missing user schema: [`DomainError::ServiceUnavailable`] (503). AM stores
//!   no users, so accepting without schema validation would bypass format and
//!   pattern rules. Seed the catalog before provisioning users.
//! - Missing tenant-name schema: `Ok(())`; the database enforces
//!   `CHECK (length(name) BETWEEN 1 AND 255)`, including during bootstrap.
//! - Other registry errors: [`TypesRegistryReader::failure`] maps outages to
//!   503, refusals of the request caller to 401/403, and all others (including
//!   platform credential refusals) to [`DomainError::Internal`].
//! - Invalid registered schema: [`DomainError::Internal`] (catalog drift).
//! - Invalid payload: [`DomainError::Validation`] (400).
//!
//! The host must link a database-backed types-registry; as a system gear it
//! initializes before AM without a `deps` edge.
//!
//! Plugin-private metadata remains opaque: AM stores it in `tenant_idp_metadata`
//! and returns it unchanged on subsequent `IdP` calls.

use std::collections::BTreeMap;

use account_management_sdk::{IdpNewUser, IdpUserPatch};
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, LifecycleStatus, Projection, ReadOptions,
    TypesRegistryApi,
};

use crate::domain::error::DomainError;
use crate::domain::types_registry_reader::TypesRegistryReader;

/// GTS type id of the Account Management user resource. Pinned to the
/// same string the published JSON Schema declares as its `$id`.
pub(crate) const USER_TYPE_ID: &str = gts_id!("cf.core.am.user.v1~");

/// GTS type id of the Account Management tenant resource. Pinned to
/// the same string the published JSON Schema declares as its `$id`.
pub(crate) const TENANT_TYPE_ID: &str = gts_id!("cf.core.am.tenant.v1~");

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
    types_registry: &dyn TypesRegistryApi,
    ctx: &SecurityContext,
) -> Result<(), DomainError> {
    let reader = TypesRegistryReader::caller(types_registry, ctx);
    let Some(properties) = lookup_effective_properties(USER_TYPE_ID, reader).await? else {
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

/// Validate the changed string fields of an [`IdpUserPatch`] against
/// the registered `gts.cf.core.am.user.v1~` schema.
///
/// Only fields being *set to a value* are validated: a `username`
/// rename and a nullable profile field with `Some(Some(v))`. Clearing
/// a nullable field (`Some(None)`) and leaving one untouched (`None`)
/// carry no value to check. Mirrors the field set validated by
/// [`validate_new_user_payload_via_gts`] (`username`, `email`,
/// `display_name`) so the create and update paths enforce the same
/// structural bounds; `first_name` / `last_name` follow create's
/// forwarded-but-unbounded treatment, and `password` is not part of the
/// user projection schema (the `IdP` enforces its own password policy).
///
/// Fail-closed identically to [`validate_new_user_payload_via_gts`]:
/// AM persists no user state
/// (`cpt-cf-account-management-constraint-no-user-storage`), so a
/// missing schema surfaces as [`DomainError::ServiceUnavailable`]
/// rather than silently degrading to length-only caps.
///
/// # Errors
///
/// See gear-level docs.
pub async fn validate_user_patch_via_gts(
    patch: &IdpUserPatch,
    types_registry: &dyn TypesRegistryApi,
    ctx: &SecurityContext,
) -> Result<(), DomainError> {
    let reader = TypesRegistryReader::caller(types_registry, ctx);
    let Some(properties) = lookup_effective_properties(USER_TYPE_ID, reader).await? else {
        return Err(DomainError::service_unavailable(format!(
            "user patch validation requires `{USER_TYPE_ID}` to be registered \
             in the Types Registry; update_user is unavailable until the \
             catalog is seeded"
        )));
    };
    if let Some(username) = &patch.username {
        validate_property_value(
            USER_TYPE_ID,
            "username",
            &Value::String(username.clone()),
            &properties,
        )?;
    }
    if let Some(Some(email)) = &patch.email {
        validate_property_value(
            USER_TYPE_ID,
            "email",
            &Value::String(email.clone()),
            &properties,
        )?;
    }
    if let Some(Some(display_name)) = &patch.display_name {
        validate_property_value(
            USER_TYPE_ID,
            "display_name",
            &Value::String(display_name.clone()),
            &properties,
        )?;
    }
    Ok(())
}

/// Validate a tenant `name` against the `name` sub-schema of the
/// registered `gts.cf.core.am.tenant.v1~` (`minLength: 1, maxLength:
/// 255`).
///
/// `reader` names the caller: the request's context for create / update,
/// the platform API for the bootstrap saga's root name only.
///
/// # Errors
///
/// See gear-level docs.
pub async fn validate_tenant_name_via_gts(
    name: &str,
    reader: TypesRegistryReader<'_>,
) -> Result<(), DomainError> {
    let Some(properties) = lookup_effective_properties(TENANT_TYPE_ID, reader).await? else {
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
    reader: TypesRegistryReader<'_>,
) -> Result<Option<BTreeMap<String, Vec<Value>>>, DomainError> {
    let projection = Projection::Select(FieldSelection::with(&[EntityField::ResolvedSchema]));
    match reader
        .get_type_schema(
            &GtsTypeId::new(type_id),
            ReadOptions {
                projection,
                ..ReadOptions::default()
            },
        )
        .await
    {
        // A deleted schema is no longer registered for new payloads.
        Ok(schema) if schema.lifecycle_status != LifecycleStatus::Active => Ok(None),
        Ok(schema) => {
            let resolved = schema.resolved_schema.ok_or_else(|| {
                DomainError::internal(format!(
                    "types-registry answered `{type_id}` without the selected resolved_schema"
                ))
            })?;
            Ok(Some(effective_properties(&resolved)))
        }
        Err(CanonicalError::NotFound { .. }) => Ok(None),
        Err(err) => Err(reader.failure(
            err,
            format_args!("GTS type schema lookup failed for `{type_id}`"),
        )),
    }
}

/// Every sub-schema a registry-resolved schema declares for each property.
/// References are inlined at admission, so the `allOf` members carry the
/// inherited declarations. The branches of a schema are conjunctive: a
/// property declared at several places must satisfy all of them, so none
/// overrides another.
fn effective_properties(schema: &Value) -> BTreeMap<String, Vec<Value>> {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    collect_properties(schema, &mut out);
    out
}

fn collect_properties(schema: &Value, out: &mut BTreeMap<String, Vec<Value>>) {
    if let Some(own) = schema.get("properties").and_then(Value::as_object) {
        for (key, value) in own {
            out.entry(key.clone()).or_default().push(value.clone());
        }
    }
    if let Some(members) = schema.get("allOf").and_then(Value::as_array) {
        for member in members {
            collect_properties(member, out);
        }
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
    properties: &BTreeMap<String, Vec<Value>>,
) -> Result<(), DomainError> {
    let Some(field_schemas) = properties.get(field_name) else {
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
    let field_schema = match field_schemas.as_slice() {
        [single] => std::borrow::Cow::Borrowed(single),
        several => std::borrow::Cow::Owned(serde_json::json!({ "allOf": several })),
    };
    let validator =
        jsonschema::validator_for(&field_schema).map_err(|err| DomainError::Internal {
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

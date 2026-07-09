// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full
//! Shared domain validation utilities.

use crate::domain::error::DomainError;

/// GTS type path prefix required for resource group types.
pub const RG_TYPE_PREFIX: &str = "gts.cf.core.rg.type.v1~";

/// Validate a GTS type code: non-empty, correct prefix, length limit.
///
/// Input is normalized (trimmed and lowercased) before validation, consistent
/// with [`resource_group_sdk::models::GtsTypePath::new`].
///
/// # Errors
///
/// Returns [`DomainError`] if the code is empty, missing the required prefix, or exceeds 1024 chars.
// @cpt-algo:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1
// @cpt-algo:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1
pub fn validate_type_code(code: &str) -> Result<(), DomainError> {
    let code = code.trim().to_lowercase();
    let code = code.as_str();
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-1
    if code.is_empty() {
        return Err(DomainError::validation("Type code must not be empty"));
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-1
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2
    if !code.starts_with(RG_TYPE_PREFIX) {
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2a
        return Err(DomainError::validation(format!(
            "Type code must start with prefix '{RG_TYPE_PREFIX}', got: '{code}'"
        )));
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2a
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-3
    if code.chars().count() > 1024 {
        return Err(DomainError::validation(
            "Type code must not exceed 1024 characters",
        ));
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-3
    Ok(())
}

/// Validate a GTS type code used as a membership resource type.
///
/// Unlike [`validate_type_code`], this does NOT require the
/// `gts.cf.core.rg.type.v1~` prefix. Per `DESIGN.md` ("RG type prefix
/// requirement"), `allowed_memberships` entries are external domain
/// types (e.g. `gts.cf.core.idp.user.v1~`, `gts.cf.vendor.lms.course.v1~`)
/// and need not live in the RG type-registry namespace.
///
/// Format validation is delegated to [`gts::GtsID::new`], the canonical
/// GTS parser. Only **exact** GTS IDs (`gts.cf.core.idp.user.v1~`) are
/// accepted; trailing-wildcard patterns (`gts.cf.core.am.*`) are
/// rejected. `allowed_memberships` entries must resolve to a registered
/// concrete type — `gts_type_allowed_membership` is a junction table
/// with `SMALLINT FK → gts_type.id`, which cannot store a pattern.
///
/// # Errors
///
/// Returns [`DomainError::validation`] if the code is not a valid GTS
/// ID, or if it is a wildcard pattern.
pub fn validate_membership_type_code(code: &str) -> Result<(), DomainError> {
    let parsed = gts::GtsID::new(code).map_err(|e| {
        DomainError::validation(format!("Invalid membership type code '{code}': {e}"))
    })?;
    if parsed.gts_id_segments.iter().any(|seg| seg.is_wildcard) {
        return Err(DomainError::validation(format!(
            "Membership type code '{code}' must be a concrete GTS type, not a wildcard pattern"
        )));
    }
    Ok(())
}

/// Validate that a `metadata_schema` value is a valid JSON Schema.
///
/// Attempts to compile the schema via `jsonschema::validator_for`. If the value
/// cannot be interpreted as a JSON Schema, returns a [`DomainError::validation`].
///
/// # Errors
///
/// Returns [`DomainError`] if the value is not a valid JSON Schema.
// @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7
pub fn validate_metadata_schema(schema: &serde_json::Value) -> Result<(), DomainError> {
    jsonschema::validator_for(schema).map_err(|e| {
        DomainError::validation(format!("metadata_schema is not a valid JSON Schema: {e}"))
    })?;
    Ok(())
}
// @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7

/// Validate a metadata JSON value against a raw JSON Schema.
///
/// Synchronous counterpart to [`validate_metadata_via_gts`] that does not
/// resolve GTS types. When either `metadata` or `schema` is `None`, the
/// check passes trivially.
///
/// # Errors
///
/// Returns [`DomainError::validation`] when the schema fails to compile
/// or the metadata violates any schema constraint.
pub fn validate_metadata_against_schema(
    metadata: Option<&serde_json::Value>,
    schema: Option<&serde_json::Value>,
) -> Result<(), DomainError> {
    let (Some(metadata), Some(schema)) = (metadata, schema) else {
        return Ok(());
    };

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| DomainError::validation(format!("metadata_schema is invalid: {e}")))?;

    let errors: Vec<String> = validator
        .iter_errors(metadata)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(DomainError::validation(format!(
            "Metadata does not match type schema: {}",
            errors.join("; ")
        )));
    }
    Ok(())
}

/// Validate a metadata value against a resolved GTS type schema.
///
/// Uses `TypesRegistryClient` to fetch the resolved schema (with `allOf`
/// composition, `$ref` resolution, and `x-gts-traits` applied), then validates
/// the metadata against the resolved schema using `jsonschema`.
///
/// Returns `Ok(())` when:
/// - `metadata` is `None` (nothing to validate)
/// - `type_code` has no registered schema in the types registry
/// - `metadata` validates against the resolved schema
///
/// # Errors
///
/// Returns [`DomainError`] when metadata violates the schema constraints
/// or the types registry is unavailable.
pub async fn validate_metadata_via_gts(
    metadata: Option<&serde_json::Value>,
    type_code: &str,
    types_registry: &dyn types_registry_sdk::TypesRegistryClient,
) -> Result<(), DomainError> {
    let Some(metadata) = metadata else {
        return Ok(());
    };

    // Fetch the GTS schema. Local client pre-links ancestors via Arc, so
    // `effective_properties()` returns the chain-resolved property map (own
    // overrides + inherited).
    let schema = match types_registry.get_type_schema(type_code).await {
        Ok(schema) => schema,
        // No registered schema for this type -- skip metadata validation.
        // The trait boundary is `CanonicalError` (ADR 0005); a missing schema
        // surfaces as `NotFound` regardless of entity kind.
        Err(toolkit_canonical_errors::CanonicalError::NotFound { .. }) => return Ok(()),
        Err(e) => {
            return Err(DomainError::validation(format!(
                "Failed to resolve GTS type '{type_code}' for metadata validation: {e}"
            )));
        }
    };

    // The chained RG type schema may declare `metadata` at any level of
    // the inheritance chain — `effective_properties` collects them all.
    let merged = schema.effective_properties();
    let metadata_schema = merged.get("metadata");

    let Some(metadata_schema) = metadata_schema else {
        // No metadata property in the schema — any metadata accepted.
        return Ok(());
    };

    let validator = jsonschema::validator_for(metadata_schema)
        .map_err(|e| DomainError::validation(format!("Type metadata_schema is invalid: {e}")))?;

    let errors: Vec<String> = validator
        .iter_errors(metadata)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(DomainError::validation(format!(
            "Metadata does not match type schema: {}",
            errors.join("; ")
        )));
    }
    Ok(())
}
// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full

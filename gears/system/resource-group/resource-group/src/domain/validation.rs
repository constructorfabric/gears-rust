// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full
//! Shared domain validation utilities.

use crate::domain::error::DomainError;
use toolkit_gts::gts_id;

/// GTS type path prefix required for resource group types.
pub const RG_TYPE_PREFIX: &str = gts_id!("cf.core.rg.type.v1~");

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

// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full

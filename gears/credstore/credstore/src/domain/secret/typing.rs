//! Secret-type trait enforcement (design §5.4, write-path checks).
//!
//! Validates a write against the type's registry-resolved traits
//! ([`credstore_sdk::SecretTypeTraits`], see
//! [`crate::domain::secret::type_resolver`]): permitted sharing modes,
//! value size, UTF-8 requirement, the `value_schema` JSON Schema trait,
//! and the expiry gate. Violations map to [`DomainError::TypeViolation`]
//! with a stable reason code (canonical 400 on the wire).

use credstore_sdk::{SecretTypeTraits, SecretValue, SharingMode};
use time::OffsetDateTime;

use crate::domain::error::DomainError;

/// Stable reason codes (wire-visible in the canonical field violation).
pub mod reasons {
    pub const UNKNOWN_SECRET_TYPE: &str = "UNKNOWN_SECRET_TYPE";
    pub const SHARING_NOT_ALLOWED_FOR_TYPE: &str = "SHARING_NOT_ALLOWED_FOR_TYPE";
    pub const VALUE_TOO_LARGE: &str = "VALUE_TOO_LARGE";
    pub const VALUE_NOT_UTF8: &str = "VALUE_NOT_UTF8";
    pub const VALUE_SCHEMA_VIOLATION: &str = "VALUE_SCHEMA_VIOLATION";
    pub const EXPIRY_NOT_SUPPORTED_FOR_TYPE: &str = "EXPIRY_NOT_SUPPORTED_FOR_TYPE";
    pub const EXPIRY_IN_THE_PAST: &str = "EXPIRY_IN_THE_PAST";
    pub const TYPE_IMMUTABLE: &str = "TYPE_IMMUTABLE";
}

fn violation(field: &'static str, reason: &'static str, detail: String) -> DomainError {
    DomainError::TypeViolation {
        field,
        reason,
        detail,
    }
}

/// Validate a write against the type's resolved traits. `type_id` is the
/// type's full GTS id, used in wire-visible violation details.
///
/// `expires_at` semantics: permitted only for `expirable` types; a value in
/// the past is rejected (it would create a secret that never resolves).
///
/// # Errors
///
/// Returns [`DomainError::TypeViolation`] with a stable reason on the first
/// violated trait, or [`DomainError::ServiceUnavailable`] when the type's
/// registered `value_schema` trait fails to compile (a broken registration,
/// not a caller error — fail closed).
pub fn validate_write(
    type_id: &str,
    traits: &SecretTypeTraits,
    sharing: SharingMode,
    value: &SecretValue,
    expires_at: Option<OffsetDateTime>,
) -> Result<(), DomainError> {
    if !traits.allows_sharing(sharing) {
        return Err(violation(
            "sharing",
            reasons::SHARING_NOT_ALLOWED_FOR_TYPE,
            format!("sharing mode {sharing:?} is not permitted for secret type '{type_id}'"),
        ));
    }

    // A value too large for u64 is definitely over any declared limit.
    let len = u64::try_from(value.as_bytes().len()).unwrap_or(u64::MAX);
    if let Some(max) = traits.max_size_bytes
        && len > max
    {
        return Err(violation(
            "value",
            reasons::VALUE_TOO_LARGE,
            format!("value of {len} bytes exceeds the {max}-byte limit of secret type '{type_id}'"),
        ));
    }

    if traits.utf8_only && std::str::from_utf8(value.as_bytes()).is_err() {
        return Err(violation(
            "value",
            reasons::VALUE_NOT_UTF8,
            format!("secret type '{type_id}' requires a valid UTF-8 value"),
        ));
    }

    if let Some(schema) = traits.value_schema.as_ref() {
        validate_value_schema(type_id, schema, value)?;
    }

    match expires_at {
        Some(_) if !traits.expirable => {
            return Err(violation(
                "expires_at",
                reasons::EXPIRY_NOT_SUPPORTED_FOR_TYPE,
                format!("secret type '{type_id}' does not support expiry"),
            ));
        }
        Some(at) if at <= OffsetDateTime::now_utc() => {
            return Err(violation(
                "expires_at",
                reasons::EXPIRY_IN_THE_PAST,
                "expires_at must be in the future".to_owned(),
            ));
        }
        _ => {}
    }

    Ok(())
}

/// Validate the value against the type's `value_schema` trait. The value
/// must parse as JSON; violation details never echo the value itself (only
/// schema paths), preserving the no-secret-logging posture.
///
/// The validator is compiled per call: schemas are dynamic (they arrive
/// with the registry resolution, which the registry client TTL-caches),
/// small, and only types that declare one pay the cost.
fn validate_value_schema(
    type_id: &str,
    schema: &serde_json::Value,
    value: &SecretValue,
) -> Result<(), DomainError> {
    let parsed: serde_json::Value = serde_json::from_slice(value.as_bytes()).map_err(|_| {
        violation(
            "value",
            reasons::VALUE_SCHEMA_VIOLATION,
            format!("secret type '{type_id}' requires a JSON value matching its schema"),
        )
    })?;

    // A non-compiling schema is a broken type registration (the registry
    // validated the trait as JSON, not as a JSON Schema): operator-fixable
    // by re-registering the type, so 503, not 400/500.
    let validator = jsonschema::validator_for(schema).map_err(|e| {
        tracing::warn!(type_id = %type_id, err = %e, "secret type value_schema failed to compile");
        DomainError::ServiceUnavailable {
            detail: format!("secret type '{type_id}' has a malformed value schema"),
            retry_after: None,
            cause: None,
        }
    })?;
    if let Err(first) = validator.validate(&parsed) {
        // instance_path only — never the offending value.
        return Err(violation(
            "value",
            reasons::VALUE_SCHEMA_VIOLATION,
            format!(
                "value does not match the '{type_id}' schema at '{}': {}",
                first.instance_path(),
                redact_schema_error(&first)
            ),
        ));
    }
    Ok(())
}

/// Keep only the violation *kind* out of a schema error — jsonschema's
/// Display can embed the offending instance fragment, which may be secret
/// material.
fn redact_schema_error(err: &jsonschema::ValidationError<'_>) -> String {
    format!("violated `{}`", err.schema_path())
}

#[cfg(test)]
#[path = "typing_tests.rs"]
mod typing_tests;

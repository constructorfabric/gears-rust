//! Wire `field` / `reason` vocabulary for field violations under
//! [`CanonicalError::InvalidArgument`].
//!
//! Each `reason` constant is a stable machine-readable code that lands
//! in `CanonicalError::InvalidArgument.ctx.field_violations[].reason`
//! when Account Management rejects a request field; each `*_FIELD`
//! constant is the corresponding `field_violations[].field` attribution.
//!
//! Consumers dispatch on these to disposition validation errors without
//! string-typing literals. The impl crate's single
//! `From<DomainError> for CanonicalError` ladder references the same
//! constants at construction time so the SDK vocabulary and the wire can
//! never drift — the round-trip tests in [`crate::error`] pin every
//! constant to its `Problem` JSON path.
//!
//! [`CanonicalError::InvalidArgument`]: toolkit_canonical_errors::CanonicalError::InvalidArgument

use core::fmt;

// ---------------------------------------------------------------------------
// `field_violations[].reason` codes (the dispatch discriminator).
// ---------------------------------------------------------------------------

/// `tenant_type` reference is malformed or unknown.
pub const INVALID_TENANT_TYPE: &str = "INVALID_TENANT_TYPE";

/// Generic request-shape validation failure (tenant-state guards,
/// metadata-payload rejects). The catch-all reason when no more
/// specific code applies.
pub const VALIDATION: &str = "VALIDATION";

/// Delete refused: the platform root tenant cannot be deleted.
pub const ROOT_TENANT_CANNOT_DELETE: &str = "ROOT_TENANT_CANNOT_DELETE";

/// Conversion refused: the platform root tenant cannot be converted.
pub const ROOT_TENANT_CANNOT_CONVERT: &str = "ROOT_TENANT_CANNOT_CONVERT";

/// Suspend / unsuspend refused: the platform root tenant's lifecycle
/// status is immutable from the public API.
pub const ROOT_TENANT_CANNOT_CHANGE_STATUS: &str = "ROOT_TENANT_CANNOT_CHANGE_STATUS";

/// The configured `IdP` plugin rejected the provisioning request shape
/// before any external call. The accompanying `field` carries the
/// dotted-path the plugin localised the violation to (e.g.
/// `provisioning_metadata.realm_name`) — see [`PROVISIONING_METADATA_FIELD`].
pub const IDP_INVALID_INPUT: &str = "IDP_INVALID_INPUT";

/// The `IdP` rejected the supplied password against its configured
/// password policy. Carried on the [`PASSWORD_FIELD`]
/// field-violation so clients can attribute the failure to the
/// password input; the raw policy text stays provider-side.
pub const PASSWORD_POLICY: &str = "PASSWORD_POLICY";

/// The patched attribute is managed by the `IdP` and not overridable
/// through AM (federated from a read-only mapper, or marked
/// non-writable in the provider's user-profile configuration). The
/// accompanying `field` is the exact `UserUpdateRequest` property the
/// caller tried to write, sourced from
/// [`crate::IdpUserAttribute::as_field_token`], so a client can
/// attribute the refusal to one input and disable it.
///
/// A capability fact about the field, not an authorization decision
/// about the caller — hence `InvalidArgument` (400) rather than
/// `PermissionDenied` (403): no grant would make the write succeed.
pub const IDP_MANAGED_FIELD: &str = "IDP_MANAGED_FIELD";

// ---------------------------------------------------------------------------
// `field_violations[].field` attribution keys.
//
// Not a dispatch discriminator — these identify *which* request field
// failed. Extracted to consts (ADR 0005 Rule 6) so the impl ladder and
// the SDK vocabulary cannot drift; pinned by the round-trip tests.
// ---------------------------------------------------------------------------

/// `tenant_type` reference field (carries [`INVALID_TENANT_TYPE`]).
pub const TENANT_TYPE_FIELD: &str = "tenant_type";

/// Whole-request field for generic tenant-state validation rejects
/// (carries [`VALIDATION`]).
pub const REQUEST_FIELD: &str = "request";

/// Metadata-payload field for metadata-content validation rejects
/// (carries [`VALIDATION`], routed to the metadata resource type).
pub const METADATA_FIELD: &str = "metadata";

/// `tenant_id` field for the root-tenant protection rejects
/// ([`ROOT_TENANT_CANNOT_DELETE`] / [`ROOT_TENANT_CANNOT_CONVERT`] /
/// [`ROOT_TENANT_CANNOT_CHANGE_STATUS`]).
pub const TENANT_ID_FIELD: &str = "tenant_id";

/// `password` field for `IdP` password-policy rejects (carries
/// [`PASSWORD_POLICY`]).
pub const PASSWORD_FIELD: &str = "password";

/// Shared fallback field for [`IDP_INVALID_INPUT`] when the `IdP`
/// plugin cannot localise the violation to a specific sub-key — the
/// public surface every `IdP` plugin shares.
pub const PROVISIONING_METADATA_FIELD: &str = "provisioning_metadata";

// ---------------------------------------------------------------------------
// Typed view of the `field_violations[].reason` codes.
// ---------------------------------------------------------------------------

/// Typed view of the AM `InvalidArgument` field-violation `reason`
/// strings declared above.
///
/// Carried by [`crate::AccountManagementError::InvalidArgument::reason`].
/// `from_wire` returns `Self` (not `Option`) with an [`Self::Unknown`]
/// catch-all because every `reason` AM emits under `InvalidArgument` is
/// one of the modeled codes — the catch-all only fires for a future code
/// added after this SDK version, keeping the projection forward-compatible.
///
/// `#[non_exhaustive]`: AM grows its `InvalidArgument` reason vocabulary
/// over time, so a downstream `match` MUST keep a wildcard arm and a new
/// reason code is not a breaking SDK release. [`Self::Unknown`] covers the
/// same growth on the *value* side (an older SDK reading a newer wire
/// string); this attribute covers it on the *type* side.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationReason {
    /// See [`INVALID_TENANT_TYPE`].
    InvalidTenantType,
    /// See [`VALIDATION`].
    Validation,
    /// See [`ROOT_TENANT_CANNOT_DELETE`].
    RootTenantCannotDelete,
    /// See [`ROOT_TENANT_CANNOT_CONVERT`].
    RootTenantCannotConvert,
    /// See [`ROOT_TENANT_CANNOT_CHANGE_STATUS`].
    RootTenantCannotChangeStatus,
    /// See [`IDP_INVALID_INPUT`].
    IdpInvalidInput,
    /// See [`PASSWORD_POLICY`].
    PasswordPolicy,
    /// See [`IDP_MANAGED_FIELD`].
    IdpManagedField,
    /// Unmodeled / future reason — preserves the raw wire string.
    Unknown(String),
}

impl ValidationReason {
    /// Project a wire `field_violations[].reason` string into the typed
    /// discriminator. Any unmodeled value is preserved in
    /// [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            INVALID_TENANT_TYPE => Self::InvalidTenantType,
            VALIDATION => Self::Validation,
            ROOT_TENANT_CANNOT_DELETE => Self::RootTenantCannotDelete,
            ROOT_TENANT_CANNOT_CONVERT => Self::RootTenantCannotConvert,
            ROOT_TENANT_CANNOT_CHANGE_STATUS => Self::RootTenantCannotChangeStatus,
            IDP_INVALID_INPUT => Self::IdpInvalidInput,
            PASSWORD_POLICY => Self::PasswordPolicy,
            IDP_MANAGED_FIELD => Self::IdpManagedField,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `reason` string.
    /// Inverse of [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::InvalidTenantType => INVALID_TENANT_TYPE,
            Self::Validation => VALIDATION,
            Self::RootTenantCannotDelete => ROOT_TENANT_CANNOT_DELETE,
            Self::RootTenantCannotConvert => ROOT_TENANT_CANNOT_CONVERT,
            Self::RootTenantCannotChangeStatus => ROOT_TENANT_CANNOT_CHANGE_STATUS,
            Self::IdpInvalidInput => IDP_INVALID_INPUT,
            Self::PasswordPolicy => PASSWORD_POLICY,
            Self::IdpManagedField => IDP_MANAGED_FIELD,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_reason_round_trips_each_constant() {
        for (wire, expected) in [
            (INVALID_TENANT_TYPE, ValidationReason::InvalidTenantType),
            (VALIDATION, ValidationReason::Validation),
            (
                ROOT_TENANT_CANNOT_DELETE,
                ValidationReason::RootTenantCannotDelete,
            ),
            (
                ROOT_TENANT_CANNOT_CONVERT,
                ValidationReason::RootTenantCannotConvert,
            ),
            (
                ROOT_TENANT_CANNOT_CHANGE_STATUS,
                ValidationReason::RootTenantCannotChangeStatus,
            ),
            (IDP_INVALID_INPUT, ValidationReason::IdpInvalidInput),
            (PASSWORD_POLICY, ValidationReason::PasswordPolicy),
            (IDP_MANAGED_FIELD, ValidationReason::IdpManagedField),
        ] {
            assert_eq!(ValidationReason::from_wire(wire), expected);
            assert_eq!(expected.as_wire(), wire);
        }
    }

    #[test]
    fn validation_reason_preserves_unknown_wire_string() {
        let raw = "FUTURE_VALIDATION_CODE";
        let r = ValidationReason::from_wire(raw);
        assert_eq!(r, ValidationReason::Unknown(raw.to_owned()));
        assert_eq!(r.as_wire(), raw);
    }
}

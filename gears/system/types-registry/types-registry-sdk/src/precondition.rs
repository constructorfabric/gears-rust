//! Wire `type` vocabulary for precondition violations under
//! [`CanonicalError::FailedPrecondition`].
//!
//! The legacy `FailedPrecondition` shape is produced by the legacy
//! local client's batch-registration parent pre-check: an entity cannot be
//! registered because its required parent type-schema is not yet registered.
//! That disposition has **no `DomainError` / REST-ladder arm** — it is an
//! adapter-only, in-process concept — so it is constructed directly on the
//! in-process boundary and carried losslessly in canonical's typed slots:
//!
//! * `violations[].type` = [`PARENT_NOT_REGISTERED`] (the discriminator),
//! * `violations[].subject` = the missing **parent** type-schema id,
//! * `resource_name` = the **dependent** entity id that failed,
//! * `violations[].description` = the human message.
//!
//! The projection ([`crate::error::TypesRegistryError::ParentNotRegistered`])
//! reconstructs `{ parent_type_id, dependent_id }` from exactly those slots, so
//! the structured batch-registration outcome survives the canonical round-trip.
//! The round-trip tests in [`crate::error`] pin the constant to its `Problem`
//! JSON path.
//!
//! Two other `FailedPrecondition` shapes share this envelope: a registration-policy
//! refusal ([`REGISTRATION_POLICY_PREFIX`], typed by [`PolicyParameter`]) and an item
//! failure ([`crate::item_failure::AdmissionFailure`]). Their decoders refuse each
//! other's shapes.
//!
//! [`CanonicalError::FailedPrecondition`]: toolkit_canonical_errors::CanonicalError::FailedPrecondition

/// The `violations[].type` token types-registry emits for a
/// parent-type-schema-not-registered precondition failure.
///
/// It is the discriminator the projection keys on to reconstruct
/// [`crate::error::TypesRegistryError::ParentNotRegistered`].
pub const PARENT_NOT_REGISTERED: &str = "PARENT_NOT_REGISTERED";

/// Prefix of the `violations[].type` of a registration-policy refusal; the rest is the
/// refused policy parameter in upper case (`REGISTRATION_POLICY_ALLOWED_VENDORS`).
///
/// The refusal names the candidate in `resource_name` and the policy region (or
/// [`DEFAULT_REGION`]) in `violations[].subject`.
pub const REGISTRATION_POLICY_PREFIX: &str = "REGISTRATION_POLICY_";

/// The `violations[].subject` of a policy refusal made by the default policy, which
/// belongs to no named region.
pub const DEFAULT_REGION: &str = "<default>";

/// The candidate's vendor is not in the region's `allowed_vendors`.
pub const REGISTRATION_POLICY_ALLOWED_VENDORS: &str = "REGISTRATION_POLICY_ALLOWED_VENDORS";

/// The candidate is not allowed by the region's `tenant_ownable` setting.
pub const REGISTRATION_POLICY_TENANT_OWNABLE: &str = "REGISTRATION_POLICY_TENANT_OWNABLE";

/// Typed view of a registration-policy `violations[].type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyParameter {
    /// See [`REGISTRATION_POLICY_ALLOWED_VENDORS`].
    AllowedVendors,
    /// See [`REGISTRATION_POLICY_TENANT_OWNABLE`].
    TenantOwnable,
    /// A policy-prefixed code this build does not know; the full code is preserved.
    Unknown(String),
}

impl PolicyParameter {
    /// Project a `violations[].type`; `None` unless it carries [`REGISTRATION_POLICY_PREFIX`].
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            REGISTRATION_POLICY_ALLOWED_VENDORS => Some(Self::AllowedVendors),
            REGISTRATION_POLICY_TENANT_OWNABLE => Some(Self::TenantOwnable),
            other if other.starts_with(REGISTRATION_POLICY_PREFIX) => {
                Some(Self::Unknown(other.to_owned()))
            }
            _ => None,
        }
    }

    /// Render back to the wire `violations[].type`. Inverse of [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::AllowedVendors => REGISTRATION_POLICY_ALLOWED_VENDORS,
            Self::TenantOwnable => REGISTRATION_POLICY_TENANT_OWNABLE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl core::fmt::Display for PolicyParameter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_wire())
    }
}

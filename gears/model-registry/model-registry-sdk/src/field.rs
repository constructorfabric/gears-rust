// Created: 2026-10-26 by Constructor Tech
// Updated: 2026-11-12 by Constructor Tech
//! Field-violation reason codes of `InvalidArgument` errors
//! (`context.field_violations[].reason`, DESIGN §4 Error Handling).

/// A field value has the wrong format (slug, name, URL, secret reference,
/// MIME type, …).
pub const INVALID_FORMAT: &str = "INVALID_FORMAT";
/// A string or JSON document exceeds its bound.
pub const TOO_LARGE: &str = "TOO_LARGE";
/// An update request carries an immutable field.
pub const IMMUTABLE_FIELD: &str = "IMMUTABLE_FIELD";
/// `gts_type` does not parse as a GTS type id.
pub const INVALID_GTS_TYPE: &str = "INVALID_GTS_TYPE";
/// `gts_type` is not derived from the provider base type.
pub const NOT_A_PROVIDER_TYPE: &str = "NOT_A_PROVIDER_TYPE";
/// Types Registry has no active schema for the provider type.
pub const GTS_TYPE_NOT_REGISTERED: &str = "GTS_TYPE_NOT_REGISTERED";
/// `provider_settings` violates the provider type schema. On a model write
/// the field is `provider_settings`; on a provider type migration the field
/// is `gts_type` and the description names the model. The description
/// carries the validator messages.
pub const SCHEMA_MISMATCH: &str = "SCHEMA_MISMATCH";
/// `CredStore` has no record under `secret_ref`.
pub const SECRET_REF_NOT_FOUND: &str = "SECRET_REF_NOT_FOUND";
/// The `CredStore` record under `secret_ref` holds no secret owned by the
/// writing tenant.
pub const SECRET_REF_NOT_OWNED: &str = "SECRET_REF_NOT_OWNED";
/// A REST `PATCH` without an `If-Match` header; the field is `If-Match`.
pub const IF_MATCH_REQUIRED: &str = "IF_MATCH_REQUIRED";

/// Typed view of the reason codes above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Reason {
    InvalidFormat,
    TooLarge,
    ImmutableField,
    InvalidGtsType,
    NotAProviderType,
    GtsTypeNotRegistered,
    SchemaMismatch,
    SecretRefNotFound,
    SecretRefNotOwned,
    IfMatchRequired,
}

impl Reason {
    /// Every variant, in declaration order.
    pub const ALL: &[Self] = &[
        Self::InvalidFormat,
        Self::TooLarge,
        Self::ImmutableField,
        Self::InvalidGtsType,
        Self::NotAProviderType,
        Self::GtsTypeNotRegistered,
        Self::SchemaMismatch,
        Self::SecretRefNotFound,
        Self::SecretRefNotOwned,
        Self::IfMatchRequired,
    ];

    /// `None` for a reason the registry does not emit (for example a
    /// toolkit `OData` reason).
    #[must_use]
    pub fn from_wire(reason: &str) -> Option<Self> {
        match reason {
            INVALID_FORMAT => Some(Self::InvalidFormat),
            TOO_LARGE => Some(Self::TooLarge),
            IMMUTABLE_FIELD => Some(Self::ImmutableField),
            INVALID_GTS_TYPE => Some(Self::InvalidGtsType),
            NOT_A_PROVIDER_TYPE => Some(Self::NotAProviderType),
            GTS_TYPE_NOT_REGISTERED => Some(Self::GtsTypeNotRegistered),
            SCHEMA_MISMATCH => Some(Self::SchemaMismatch),
            SECRET_REF_NOT_FOUND => Some(Self::SecretRefNotFound),
            SECRET_REF_NOT_OWNED => Some(Self::SecretRefNotOwned),
            IF_MATCH_REQUIRED => Some(Self::IfMatchRequired),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::InvalidFormat => INVALID_FORMAT,
            Self::TooLarge => TOO_LARGE,
            Self::ImmutableField => IMMUTABLE_FIELD,
            Self::InvalidGtsType => INVALID_GTS_TYPE,
            Self::NotAProviderType => NOT_A_PROVIDER_TYPE,
            Self::GtsTypeNotRegistered => GTS_TYPE_NOT_REGISTERED,
            Self::SchemaMismatch => SCHEMA_MISMATCH,
            Self::SecretRefNotFound => SECRET_REF_NOT_FOUND,
            Self::SecretRefNotOwned => SECRET_REF_NOT_OWNED,
            Self::IfMatchRequired => IF_MATCH_REQUIRED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_round_trips_through_wire() {
        for reason in Reason::ALL {
            assert_eq!(Reason::from_wire(reason.as_wire()), Some(*reason));
        }
        assert_eq!(Reason::from_wire("UNKNOWN_FIELD"), None);
    }
}

//! Wire `reason` vocabulary for field violations under
//! [`CanonicalError::InvalidArgument`].
//!
//! Each constant is a stable machine-readable code that lands in
//! `CanonicalError::InvalidArgument.ctx.field_violations[].reason` when the
//! credstore gear rejects a request field. The round-trip tests in
//! `crate::error` pin every value; the gear will build its errors from these
//! constants when the gear code lands (a later step).
//!
//! [`Reason`] is the typed view the [`CredStoreError::Invalid`] projection
//! carries.
//!
//! [`CanonicalError::InvalidArgument`]: toolkit_canonical_errors::CanonicalError::InvalidArgument
//! [`CredStoreError::Invalid`]: crate::CredStoreError::Invalid

use core::fmt;

// -------------------------------------------------------------------------
// Reference: Reference syntax
// -------------------------------------------------------------------------

/// The secret reference is empty, too long or has characters outside `[a-zA-Z0-9_-]`. Field `reference`.
pub const INVALID_SECRET_REF: &str = "INVALID_SECRET_REF";

// -------------------------------------------------------------------------
// Type rules: Violations of the secret type's traits
// -------------------------------------------------------------------------

/// The named secret type is not in the catalog or the registry.
pub const UNKNOWN_SECRET_TYPE: &str = "UNKNOWN_SECRET_TYPE";

/// The requested sharing mode is not allowed for the secret type.
pub const SHARING_NOT_ALLOWED_FOR_TYPE: &str = "SHARING_NOT_ALLOWED_FOR_TYPE";

/// The secret value exceeds the type's size limit.
pub const VALUE_TOO_LARGE: &str = "VALUE_TOO_LARGE";

/// The type requires a UTF-8 value and the bytes are not.
pub const VALUE_NOT_UTF8: &str = "VALUE_NOT_UTF8";

/// The value does not satisfy the type's value schema.
pub const VALUE_SCHEMA_VIOLATION: &str = "VALUE_SCHEMA_VIOLATION";

/// `expires_at` was set on a type that does not expire.
pub const EXPIRY_NOT_SUPPORTED_FOR_TYPE: &str = "EXPIRY_NOT_SUPPORTED_FOR_TYPE";

/// `expires_at` lies in the past.
pub const EXPIRY_IN_THE_PAST: &str = "EXPIRY_IN_THE_PAST";

// -------------------------------------------------------------------------
// Request shape: Requests that are malformed independently of any secret type
// -------------------------------------------------------------------------

/// A `patch` that touches nothing.
pub const EMPTY_PATCH: &str = "EMPTY_PATCH";

/// A create that names no secret type.
pub const TYPE_REQUIRED: &str = "TYPE_REQUIRED";

// -------------------------------------------------------------------------
// List query: `list` query parameters
// -------------------------------------------------------------------------

/// `$filter` uses an unsupported field, operator or value.
pub const INVALID_FILTER: &str = "INVALID_FILTER";

/// `$orderby` names a field other than `reference`.
pub const INVALID_ORDERBY_FIELD: &str = "INVALID_ORDERBY_FIELD";

/// `$select` names an unknown field.
pub const INVALID_SELECT: &str = "INVALID_SELECT";

/// `$select` containing `secret` is combined with a field set the read does not allow.
pub const SECRET_SELECT_FIELDS: &str = "SECRET_SELECT_FIELDS";

/// The cursor is malformed.
pub const INVALID_CURSOR: &str = "INVALID_CURSOR";

/// `limit` is out of range.
pub const INVALID_LIMIT: &str = "INVALID_LIMIT";

/// The cursor was minted under a different order.
pub const ORDER_MISMATCH: &str = "ORDER_MISMATCH";

/// The cursor was minted under a different filter.
pub const FILTER_MISMATCH: &str = "FILTER_MISMATCH";

/// `$orderby` is supplied together with a cursor.
pub const ORDER_WITH_CURSOR: &str = "ORDER_WITH_CURSOR";

// -------------------------------------------------------------------------
// REST-only: Codes only the REST layer emits; the typed in-process API cannot express them
// -------------------------------------------------------------------------

/// A required `secret` is absent.
pub const SECRET_REQUIRED: &str = "SECRET_REQUIRED";

/// An explicit `null` where the field does not accept one.
pub const NULL_NOT_ALLOWED: &str = "NULL_NOT_ALLOWED";

/// `PUT` sent neither `If-None-Match` nor `If-Match`, or both (field
/// `If-Match`); or `If-None-Match` was sent on `PATCH` (field `If-None-Match`).
/// A `PATCH`/`DELETE` without `If-Match` is [`IF_MATCH_REQUIRED`].
pub const PRECONDITION_REQUIRED: &str = "PRECONDITION_REQUIRED";

/// The `If-Match` header is malformed. Field `If-Match`.
pub const INVALID_IF_MATCH: &str = "INVALID_IF_MATCH";

/// A `PATCH` or `DELETE` without the mandatory `If-Match` header. Field
/// `If-Match`.
pub const IF_MATCH_REQUIRED: &str = "IF_MATCH_REQUIRED";

/// `expires_at` is not an RFC 3339 timestamp.
pub const INVALID_EXPIRES_AT: &str = "INVALID_EXPIRES_AT";

/// The `PATCH` body is not a readable JSON object. Field `body`.
pub const INVALID_MERGE_PATCH_BODY: &str = "INVALID_MERGE_PATCH_BODY";

/// `PATCH` without `Content-Type: application/merge-patch+json`; the REST layer pins HTTP 415. Field `Content-Type`.
pub const UNSUPPORTED_MEDIA_TYPE: &str = "UNSUPPORTED_MEDIA_TYPE";

/// Typed view of the field-violation `reason` codes above.
///
/// [`Self::Unknown`] preserves any value this SDK version does not model, so
/// the projection stays forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// See [`INVALID_SECRET_REF`].
    InvalidSecretRef,
    /// See [`UNKNOWN_SECRET_TYPE`].
    UnknownSecretType,
    /// See [`SHARING_NOT_ALLOWED_FOR_TYPE`].
    SharingNotAllowedForType,
    /// See [`VALUE_TOO_LARGE`].
    ValueTooLarge,
    /// See [`VALUE_NOT_UTF8`].
    ValueNotUtf8,
    /// See [`VALUE_SCHEMA_VIOLATION`].
    ValueSchemaViolation,
    /// See [`EXPIRY_NOT_SUPPORTED_FOR_TYPE`].
    ExpiryNotSupportedForType,
    /// See [`EXPIRY_IN_THE_PAST`].
    ExpiryInThePast,
    /// See [`EMPTY_PATCH`].
    EmptyPatch,
    /// See [`TYPE_REQUIRED`].
    TypeRequired,
    /// See [`INVALID_FILTER`].
    InvalidFilter,
    /// See [`INVALID_ORDERBY_FIELD`].
    InvalidOrderbyField,
    /// See [`INVALID_SELECT`].
    InvalidSelect,
    /// See [`SECRET_SELECT_FIELDS`].
    SecretSelectFields,
    /// See [`INVALID_CURSOR`].
    InvalidCursor,
    /// See [`INVALID_LIMIT`].
    InvalidLimit,
    /// See [`ORDER_MISMATCH`].
    OrderMismatch,
    /// See [`FILTER_MISMATCH`].
    FilterMismatch,
    /// See [`ORDER_WITH_CURSOR`].
    OrderWithCursor,
    /// See [`SECRET_REQUIRED`].
    SecretRequired,
    /// See [`NULL_NOT_ALLOWED`].
    NullNotAllowed,
    /// See [`PRECONDITION_REQUIRED`].
    PreconditionRequired,
    /// See [`INVALID_IF_MATCH`].
    InvalidIfMatch,
    /// See [`IF_MATCH_REQUIRED`].
    IfMatchRequired,
    /// See [`INVALID_EXPIRES_AT`].
    InvalidExpiresAt,
    /// See [`INVALID_MERGE_PATCH_BODY`].
    InvalidMergePatchBody,
    /// See [`UNSUPPORTED_MEDIA_TYPE`].
    UnsupportedMediaType,
    /// Unmodeled reason; preserves the raw wire string.
    Unknown(String),
}

impl Reason {
    /// Project a wire `field_violations[].reason` string into the typed
    /// discriminator. A value this SDK does not know maps to
    /// [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            INVALID_SECRET_REF => Self::InvalidSecretRef,
            UNKNOWN_SECRET_TYPE => Self::UnknownSecretType,
            SHARING_NOT_ALLOWED_FOR_TYPE => Self::SharingNotAllowedForType,
            VALUE_TOO_LARGE => Self::ValueTooLarge,
            VALUE_NOT_UTF8 => Self::ValueNotUtf8,
            VALUE_SCHEMA_VIOLATION => Self::ValueSchemaViolation,
            EXPIRY_NOT_SUPPORTED_FOR_TYPE => Self::ExpiryNotSupportedForType,
            EXPIRY_IN_THE_PAST => Self::ExpiryInThePast,
            EMPTY_PATCH => Self::EmptyPatch,
            TYPE_REQUIRED => Self::TypeRequired,
            INVALID_FILTER => Self::InvalidFilter,
            INVALID_ORDERBY_FIELD => Self::InvalidOrderbyField,
            INVALID_SELECT => Self::InvalidSelect,
            SECRET_SELECT_FIELDS => Self::SecretSelectFields,
            INVALID_CURSOR => Self::InvalidCursor,
            INVALID_LIMIT => Self::InvalidLimit,
            ORDER_MISMATCH => Self::OrderMismatch,
            FILTER_MISMATCH => Self::FilterMismatch,
            ORDER_WITH_CURSOR => Self::OrderWithCursor,
            SECRET_REQUIRED => Self::SecretRequired,
            NULL_NOT_ALLOWED => Self::NullNotAllowed,
            PRECONDITION_REQUIRED => Self::PreconditionRequired,
            INVALID_IF_MATCH => Self::InvalidIfMatch,
            IF_MATCH_REQUIRED => Self::IfMatchRequired,
            INVALID_EXPIRES_AT => Self::InvalidExpiresAt,
            INVALID_MERGE_PATCH_BODY => Self::InvalidMergePatchBody,
            UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `reason` string. Inverse of
    /// [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::InvalidSecretRef => INVALID_SECRET_REF,
            Self::UnknownSecretType => UNKNOWN_SECRET_TYPE,
            Self::SharingNotAllowedForType => SHARING_NOT_ALLOWED_FOR_TYPE,
            Self::ValueTooLarge => VALUE_TOO_LARGE,
            Self::ValueNotUtf8 => VALUE_NOT_UTF8,
            Self::ValueSchemaViolation => VALUE_SCHEMA_VIOLATION,
            Self::ExpiryNotSupportedForType => EXPIRY_NOT_SUPPORTED_FOR_TYPE,
            Self::ExpiryInThePast => EXPIRY_IN_THE_PAST,
            Self::EmptyPatch => EMPTY_PATCH,
            Self::TypeRequired => TYPE_REQUIRED,
            Self::InvalidFilter => INVALID_FILTER,
            Self::InvalidOrderbyField => INVALID_ORDERBY_FIELD,
            Self::InvalidSelect => INVALID_SELECT,
            Self::SecretSelectFields => SECRET_SELECT_FIELDS,
            Self::InvalidCursor => INVALID_CURSOR,
            Self::InvalidLimit => INVALID_LIMIT,
            Self::OrderMismatch => ORDER_MISMATCH,
            Self::FilterMismatch => FILTER_MISMATCH,
            Self::OrderWithCursor => ORDER_WITH_CURSOR,
            Self::SecretRequired => SECRET_REQUIRED,
            Self::NullNotAllowed => NULL_NOT_ALLOWED,
            Self::PreconditionRequired => PRECONDITION_REQUIRED,
            Self::InvalidIfMatch => INVALID_IF_MATCH,
            Self::IfMatchRequired => IF_MATCH_REQUIRED,
            Self::InvalidExpiresAt => INVALID_EXPIRES_AT,
            Self::InvalidMergePatchBody => INVALID_MERGE_PATCH_BODY,
            Self::UnsupportedMediaType => UNSUPPORTED_MEDIA_TYPE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// Every constant of this module, in declaration order.
#[cfg(test)]
pub(crate) const ALL: &[&str] = &[
    INVALID_SECRET_REF,
    UNKNOWN_SECRET_TYPE,
    SHARING_NOT_ALLOWED_FOR_TYPE,
    VALUE_TOO_LARGE,
    VALUE_NOT_UTF8,
    VALUE_SCHEMA_VIOLATION,
    EXPIRY_NOT_SUPPORTED_FOR_TYPE,
    EXPIRY_IN_THE_PAST,
    EMPTY_PATCH,
    TYPE_REQUIRED,
    INVALID_FILTER,
    INVALID_ORDERBY_FIELD,
    INVALID_SELECT,
    SECRET_SELECT_FIELDS,
    INVALID_CURSOR,
    INVALID_LIMIT,
    ORDER_MISMATCH,
    FILTER_MISMATCH,
    ORDER_WITH_CURSOR,
    SECRET_REQUIRED,
    NULL_NOT_ALLOWED,
    PRECONDITION_REQUIRED,
    INVALID_IF_MATCH,
    IF_MATCH_REQUIRED,
    INVALID_EXPIRES_AT,
    INVALID_MERGE_PATCH_BODY,
    UNSUPPORTED_MEDIA_TYPE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_constant_round_trips_through_the_typed_reason() {
        for wire in ALL {
            let typed = Reason::from_wire(wire);
            assert!(
                !matches!(typed, Reason::Unknown(_)),
                "{wire} must be a modeled variant"
            );
            assert_eq!(typed.as_wire(), *wire);
            assert_eq!(typed.to_string(), *wire);
        }
    }

    #[test]
    fn constants_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for wire in ALL {
            assert!(seen.insert(*wire), "duplicate constant {wire}");
        }
    }

    #[test]
    fn unknown_wire_value_is_preserved() {
        let typed = Reason::from_wire("SOMETHING_NEW");
        assert_eq!(typed, Reason::Unknown("SOMETHING_NEW".to_owned()));
        assert_eq!(typed.as_wire(), "SOMETHING_NEW");
        assert_eq!(Reason::from_wire(""), Reason::Unknown(String::new()));
    }
}

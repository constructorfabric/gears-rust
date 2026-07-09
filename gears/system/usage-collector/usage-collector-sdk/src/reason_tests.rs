//! Wire round-trip unit tests for the typed reason enums.

use super::*;

#[test]
fn validation_reason_round_trips_each_constant() {
    for (wire, expected) in [
        (SEMANTICS_VIOLATION, ValidationReason::SemanticsViolation),
        (VALIDATION, ValidationReason::Validation),
        (METADATA_VALIDATION, ValidationReason::MetadataValidation),
        (UNKNOWN_METADATA_KEY, ValidationReason::UnknownMetadataKey),
        (
            GAUGE_COMPENSATION_REJECTED,
            ValidationReason::GaugeCompensationRejected,
        ),
        (MISSING_TIME_WINDOW, ValidationReason::MissingTimeWindow),
        (INVALID_BASE_GTS_ID, ValidationReason::InvalidBaseGtsId),
        (
            INVALID_METADATA_FIELDS_EMPTY_STRING,
            ValidationReason::MetadataFieldEmptyString,
        ),
        (
            INVALID_METADATA_FIELDS_INVALID_KEY,
            ValidationReason::MetadataFieldInvalidKey,
        ),
        (
            INVALID_METADATA_FIELDS_DUPLICATE,
            ValidationReason::MetadataFieldDuplicate,
        ),
    ] {
        assert_eq!(ValidationReason::from_wire(wire), expected);
        assert_eq!(expected.as_wire(), wire);
    }
}

#[test]
fn conflict_reason_round_trips_each_constant() {
    for (wire, expected) in [
        (USAGE_TYPE_REFERENCED, ConflictReason::UsageTypeReferenced),
        (ALREADY_INACTIVE, ConflictReason::AlreadyInactive),
        (IDEMPOTENCY_CONFLICT, ConflictReason::IdempotencyConflict),
        (
            CORRECTS_ID_TARGETS_COMPENSATION,
            ConflictReason::CorrectsIdTargetsCompensation,
        ),
        (
            CORRECTS_ID_WRONG_SCOPE,
            ConflictReason::CorrectsIdWrongScope,
        ),
        (CORRECTS_ID_INACTIVE, ConflictReason::CorrectsIdInactive),
    ] {
        assert_eq!(ConflictReason::from_wire(wire), expected);
        assert_eq!(expected.as_wire(), wire);
    }
}

#[test]
fn reasons_preserve_unknown_wire_string() {
    assert_eq!(
        ValidationReason::from_wire("FUTURE_CODE"),
        ValidationReason::Unknown("FUTURE_CODE".to_owned())
    );
    assert_eq!(
        ConflictReason::from_wire("FUTURE_CODE"),
        ConflictReason::Unknown("FUTURE_CODE".to_owned())
    );
}

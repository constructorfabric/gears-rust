//! Wire round-trip unit tests for the typed reason enums.

use super::*;

#[test]
fn validation_reason_round_trips_each_constant() {
    for (wire, expected) in [
        (SEMANTICS_VIOLATION, ValidationReason::SemanticsViolation),
        (VALIDATION, ValidationReason::Validation),
        (METADATA_VALIDATION, ValidationReason::MetadataValidation),
        (UNKNOWN_METADATA_KEY, ValidationReason::UnknownMetadataKey),
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
        (
            AGGREGATION_RESULT_TOO_LARGE,
            ValidationReason::AggregationResultTooLarge,
        ),
        (
            INVALIDATION_FIELD_MISMATCH,
            ValidationReason::InvalidationFieldMismatch,
        ),
        (FUTURE_WINDOW, ValidationReason::FutureWindow),
        (PAST_WINDOW, ValidationReason::PastWindow),
        (QUANTITY_OUT_OF_RANGE, ValidationReason::QuantityOutOfRange),
    ] {
        assert_eq!(ValidationReason::from_wire(wire), expected);
        assert_eq!(expected.as_wire(), wire);
    }
}

#[test]
fn conflict_reason_round_trips_each_constant() {
    for (wire, expected) in [
        (IDEMPOTENCY_CONFLICT, ConflictReason::IdempotencyConflict),
        (ALREADY_INVALIDATED, ConflictReason::AlreadyInvalidated),
        (TARGET_NOT_CONVERGED, ConflictReason::TargetNotConverged),
    ] {
        assert_eq!(ConflictReason::from_wire(wire), expected);
        assert_eq!(expected.as_wire(), wire);
    }
}

#[test]
fn the_retired_compensation_reasons_no_longer_model_themselves() {
    // The four compensation codes left the vocabulary with the
    // mutate-in-place correction model
    // (`cpt-cf-usage-collector-adr-append-only-invalidation`). Because
    // `ConflictReason` is `#[non_exhaustive]`, their removal is silent for
    // a downstream matcher: it falls through to `Unknown` rather than
    // failing to build. So pin that the fall-through happens *and* that it
    // preserves the raw string, so a consumer reading an envelope stored
    // under the old model still sees what it said.
    for wire in [
        "ALREADY_INACTIVE",
        "CORRECTS_ID_TARGETS_COMPENSATION",
        "CORRECTS_ID_WRONG_SCOPE",
        "CORRECTS_ID_INACTIVE",
    ] {
        assert_eq!(
            ConflictReason::from_wire(wire),
            ConflictReason::Unknown(wire.to_owned()),
        );
        assert_eq!(ConflictReason::from_wire(wire).as_wire(), wire);
    }
}

/// The two cursor codes now fall through to `Unknown`, preserving the wire
/// string.
///
/// Spec §3.13 gives them to `toolkit_odata`. Because `ValidationReason` is
/// `#[non_exhaustive]`, removing a variant is silent for a downstream
/// matcher: it falls through rather than failing to build. So pin that the
/// fall-through happens *and* that it preserves the raw string, so a
/// consumer holding a stored envelope that carries either code still reads
/// what it said.
///
/// `ORDER_WITH_CURSOR` is in the list although this gear never modeled it —
/// the rule §3.13 states covers all three, and a future variant added for it
/// would be the same mistake.
#[test]
fn the_upstream_cursor_reasons_no_longer_model_themselves() {
    for wire in ["INVALID_CURSOR", "FILTER_MISMATCH", "ORDER_WITH_CURSOR"] {
        assert_eq!(
            ValidationReason::from_wire(wire),
            ValidationReason::Unknown(wire.to_owned()),
        );
        assert_eq!(ValidationReason::from_wire(wire).as_wire(), wire);
    }
}

/// None of the four invalidation reasons the identity ADR emptied models
/// itself any more.
///
/// Two named key rules that only existed while an invalidation's key was
/// server-derived under a reserved prefix: `KEY_ON_INVALIDATION` refused a
/// caller key on an invalidation, and `RESERVED_KEY_PREFIX` refused a caller
/// key that began `inv:`.
/// `cpt-cf-usage-collector-adr-record-identity-derivation` makes an
/// invalidation repeat its target's caller-supplied key and reserves no
/// prefix, so both rules are gone.
///
/// The other two lost their raiser to the same ADR.
/// `INVALIDATION_REFERENCE_INCOMPLETE` refused a REST body carrying a target
/// reference without a reason code, or the reverse; the fold point that
/// raised it went with the DTO's flat pair, because the gateway now derives
/// the target from the withdrawal's own fields rather than reading a
/// caller-supplied one (`DESIGN.md` §3.1, Target resolution).
/// `INVALIDATION_TARGET_NOT_RECORD` refused a target that was itself an
/// invalidation; that property now "holds by construction and needs no check
/// of its own" (`DESIGN.md` §3.1), so the check that raised it is gone too.
///
/// **All four go because their cause is gone**, not because the vocabulary
/// was tidied: a reason no code path can produce is a reason a consumer
/// would be modelling for nothing. None of the four is published in
/// `usage-collector-v2.yaml`, so retiring them is not a wire-contract change.
///
/// Because `ValidationReason` is `#[non_exhaustive]`, removing a variant is
/// silent for a downstream matcher: it falls through rather than failing to
/// build. Pin that the fall-through happens *and* that it preserves the raw
/// string.
#[test]
fn the_retired_invalidation_reasons_no_longer_model_themselves() {
    for wire in [
        "KEY_ON_INVALIDATION",
        "RESERVED_KEY_PREFIX",
        "INVALIDATION_REFERENCE_INCOMPLETE",
        "INVALIDATION_TARGET_NOT_RECORD",
    ] {
        assert_eq!(
            ValidationReason::from_wire(wire),
            ValidationReason::Unknown(wire.to_owned()),
        );
        assert_eq!(ValidationReason::from_wire(wire).as_wire(), wire);
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

/// Pins the *value* of every wire constant against its own identifier.
///
/// The round-trip tables above prove `from_wire` and
/// `as_wire` are inverses and nothing else, because both sides of every
/// assertion read the same constant. Changing a constant's value leaves
/// them green while every client matching the published spelling breaks.
///
/// Every code in this module is `SCREAMING_SNAKE` and identical to its
/// identifier, which is what makes a single `stringify!` table able to pin
/// all of them. Coverage is checked against `reason.rs`'s own text at
/// compile time rather than against a hand-maintained count, so a constant
/// added to `reason.rs` without a matching row here fails the count instead
/// of going unpinned and invisible.
#[test]
fn every_wire_constant_spells_its_own_identifier() {
    macro_rules! pin {
        ($($name:ident),+ $(,)?) => {
            [$((stringify!($name), $name)),+]
        };
    }

    let pinned = pin![
        SEMANTICS_VIOLATION,
        VALIDATION,
        METADATA_VALIDATION,
        UNKNOWN_METADATA_KEY,
        INVALID_BASE_GTS_ID,
        INVALID_METADATA_FIELDS_EMPTY_STRING,
        INVALID_METADATA_FIELDS_INVALID_KEY,
        INVALID_METADATA_FIELDS_DUPLICATE,
        AGGREGATION_RESULT_TOO_LARGE,
        INVALIDATION_FIELD_MISMATCH,
        FUTURE_WINDOW,
        PAST_WINDOW,
        QUANTITY_OUT_OF_RANGE,
        CURSOR_BEYOND_RETENTION,
        IDEMPOTENCY_CONFLICT,
        ALREADY_INVALIDATED,
        TARGET_NOT_CONVERGED,
    ];

    for (identifier, value) in pinned {
        assert_eq!(
            value, identifier,
            "the wire code `{identifier}` must spell its own identifier: it is \
             published in usage-collector-v2.yaml and matched by clients, so a \
             changed value is a silent wire break",
        );
    }

    // `include_str!` reads reason.rs at compile time, so this counts the
    // constants the module really declares rather than the rows this table
    // happens to list. Comparing `pinned.len()` against a hardcoded number
    // instead would be a tautology: `pinned` is a fixed-size array, so its
    // length always equals the row count written above it, and a constant
    // added to reason.rs without a row here would stay unpinned and
    // invisible — the one case this assertion exists for.
    //
    // This is a textual scan, not parsing, and it is line-anchored because
    // every declaration in reason.rs sits at column zero — an indented
    // `pub const` inside a function or impl block is not a wire constant.
    // It therefore counts lines that *look* like declarations: a
    // `pub const`-shaped line inside a block comment or a multi-line string
    // literal, or one gated behind a `#[cfg]` that is off in this build,
    // would be counted too. None exists in this file today.
    //
    // The trade is deliberate. Every one of those miscounts is an
    // over-count, so it fails loudly and points at reason.rs, where the
    // hardcoded length it replaced could only ever be wrong silently. If a
    // comment here ever needs to show a `pub const` line, indent it rather
    // than trusting this scan to skip it.
    let declared = include_str!("reason.rs")
        .lines()
        .filter(|line| line.starts_with("pub const "))
        .count();

    assert_eq!(
        pinned.len(),
        declared,
        "every `pub const` in reason.rs needs a row in this table: reason.rs \
         declares {declared} and this table pins {}",
        pinned.len(),
    );
}

#[test]
fn cursor_beyond_retention_round_trips_through_its_wire_spelling() {
    use crate::v2::reason::{CURSOR_BEYOND_RETENTION, ValidationReason};

    assert_eq!(CURSOR_BEYOND_RETENTION, "CURSOR_BEYOND_RETENTION");
    assert_eq!(
        ValidationReason::from_wire(CURSOR_BEYOND_RETENTION),
        ValidationReason::CursorBeyondRetention,
        "usage-collector-v2.yaml publishes CURSOR_BEYOND_RETENTION as a cursor field violation, \
         so the typed reason must decode from exactly that spelling"
    );
    assert_eq!(
        ValidationReason::CursorBeyondRetention.as_wire(),
        CURSOR_BEYOND_RETENTION
    );
}

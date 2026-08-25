//! Tests for the optimistic-concurrency vocabulary.

use super::{PolicyTag, RowVersion, require_match, require_policy_match};
use crate::domain::error::DomainError;

fn refused(raw: &str) -> DomainError {
    RowVersion::from_etag(raw).expect_err("expected the value to be refused")
}

#[test]
fn a_version_survives_the_wire_round_trip() {
    // The rendering and the parser are one pair: whatever the surface hands out
    // as an `ETag` has to come back as the same version, or the guard compares
    // a version nobody ever held.
    let version = RowVersion::new(12);
    assert_eq!(version.to_etag(), "\"12\"");
    assert_eq!(
        RowVersion::from_etag(&version.to_etag()).expect("the rendered tag parses"),
        version
    );
}

#[test]
fn surrounding_whitespace_on_the_header_value_is_tolerated() {
    // A header value arrives with whatever padding the client's writer left; the
    // padding says nothing about which version was read.
    assert_eq!(
        RowVersion::from_etag("  \"7\" ").expect("a padded tag parses"),
        RowVersion::new(7)
    );
}

#[test]
fn a_weak_validator_is_refused() {
    // RFC 9110 forbids one in `If-Match`: a weak comparison asserts that two
    // representations are equivalent, which cannot decide whether a write is
    // safe.
    //
    // The message is asserted, not only the variant. `W/"12"` fails the generic
    // quoting check too, so deleting the dedicated branch would still refuse it
    // — and the refusal would stop saying why. The variant alone cannot tell
    // those two worlds apart; the substring can.
    let err = refused("W/\"12\"");
    assert!(matches!(err, DomainError::InvalidRequest(_)));
    assert!(err.to_string().contains("weak validator"));
}

#[test]
fn the_wildcard_is_refused() {
    // `*` means "if the resource exists at all" — overwrite whatever is there,
    // which is exactly the silent overwrite `fr-concurrent-edit` forbids. The
    // substring pins that reasoning: without the dedicated branch `*` is merely
    // an unquoted string, refused for the wrong reason.
    let err = refused("*");
    assert!(matches!(err, DomainError::InvalidRequest(_)));
    assert!(err.to_string().contains("wildcard"));
}

#[test]
fn a_list_of_tags_is_refused() {
    // An authoring mutation targets one known version; picking a member would be
    // guessing which one the caller actually read. Same trap as the two above —
    // a list also fails the quoting check, so only the message distinguishes a
    // deliberate refusal from an accidental one.
    let err = refused("\"12\", \"13\"");
    assert!(matches!(err, DomainError::InvalidRequest(_)));
    assert!(
        err.to_string()
            .contains("a list does not say which version was read")
    );
}

#[test]
fn a_bare_integer_is_refused() {
    // An unquoted number is not an entity tag. Accepting it would make the gear
    // laxer than the header it claims to implement, and the leniency would have
    // to be mirrored by every other reader of the same value.
    assert!(matches!(refused("12"), DomainError::InvalidRequest(_)));
}

#[test]
fn an_empty_tag_is_refused() {
    assert!(matches!(refused("\"\""), DomainError::InvalidRequest(_)));
}

#[test]
fn a_signed_version_is_refused() {
    // A version is a count, never a delta, so neither sign is a version.
    assert!(matches!(refused("\"+12\""), DomainError::InvalidRequest(_)));
    assert!(matches!(refused("\"-12\""), DomainError::InvalidRequest(_)));
}

#[test]
fn a_non_digit_tag_is_refused() {
    assert!(matches!(refused("\"v12\""), DomainError::InvalidRequest(_)));
}

#[test]
fn a_version_past_u64_is_refused() {
    // All digits and still not a version. Refusing beats saturating: a saturated
    // parse would compare equal to a real row that happens to sit at the top.
    assert!(matches!(
        refused("\"18446744073709551616\""),
        DomainError::InvalidRequest(_)
    ));
}

#[test]
fn a_stored_version_survives_the_storage_round_trip() {
    let stored = RowVersion::new(9).to_stored().expect("9 fits in a bigint");
    assert_eq!(stored, 9);
    assert_eq!(
        RowVersion::from_stored(stored).expect("a stored version rehydrates"),
        RowVersion::new(9)
    );
}

#[test]
fn a_negative_stored_version_is_an_internal_fault() {
    // The column is `NOT NULL DEFAULT 0` and only ever incremented, so nothing a
    // caller submitted could produce this; reporting it as a bad request would
    // send an operator looking at the request instead of at the row.
    let err = RowVersion::from_stored(-1).expect_err("a negative column value is a breach");
    assert!(matches!(err, DomainError::Internal(_)));
    assert!(err.to_string().contains("row_version"));
    assert!(err.to_string().contains("-1"));
}

#[test]
fn a_version_past_the_bigint_range_is_an_internal_fault() {
    // Same side of the line, the other direction: the column cannot hold it, and
    // no reshaped request would make it fit.
    let err = RowVersion::new(u64::MAX)
        .to_stored()
        .expect_err("u64::MAX does not fit a bigint");
    assert!(matches!(err, DomainError::Internal(_)));
    assert!(err.to_string().contains("row_version"));
}

#[test]
fn a_submit_on_the_version_that_was_read_is_admitted() {
    assert!(require_match(RowVersion::new(4), RowVersion::new(4)).is_ok());
}

#[test]
fn a_stale_submit_is_refused_and_names_both_versions() {
    // The pair is the diagnosis: an operator has to be able to tell a caller
    // that never refreshed from a genuine bulk-vs-interactive collision.
    let err = require_match(RowVersion::new(5), RowVersion::new(4))
        .expect_err("version 4 is stale against 5");
    assert!(matches!(err, DomainError::StaleVersion(_)));
    assert!(err.to_string().contains('5'));
    assert!(err.to_string().contains('4'));
}

#[test]
fn display_renders_the_bare_integer() {
    // The storage spelling and the body of the entity tag, so a log line and a
    // wire value read the same.
    assert_eq!(RowVersion::new(0).to_string(), "0");
    assert_eq!(RowVersion::new(12).to_string(), "12");
}

// ---------------------------------------------------------------------------
// D-186: the policy tag.
// ---------------------------------------------------------------------------

/// **Absence is not version zero**, which is the one confusion this rendering must
/// not make.
///
/// `ThresholdService::propose` mints a tenant's first version as `0`, and
/// `threshold_repo::latest_version`'s own doc says the absence and version zero are
/// different states. A tag that collapsed them would equal the bootstrap's tag and
/// the first-approved-version's tag — so a `PUT` authored *before* that approval
/// would be accepted after it, which is exactly the sequential lost update the
/// precondition exists to refuse.
///
/// Both axes, because the framing has to be null-safe on each: an absent effective
/// version beside a present unit must not collide with a present version beside an
/// absent unit either.
#[test]
fn the_absence_of_a_version_is_not_version_zero() {
    let unit = uuid::Uuid::from_u128(0x1_d6);

    assert_ne!(PolicyTag::of(None, None), PolicyTag::of(Some(0), None));
    assert_ne!(PolicyTag::of(None, None), PolicyTag::of(None, Some(unit)));
    assert_ne!(
        PolicyTag::of(Some(0), None),
        PolicyTag::of(None, Some(unit)),
        "the two fields are framed distinguishably, not concatenated"
    );
    assert_ne!(PolicyTag::of(Some(0), None), PolicyTag::of(Some(1), None));
}

/// The tag moves with **either** field, which is what makes it a tag over the whole
/// representation rather than over half of it.
///
/// `ThresholdPolicyView` serves `effective` and `pendingApproval`. A validator that
/// did not change when a proposal opened would tell a caller their premise held
/// while the thing they read had changed underneath them.
#[test]
fn the_tag_covers_both_served_facts() {
    let one = uuid::Uuid::from_u128(0xa);
    let two = uuid::Uuid::from_u128(0xb);

    assert_ne!(
        PolicyTag::of(Some(3), None),
        PolicyTag::of(Some(3), Some(one)),
        "a proposal opening changes the representation"
    );
    assert_ne!(
        PolicyTag::of(Some(3), Some(one)),
        PolicyTag::of(Some(3), Some(two)),
        "and so does a different proposal being the open one"
    );
    // Determinism, which every comparison above silently assumes: the same pair
    // renders the same tag, or a caller's own round trip would refuse itself.
    assert_eq!(
        PolicyTag::of(Some(3), Some(one)),
        PolicyTag::of(Some(3), Some(one))
    );
}

/// The tag survives the wire round trip, and the envelope it is read back through is
/// the shared one.
///
/// The three refusals of meaning are `strong_tag_body`'s and are asserted for
/// [`RowVersion`] above; what is asserted here is that this reader **gets** them
/// rather than re-implementing the envelope laxly — the wildcard on this verb would
/// author a governance policy over whatever happens to be current.
#[test]
fn a_policy_tag_survives_the_wire_round_trip_and_refuses_the_wildcard() {
    let tag = PolicyTag::of(Some(7), None);
    let rendered = tag.to_etag();
    assert!(
        rendered.starts_with('"') && rendered.ends_with('"'),
        "a strong entity tag is quoted: {rendered}"
    );
    assert_eq!(
        PolicyTag::from_etag(&rendered).expect("the rendered tag parses"),
        tag
    );
    assert_eq!(
        PolicyTag::from_etag(&format!("  {rendered} ")).expect("a padded tag parses"),
        tag,
        "padding says nothing about which policy was read"
    );

    for raw in ["*", "W/\"aa\"", "\"aa\", \"bb\""] {
        assert!(
            matches!(
                PolicyTag::from_etag(raw),
                Err(DomainError::InvalidRequest(_))
            ),
            "{raw} is refused as a request that cannot be interpreted"
        );
    }
}

/// A body this surface could never have issued is a **malformed request**, not a
/// stale one.
///
/// The distinction is the caller's remedy. `STALE_VERSION` says *re-read and
/// resubmit*; for a tag nobody ever handed out, re-reading changes nothing and the
/// honest answer is that the request cannot be interpreted. So the shape check lives
/// in the reader, and the 409 is reserved for a well-formed tag whose premise moved.
#[test]
fn a_body_this_surface_never_issued_is_not_a_stale_tag() {
    for raw in [
        "\"\"",
        "\"7\"",
        "\"not-a-digest\"",
        // 64 characters, but uppercase — the rendering is lowercase hex, so this is
        // not a tag this surface issued either. Refused rather than folded, because
        // folding would make two spellings of one tag compare unequal elsewhere.
        "\"0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF\"",
        // 63 characters: one short of a digest.
        "\"012345678901234567890123456789012345678901234567890123456789012\"",
    ] {
        assert!(
            matches!(
                PolicyTag::from_etag(raw),
                Err(DomainError::InvalidRequest(_))
            ),
            "{raw} was never issued by this surface"
        );
    }
}

/// The comparison refuses a moved premise as `STALE_VERSION`, naming both tags.
///
/// 409 and deliberately not 412: `01-foundation.md` §3.3 names `STALE_VERSION` for
/// an `ETag` conflict and forbids minting a status the canonical family does not
/// carry. And both tags are in the detail for `require_match`'s reason — an operator
/// needs to see which read they were working from, not only that it was stale.
#[test]
fn a_moved_policy_premise_is_a_stale_version_naming_both_tags() {
    let read = PolicyTag::of(None, None);
    let current = PolicyTag::of(Some(0), None);

    require_policy_match(&read, &read).expect("a premise that has not moved is accepted");

    let err = require_policy_match(&current, &read).expect_err("a moved premise is refused");
    assert!(matches!(err, DomainError::StaleVersion(_)));
    let detail = err.to_string();
    assert!(detail.contains(current.as_str()), "{detail}");
    assert!(detail.contains(read.as_str()), "{detail}");
}

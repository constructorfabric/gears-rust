//! Unit tests for the feed types.
//!
//! Every assertion here is about a constructor's own decision — what
//! [`super::FeedPosition::new`] and [`super::FeedSubscription::new`] admit,
//! refuse and normalise. Nothing about [`super::FeedPage`] is asserted,
//! deliberately: it is a plain struct with two public fields, so a test
//! building one and reading its own arguments back would pin the fields'
//! types and visibility at compile time and nothing at all at run time. What
//! discriminates a live page's cursor from a finished replay's is a backend's
//! answer, and that is asserted where a backend answers — a plugin's own
//! tests, such as the noop plugin's `plugin_tests`.

use std::collections::HashSet;

use super::{
    FeedPosition, FeedPositionInvalid, FeedStart, FeedSubscription, FeedSubscriptionInvalid,
    MAX_FEED_POSITION_BYTES, SubscribedMeters, SubscribedMetersInvalid,
};
use crate::v2::models::MeterTypeId;
use crate::v2::stored::MeterRef;

#[test]
fn a_position_at_the_byte_bound_is_accepted() {
    let bytes = vec![7_u8; MAX_FEED_POSITION_BYTES];

    let position = FeedPosition::new(bytes.clone()).expect("the bound itself is admissible");

    assert_eq!(position.as_bytes(), &bytes[..]);
    assert_eq!(position.len(), MAX_FEED_POSITION_BYTES);
    assert!(
        !position.is_empty(),
        "new() never admits an empty sequence, so a built position is never empty"
    );
}

#[test]
fn a_position_one_byte_over_the_bound_is_refused() {
    let bytes = vec![7_u8; MAX_FEED_POSITION_BYTES + 1];

    let refused = FeedPosition::new(bytes).expect_err(
        "a position past the bound cannot fit the wire cursor, so it must be refused where it \
         is built rather than where it is encoded",
    );

    assert!(matches!(
        refused,
        FeedPositionInvalid::TooLarge { actual } if actual == MAX_FEED_POSITION_BYTES + 1
    ));
}

#[test]
fn an_empty_position_is_refused() {
    let refused = FeedPosition::new(Vec::new()).expect_err(
        "a position is a point a plugin issued; zero bytes is a plugin that forgot to fill it, \
         not an encoding",
    );

    assert!(matches!(refused, FeedPositionInvalid::Empty));
}

#[test]
fn positions_compare_and_hash_equal_exactly_when_their_bytes_are_equal() {
    let one = FeedPosition::new(vec![0, 1]).expect("two bytes are admissible");
    let same_bytes = FeedPosition::new(vec![0, 1]).expect("same bytes");
    let different_bytes = FeedPosition::new(vec![0, 2]).expect("two bytes are admissible");

    assert_eq!(one, same_bytes);
    assert_ne!(one, different_bytes);

    let mut positions = HashSet::new();
    positions.insert(one);
    assert!(
        positions.contains(&same_bytes),
        "equal positions must hash equally to work as a map key"
    );
    assert!(!positions.contains(&different_bytes));
}

fn meter(suffix: &str) -> MeterTypeId {
    MeterTypeId::new(format!(
        "gts.cf.core.uc.usage_record.v2~test.{suffix}._.meter.v1~"
    ))
    .expect("the fixture meter id is well formed")
}

#[test]
fn a_start_is_named_rather_than_inferred_from_an_absent_position() {
    let oldest: FeedStart<FeedPosition> = FeedStart::Oldest;
    let resumed = FeedStart::After(FeedPosition::new(vec![1]).expect("one byte is admissible"));
    let same_resumed =
        FeedStart::After(FeedPosition::new(vec![1]).expect("one byte is admissible"));

    assert_eq!(oldest, FeedStart::Oldest);
    assert_eq!(resumed, same_resumed);
    assert_ne!(oldest, resumed);
    assert!(matches!(oldest, FeedStart::Oldest));
    assert!(matches!(resumed, FeedStart::After(ref p) if p.as_bytes() == [1]));
}

#[test]
fn a_subscription_is_a_set_so_it_dedupes_and_orders() {
    let subscription = FeedSubscription::new([meter("b"), meter("a"), meter("b")])
        .expect("two distinct types are a valid subscription");
    let already_sorted_and_deduped = FeedSubscription::new([meter("a"), meter("b")])
        .expect("two distinct types are a valid subscription");

    assert_eq!(
        subscription, already_sorted_and_deduped,
        "DESIGN section 3.1 calls a subscription a set of GTS types, so a repeat is the same \
         subscription whatever order or multiplicity the caller listed it in"
    );
    assert_eq!(
        subscription.types(),
        &[meter("a"), meter("b")],
        "the order is the SDK's to fix rather than the caller's to choose"
    );
}

#[test]
fn an_empty_subscription_is_refused() {
    let refused = FeedSubscription::new(Vec::new()).expect_err(
        "a subscription reading no type has no page to serve, so it is a caller defect rather \
         than an empty feed",
    );

    assert_eq!(refused, FeedSubscriptionInvalid::Empty);
}

// ── SubscribedMeters: the shape read_feed_page takes ───────────────────────

fn meter_ref(raw: &str, uuid: u128) -> MeterRef {
    MeterRef::new(
        uuid::Uuid::from_u128(uuid),
        MeterTypeId::new(raw).expect("valid meter id"),
    )
}

const METER_A: &str = "gts.cf.core.uc.usage_record.v2~example.metering._.a.v1~";
const METER_B: &str = "gts.cf.core.uc.usage_record.v2~example.metering._.b.v1~";

/// A page is bounded by its subscription, so a subscription naming nothing has
/// no page to serve. `&[MeterRef]` could express that call; this type cannot.
#[test]
fn an_empty_meter_subscription_is_refused() {
    assert_eq!(
        SubscribedMeters::new([]),
        Err(SubscribedMetersInvalid::Empty),
    );
}

/// `PgRecordStore::feed_page` deduplicated defensively because the slice shape
/// promised nothing. The constructor is where that belongs.
#[test]
fn a_repeated_reference_is_deduplicated() {
    let meters = SubscribedMeters::new([
        meter_ref(METER_A, 0xA1),
        meter_ref(METER_B, 0xB1),
        meter_ref(METER_A, 0xA1),
    ])
    .expect("three entries naming two meters is a two-meter subscription");

    assert_eq!(meters.refs().len(), 2);
}

/// Deduplication is on the reference, which is the identity a plugin keys on.
/// Two entries that agree on *both* halves are one meter named twice.
#[test]
fn deduplication_reads_the_reference_not_the_identifier() {
    let meters = SubscribedMeters::new([meter_ref(METER_A, 0xA1), meter_ref(METER_A, 0xA1)])
        .expect("the same meter listed twice is one meter");

    assert_eq!(meters.refs().len(), 1);
}

/// The same set of meters is the same subscription whatever order the caller
/// listed it in, which is what lets a plugin treat the sequence as canonical.
#[test]
fn the_order_a_caller_lists_meters_in_does_not_matter() {
    let one = SubscribedMeters::new([meter_ref(METER_A, 0xA1), meter_ref(METER_B, 0xB1)])
        .expect("non-empty");
    let other = SubscribedMeters::new([meter_ref(METER_B, 0xB1), meter_ref(METER_A, 0xA1)])
        .expect("non-empty");

    assert_eq!(one.refs(), other.refs());
}

/// One registry reference maps to one identifier for the life of an
/// installation. Two entries sharing a reference but naming different
/// identifiers is that invariant broken upstream, and it is refused here
/// rather than quietly resolved.
///
/// Deduplicating such a pair instead would keep one label and drop the other,
/// and every entry of *both* meters would then be served relabelled under the
/// surviving one — the one relabelling route the re-attachment guards
/// elsewhere do not close.
#[test]
fn one_reference_under_two_identifiers_is_refused_not_collapsed() {
    assert_eq!(
        SubscribedMeters::new([meter_ref(METER_A, 0xA1), meter_ref(METER_B, 0xA1)]),
        Err(SubscribedMetersInvalid::ConflictingIdentifiers {
            reference: uuid::Uuid::from_u128(0xA1),
        }),
    );
}

/// Pins the number, which every other test here reads symbolically.
///
/// It is derived, not chosen: a position is carried inside a
/// `toolkit_odata::CursorV1` whose wire token the contract caps at
/// `maxLength: 4096`, and base64url of 512 bytes is 684 characters, leaving
/// room for the cursor's own fields. Raising it past what that cap holds
/// would hand a consumer a continuation its next request refuses.
#[test]
fn the_published_feed_position_bound_is_five_hundred_and_twelve_bytes() {
    assert_eq!(MAX_FEED_POSITION_BYTES, 512);
    assert!(MAX_FEED_POSITION_BYTES.div_ceil(3) * 4 < 4096);
}

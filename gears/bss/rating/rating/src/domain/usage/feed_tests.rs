#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{
    FeedCursor, FeedStart, FeedSubscription, MAX_CURSOR_LEN, MAX_METER_ID_LEN, METER_BASE_TYPE,
    MeterId,
};

fn meter(name: &str) -> MeterId {
    MeterId::parse(format!("{METER_BASE_TYPE}cf.test.{name}.v1~")).unwrap()
}

#[test]
fn a_meter_adds_exactly_one_segment_to_the_v2_base_type() {
    assert!(MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1~").is_ok());
    for refused in [
        "gts.cf.core.uc.usage_record.v1~cf.oss.cloudlet_hours.v1~",
        "gts.cf.core.uc.usage_record.v2~",
        "gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1",
        "gts.cf.core.uc.usage_record.v2~a~b~",
        "gts.cf.core.uc.usage_record.v2~cf.oss\u{7}.v1~",
    ] {
        assert!(
            MeterId::parse(refused).is_err(),
            "{refused} must be refused"
        );
    }
    let long = format!("{METER_BASE_TYPE}{}~", "a".repeat(MAX_METER_ID_LEN));
    assert!(MeterId::parse(long).is_err());
}

#[test]
fn a_subscription_is_the_same_whatever_order_its_meters_are_listed_in() {
    let one = FeedSubscription::new([meter("b"), meter("a"), meter("b")]).unwrap();
    let two = FeedSubscription::new([meter("a"), meter("b")]).unwrap();
    assert_eq!(one, two);
    assert_eq!(one.meters(), &[meter("a"), meter("b")]);
}

#[test]
fn a_subscription_names_one_to_a_hundred_meters() {
    assert!(FeedSubscription::new([]).is_err());
    let hundred = (0..100).map(|i| meter(&format!("m{i}")));
    assert!(FeedSubscription::new(hundred).is_ok());
    let more = (0..101).map(|i| meter(&format!("m{i}")));
    assert!(FeedSubscription::new(more).is_err());
}

#[test]
fn a_cursor_is_one_to_four_kibibytes() {
    assert!(FeedCursor::parse("").is_err());
    assert!(FeedCursor::parse("c".repeat(MAX_CURSOR_LEN)).is_ok());
    assert!(FeedCursor::parse("c".repeat(MAX_CURSOR_LEN + 1)).is_err());
}

#[test]
fn a_source_without_a_cursor_reads_from_oldest() {
    assert_eq!(FeedStart::from_checkpoint(None), FeedStart::Oldest);
    let cursor = FeedCursor::parse("c1").unwrap();
    assert_eq!(
        FeedStart::from_checkpoint(Some(&cursor)),
        FeedStart::After(&cursor)
    );
}

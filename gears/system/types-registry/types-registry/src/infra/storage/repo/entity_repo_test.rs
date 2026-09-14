//! Pure unit tests for the SQL prefilter, which is the one piece of
//! [`super`] that is a hand-written approximation and therefore the one piece a
//! DB-backed test cannot fully pin.
//!
//! The prefilter must never be *too tight*: a range that excludes a real match
//! loses rows silently, with no error and no failing query. Being too wide only
//! costs a comparison in Rust. Every case below is written from that asymmetry.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use gts::GtsIdPattern;
use toolkit_gts::gts_id;

use super::{prefilter_prefix, range_upper_bound};

fn prefix_of(pattern: &str) -> Option<String> {
    let pattern = GtsIdPattern::try_new(pattern).expect("valid pattern");
    prefilter_prefix(&pattern)
}

#[test]
fn the_prefix_stops_before_the_final_segment() {
    assert_eq!(
        prefix_of(gts_id!("acme.crm.customer.type.v1~")).as_deref(),
        Some("gts.acme.crm.customer.type.")
    );
}

/// The reason the final segment is dropped. A pattern segment matches with
/// minor-version flexibility, so `…type.v1~` also matches `…type.v1.0~` — whose
/// bytes do **not** start with `…type.v1~`. A range built from the full literal
/// string would exclude a real match.
#[test]
fn a_minor_bearing_identifier_still_falls_inside_the_range() {
    let prefix = prefix_of(gts_id!("acme.crm.customer.type.v1~")).expect("prefix");
    for candidate in [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.customer.type.v1.0~"),
        gts_id!("acme.crm.customer.type.v1.7~"),
    ] {
        assert!(
            candidate.starts_with(&prefix),
            "{candidate} must be inside the prefilter range"
        );
    }
}

#[test]
fn a_trailing_wildcard_is_cut_before_the_range_is_built() {
    assert_eq!(
        prefix_of(&format!("{}*", gts_id!("acme.crm.customer.type.v1~"))).as_deref(),
        Some("gts.acme.crm.customer.type.")
    );
    assert_eq!(prefix_of(gts_id!("acme.*")).as_deref(), Some("gts."));
}

/// A chained identifier's boundary is `~`, not `.`, and the rule must cut at
/// whichever comes last.
#[test]
fn a_chained_pattern_keeps_everything_up_to_its_last_segment() {
    assert_eq!(
        prefix_of(gts_id!("cf.core.events.type.v1~acme.crm.order.type.v1~")).as_deref(),
        Some("gts.cf.core.events.type.v1~acme.crm.order.type.")
    );
}

/// A pattern with nothing before its first boundary constrains nothing usable, so
/// the read runs without a range rather than with a wrong one.
#[test]
fn a_pattern_with_no_usable_prefix_yields_none() {
    assert_eq!(prefix_of(gts_id!("*")), None);
}

#[test]
fn the_upper_bound_increments_the_last_byte() {
    // '.' is 0x2E, so the bound is '/' (0x2F) — every string starting with the
    // prefix sorts below it in byte order.
    assert_eq!(range_upper_bound("gts.acme.").as_deref(), Some("gts.acme/"));
    assert_eq!(range_upper_bound("").as_deref(), None);
}

#[test]
fn the_range_brackets_exactly_the_strings_with_that_prefix() {
    let prefix = "gts.acme.crm.customer.type.";
    let upper = range_upper_bound(prefix).expect("bound");
    for inside in [
        gts_id!("acme.crm.customer.type.v1~"),
        gts_id!("acme.crm.customer.type.v1.0~"),
        "gts.acme.crm.customer.type.zzz",
    ] {
        assert!(
            inside >= prefix && inside < upper.as_str(),
            "{inside} inside"
        );
    }
    for outside in [
        gts_id!("acme.crm.customer.other.v1~"),
        gts_id!("acme.crm.customer.types.v1~"),
        gts_id!("acme.crm.invoice.type.v1~"),
    ] {
        assert!(
            !(outside >= prefix && outside < upper.as_str()),
            "{outside} outside"
        );
    }
}

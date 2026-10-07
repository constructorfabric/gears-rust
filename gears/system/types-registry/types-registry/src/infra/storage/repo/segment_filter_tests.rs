//! Shape tests for the segment compiler. The differential check against
//! `GtsId::matches_pattern` runs through SQL in `discovery_pattern_backends_test`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use gts::{GTS_ID_PREFIX, GtsId, GtsIdPattern};
use toolkit_gts::gts_id;

use super::{
    NameFilter, PatternPlan, SegmentFilter, SegmentRow, SegmentRowError, compile, id_range,
    segment_rows, upper_bound,
};

fn plan(pattern: &str) -> PatternPlan {
    compile(&GtsIdPattern::try_new(pattern).expect("valid pattern")).expect("compiles")
}

fn filters(pattern: &str) -> Vec<SegmentFilter> {
    match plan(pattern) {
        PatternPlan::Segments(filters) => filters,
        PatternPlan::Never => panic!("{pattern} compiled to Never"),
    }
}

fn exact(name: &str) -> NameFilter {
    NameFilter::Exact(name.to_owned())
}

fn prefix(name: &str) -> NameFilter {
    NameFilter::Prefix(name.to_owned())
}

fn concrete(no: i16, name: &str, major: i64, minor: Option<i64>, is_type: bool) -> SegmentFilter {
    SegmentFilter {
        segment_no: no,
        name: Some(exact(name)),
        major: Some(major),
        minor,
        is_type: Some(is_type),
    }
}

fn wild(no: i16, name: NameFilter, major: Option<i64>) -> SegmentFilter {
    SegmentFilter {
        segment_no: no,
        name: Some(name),
        major,
        minor: None,
        is_type: None,
    }
}

#[test]
fn stored_rows_carry_every_parsed_field() {
    let id = GtsId::try_new(gts_id!("x.p.n.t.v1.2~a.b.c.d.v0~e.f.g.h.v3")).expect("id");
    let row = |no, name: &str, major, minor, is_type| SegmentRow {
        segment_no: no,
        segment_name: name.to_owned(),
        major,
        minor,
        is_type,
    };
    assert_eq!(
        segment_rows(&id).expect("rows"),
        [
            row(0, "x.p.n.t", 1, Some(2), true),
            row(1, "a.b.c.d", 0, None, true),
            row(2, "e.f.g.h", 3, None, false),
        ]
    );
}

#[test]
fn a_uuid_tail_has_no_stored_shape() {
    let id =
        GtsId::try_new(gts_id!("x.p.n.t.v1~7a1d2f34-5678-49ab-9012-abcdef123456")).expect("id");
    assert_eq!(segment_rows(&id), Err(SegmentRowError::UuidTail));
}

#[test]
fn concrete_segments_pin_every_field_and_minor_only_when_given() {
    assert_eq!(
        filters(gts_id!("x.p.n.t.v1.2~a.b.c.d.v3~e.f.g.h.v0")),
        [
            concrete(0, "x.p.n.t", 1, Some(2), true),
            concrete(1, "a.b.c.d", 3, None, true),
            concrete(2, "e.f.g.h", 0, None, false),
        ]
    );
}

#[test]
fn a_wildcard_pins_its_given_fields_and_never_the_type_marker() {
    for (pattern, name, major) in [
        (gts_id!("x.*"), prefix("x."), None),
        (gts_id!("x.p.*"), prefix("x.p."), None),
        (gts_id!("x.p.n.*"), prefix("x.p.n."), None),
        (gts_id!("x.p.n.t.*"), exact("x.p.n.t"), None),
        (gts_id!("x.p.n.t.v*"), exact("x.p.n.t"), None),
        (gts_id!("x.p.n.t.v0.*"), exact("x.p.n.t"), Some(0)),
        (gts_id!("x.p.n.t.v1.*"), exact("x.p.n.t"), Some(1)),
    ] {
        assert_eq!(filters(pattern), [wild(0, name, major)], "{pattern}");
    }
}

#[test]
fn a_bare_star_leaves_its_position_free() {
    assert_eq!(filters(gts_id!("*")), []);
    assert_eq!(
        filters(gts_id!("x.p.n.t.v1~*")),
        [concrete(0, "x.p.n.t", 1, None, true)]
    );
    assert_eq!(
        filters(gts_id!("x.p.n.t.v1~a.*")),
        [
            concrete(0, "x.p.n.t", 1, None, true),
            wild(1, prefix("a."), None)
        ]
    );
}

#[test]
fn a_uuid_tail_pattern_matches_no_stored_identifier() {
    assert_eq!(
        plan(gts_id!("x.p.n.t.v1~7a1d2f34-5678-49ab-9012-abcdef123456")),
        PatternPlan::Never
    );
}

#[test]
fn the_id_range_comes_from_the_first_segment_only() {
    let range = |pattern: &str| id_range(&filters(pattern)).map(|(lower, _)| lower);
    let p = GTS_ID_PREFIX;
    assert_eq!(range(gts_id!("*")), None);
    assert_eq!(range(gts_id!("x.*")), Some(format!("{p}x.")));
    assert_eq!(range(gts_id!("x.p.n.t.*")), Some(format!("{p}x.p.n.t.")));
    assert_eq!(
        range(gts_id!("x.p.n.t.v1.*")),
        Some(format!("{p}x.p.n.t.v1"))
    );
    assert_eq!(
        range(gts_id!("x.p.n.t.v1.2~a.*")),
        Some(format!("{p}x.p.n.t.v1"))
    );
}

/// The range may be wider than the pattern, never narrower.
#[test]
fn every_match_falls_inside_the_id_range() {
    let ids = [
        gts_id!("x.p.n.t.v1~"),
        gts_id!("x.p.n.t.v1.0~"),
        gts_id!("x.p.n.t.v1.7~a.b.c.d.v1"),
        gts_id!("x.p.n.t.v10~"),
        gts_id!("x.p.n.t2.v1~"),
        gts_id!("x.p.n.t_x.v1~"),
        gts_id!("x0.p.n.t.v1~"),
        gts_id!("x_y.p.n.t.v1~"),
    ];
    for pattern in [
        gts_id!("x.*"),
        gts_id!("x.p.n.t.*"),
        gts_id!("x.p.n.t.v1~"),
        gts_id!("x.p.n.t.v1.*"),
        gts_id!("x.p.n.t.v1~*"),
        gts_id!("x.p.n.t.v1.7~a.*"),
    ] {
        let parsed = GtsIdPattern::try_new(pattern).expect("pattern");
        let (lower, upper) = id_range(&filters(pattern)).expect("range");
        let upper = upper.expect("bounded");
        for id in ids {
            if GtsId::try_new(id).expect("id").matches_pattern(&parsed) {
                assert!(
                    id >= lower.as_str() && id < upper.as_str(),
                    "{id} outside the range of {pattern}"
                );
            }
        }
    }
}

#[test]
fn the_upper_bound_increments_the_last_byte() {
    assert_eq!(upper_bound("gts.acme.").as_deref(), Some("gts.acme/"));
    assert_eq!(upper_bound(""), None);
}

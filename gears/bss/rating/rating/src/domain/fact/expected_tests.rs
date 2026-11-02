#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_rating_core::engine::current::input::{
    BillingTiming, ChildKind, PolicyScope, RatingWindowKind,
};
use rust_decimal::Decimal;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{UsageShape, expected_children, usage_shape};
use crate::domain::child::{ChildFailure, ChildStatus};
use crate::domain::fact::model::FactVersion;
use crate::domain::fact::profile::{ReadFact, read_fact};
use crate::test_providers::subscriptions::{
    FactSpec, FakeSubscriptionsBilling, lot, recurring_binding, seal, term, usage_binding,
};

const OCT: &str = "2026-10-01T00:00:00Z";
const NOV: &str = "2026-11-01T00:00:00Z";
const METER: &str = "products.derived/cloudlet@1";

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

fn party() -> FakeSubscriptionsBilling {
    FakeSubscriptionsBilling::new(Uuid::from_u128(0x5e11))
}

fn read(spec: &FactSpec) -> FactVersion {
    match read_fact(&spec.payload()).unwrap() {
        ReadFact::Accepted(fact) => *fact,
        other => panic!("not accepted: {other:?}"),
    }
}

fn usage(window: RatingWindowKind) -> FactSpec {
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(item, Uuid::from_u128(0xa), window, METER, None);
    let mut spec =
        party()
            .fact("usage", item, OCT, NOV)
            .chargeable(OCT, NOV, term("u1", binding), Vec::new());
    "arrears".clone_into(&mut spec.timing);
    spec
}

#[test]
fn a_calendar_hour_binding_yields_744_children_for_october() {
    let expected =
        expected_children(&read(&usage(RatingWindowKind::CalendarHourUtc)), at(OCT)).unwrap();
    assert_eq!(expected.children.len(), 744);
    assert!(expected.schedule.is_some());
    assert!(expected.children.iter().all(|child| !child.at_intake
        && child.kind == ChildKind::UsageWindow
        && child.status == ChildStatus::Pending));
    let first = &expected.children[0];
    assert_eq!(first.window.window_start, at(OCT));
    assert_eq!(first.window.window_end, at("2026-10-01T01:00:00Z"));
    let mut ids: Vec<Uuid> = expected.children.iter().map(|c| c.key.child_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 744, "every hour is its own child");
}

#[test]
fn a_billing_cycle_binding_yields_one_child_for_the_period() {
    let expected =
        expected_children(&read(&usage(RatingWindowKind::BillingCycle)), at(OCT)).unwrap();
    assert_eq!(expected.children.len(), 1);
    assert_eq!(expected.children[0].window.window_start, at(OCT));
    assert_eq!(expected.children[0].window.window_end, at(NOV));
}

#[test]
fn f29_a_fact_opening_at_half_past_has_its_first_hour_served_from_half_past() {
    let mut spec = usage(RatingWindowKind::CalendarHourUtc);
    let open = "2026-10-01T10:30:00Z";
    spec.served.0 = open.to_owned();
    spec.slices[0].from = open.to_owned();
    let expected = expected_children(&read(&spec), at(OCT)).unwrap();
    let first = &expected.children[0];
    assert_eq!(first.window.window_start, at("2026-10-01T10:00:00Z"));
    assert_eq!(first.window.served_from, at(open));
}

#[test]
fn v85_hours_over_an_unresolved_slice_are_expected() {
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    let mid = "2026-10-10T00:00:00Z";
    let mut spec = party()
        .fact("usage", item, OCT, NOV)
        .chargeable(OCT, mid, term("u1", binding), Vec::new())
        .unresolved(mid, NOV, "continuation_missing");
    "arrears".clone_into(&mut spec.timing);
    assert_eq!(
        expected_children(&read(&spec), at(OCT))
            .unwrap()
            .children
            .len(),
        744
    );
}

#[test]
fn v86_bindings_that_disagree_on_the_window_fail_one_child_closed() {
    let item = Uuid::from_u128(0x18);
    let hourly = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    let cycle = usage_binding(
        item,
        Uuid::from_u128(0xb),
        RatingWindowKind::BillingCycle,
        METER,
        None,
    );
    let mid = "2026-10-15T00:00:00Z";
    let mut spec = party()
        .fact("usage", item, OCT, NOV)
        .chargeable(OCT, mid, term("u1", hourly), Vec::new())
        .chargeable(mid, NOV, term("u2", cycle), Vec::new());
    "arrears".clone_into(&mut spec.timing);
    let fact = read(&spec);
    assert_eq!(usage_shape(&fact), UsageShape::Mismatch);
    let expected = expected_children(&fact, at(OCT)).unwrap();
    assert_eq!(expected.children.len(), 1);
    assert!(expected.schedule.is_none());
    let child = &expected.children[0];
    assert_eq!(child.status, ChildStatus::Failed);
    assert_eq!(child.failure, Some(ChildFailure::UsagePolicyMismatch));
    assert!(child.at_intake);
}

#[test]
fn a_resource_scoped_policy_is_launch_gated() {
    let item = Uuid::from_u128(0x18);
    let mut binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        METER,
        None,
    );
    binding.usage_policy.as_mut().unwrap().aggregation_scope = PolicyScope::Resource;
    let binding = seal(binding).unwrap();
    let mut spec =
        party()
            .fact("usage", item, OCT, NOV)
            .chargeable(OCT, NOV, term("u1", binding), Vec::new());
    "arrears".clone_into(&mut spec.timing);
    let expected = expected_children(&read(&spec), at(OCT)).unwrap();
    assert_eq!(expected.children.len(), 1);
    assert_eq!(
        expected.children[0].failure,
        Some(ChildFailure::ResourceScopeNotSupported)
    );
}

#[test]
fn a_recurring_fact_has_one_period_line_woken_by_its_timing() {
    let item = Uuid::from_u128(0x17);
    let now = at("2026-10-01T08:00:00Z");
    for (timing, next) in [
        (BillingTiming::Advance, now),
        (BillingTiming::Arrears, at(NOV)),
    ] {
        let binding = recurring_binding(item, Uuid::from_u128(0x9), Decimal::new(10, 0), timing);
        let mut spec = party().fact("recurring", item, OCT, NOV).chargeable(
            OCT,
            NOV,
            term("t1", binding),
            vec![lot("A", "10", "t1")],
        );
        spec.timing = match timing {
            BillingTiming::Advance => "advance",
            BillingTiming::Arrears => "arrears",
        }
        .to_owned();
        let expected = expected_children(&read(&spec), now).unwrap();
        assert_eq!(expected.children.len(), 1);
        assert_eq!(expected.children[0].kind, ChildKind::PeriodLine);
        assert_eq!(expected.children[0].next_check_at, Some(next));
        assert!(expected.schedule.is_none());
    }
}

#[test]
fn v84_a_zeroed_version_and_a_one_time_fact_expect_no_child() {
    let mut zeroed = usage(RatingWindowKind::CalendarHourUtc);
    "zeroed".clone_into(&mut zeroed.obligation);
    zeroed.slices.clear();
    zeroed.terms.clear();
    assert!(
        expected_children(&read(&zeroed), at(OCT))
            .unwrap()
            .children
            .is_empty()
    );
}

#[test]
fn a_fully_nonchargeable_usage_fact_expects_no_child_and_an_unresolved_one_waits() {
    let item = Uuid::from_u128(0x18);
    let mut free = party()
        .fact("usage", item, OCT, NOV)
        .nonchargeable(OCT, NOV, "trial");
    "arrears".clone_into(&mut free.timing);
    let expected = expected_children(&read(&free), at(OCT)).unwrap();
    assert!(expected.children.is_empty());
    assert!(expected.schedule.is_none());

    let mut unresolved = party()
        .fact("usage", item, OCT, NOV)
        .nonchargeable(OCT, "2026-10-10T00:00:00Z", "trial")
        .unresolved("2026-10-10T00:00:00Z", NOV, "continuation_missing");
    "arrears".clone_into(&mut unresolved.timing);
    let expected = expected_children(&read(&unresolved), at(OCT)).unwrap();
    assert_eq!(expected.children.len(), 1);
    assert_eq!(expected.children[0].status, ChildStatus::Pending);
    assert!(expected.children[0].at_intake);
}

#[test]
fn the_manifest_is_the_set_of_children() {
    let fact = read(&usage(RatingWindowKind::CalendarHourUtc));
    let mut one = expected_children(&fact, at(OCT)).unwrap();
    let two = expected_children(&fact, at(NOV)).unwrap();
    assert_eq!(one.manifest_digest(), two.manifest_digest());
    one.children.reverse();
    assert_eq!(one.manifest_digest(), two.manifest_digest());
    one.children.pop();
    assert_ne!(one.manifest_digest(), two.manifest_digest());
}

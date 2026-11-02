//! Commercial fact intake (feature 06 §5.1; DECISIONS T-D-87) over SQLite: versioned facts of the
//! `subscriptions-rating/1` profile through the inbox, the bindings of record, the children and
//! schedules they create, and the pricing binding store's freshness reads.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use bss_pricing_sdk::read::PricingReadV1;
use bss_pricing_sdk::read::{
    BindingSelection, CatalogRef, PricePin as PricingPin, ResolveQuery, ResolvedBindings,
    ResolvedCell,
};
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::child::ChildStatus;
use bss_rating::domain::fact::intake::RebindOutcome;
use bss_rating::domain::fact::{BindingState, FailedBinding, PendingBinding};
use bss_rating::domain::inbox::{AcceptOutcome, QuarantineReason};
use bss_rating::domain::ports::Clock;
use bss_rating::domain::pricing::convert::to_pricing;
use bss_rating::domain::pricing::{
    PricingBindingStore, PricingClient, ResolveOutcome, ResolveQuestion,
};
use bss_rating::infra::storage::repo::fact_repo::test_reads;
use bss_rating::infra::storage::repo::pricing_repo::OrmPricingRepo;
use bss_rating::test_providers::PricingCall;
use bss_rating::test_providers::subscriptions::{
    FactSpec, FakeSubscriptionsBilling, lot, recurring_binding, term, usage_binding,
};
use bss_rating_core::engine::current::input::{BillingTiming, PricePin, RatingWindowKind};
use common::facts::{FactFixture, METER, fact_fixture};
use rust_decimal::Decimal;
use time::{Date, Month};
use uuid::Uuid;

const OCT: &str = "2026-10-01T00:00:00Z";
const NOV: &str = "2026-11-01T00:00:00Z";
const SELLER: Uuid = Uuid::from_u128(0x5e11);

fn party() -> FakeSubscriptionsBilling {
    FakeSubscriptionsBilling::new(SELLER)
}

fn recurring(party: &FakeSubscriptionsBilling, timing: BillingTiming, seats: &str) -> FactSpec {
    let item = Uuid::from_u128(0x17);
    let binding = recurring_binding(item, Uuid::from_u128(0x9), Decimal::new(10, 0), timing);
    let mut spec = party.fact("recurring", item, OCT, NOV).chargeable(
        OCT,
        NOV,
        term("t1", binding),
        vec![lot("A", seats, "t1")],
    );
    if timing == BillingTiming::Arrears {
        "arrears".clone_into(&mut spec.timing);
    }
    spec
}

fn usage(party: &FakeSubscriptionsBilling, window: RatingWindowKind) -> FactSpec {
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(item, Uuid::from_u128(0xa), window, METER, None);
    let mut spec =
        party
            .fact("usage", item, OCT, NOV)
            .chargeable(OCT, NOV, term("u1", binding), Vec::new());
    "arrears".clone_into(&mut spec.timing);
    spec
}

fn quarantine(outcome: &AcceptOutcome) -> QuarantineReason {
    match outcome {
        AcceptOutcome::Quarantined(entry) => entry.quarantine().unwrap().reason(),
        other => panic!("not quarantined: {other:?}"),
    }
}

async fn head(f: &FactFixture, fact_id: Uuid) -> bss_rating::domain::fact::FactHead {
    test_reads::head(&f.db().conn().unwrap(), SELLER, fact_id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn an_advance_recurring_fact_is_stored_bound_and_its_period_line_enqueued() {
    let f = fact_fixture("2026-10-01T08:00:00Z").await;
    let spec = recurring(&party(), BillingTiming::Advance, "10");
    let outcome = f.receive(spec.payload()).await.unwrap();
    assert!(matches!(outcome, AcceptOutcome::Applied(_)), "{outcome:?}");

    let head = head(&f, spec.fact_id).await;
    assert_eq!(head.current_version, 1);
    assert_eq!(head.binding, BindingState::Bound);
    assert!(head.expected_manifest_digest.is_some());
    assert_eq!(
        head.rollup_due_at, None,
        "an advance line rolls up when its child is final"
    );
    let children = test_reads::children(&f.db().conn().unwrap(), SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].child_kind, "period_line");
    assert_eq!(children[0].status, "pending");
    assert_eq!(
        children[0].next_check_at.unwrap(),
        f.clock_now(),
        "an advance line is evaluated at acceptance"
    );
    assert_eq!(f.queue.children().len(), 1);
    assert_eq!(f.queue.children()[0].child_id, children[0].child_id);
    // The accepted binding is the binding of record, stored once; intake asked Pricing nothing.
    assert_eq!(
        test_reads::answers(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(f.pricing.calls().is_empty());
}

#[tokio::test]
async fn v14_an_arrears_line_wakes_at_the_end_of_its_served_extent() {
    let f = fact_fixture("2026-10-01T08:00:00Z").await;
    let spec = recurring(&party(), BillingTiming::Arrears, "10");
    f.receive(spec.payload()).await.unwrap();
    let children = test_reads::children(&f.db().conn().unwrap(), SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(
        children[0].next_check_at.unwrap().to_string(),
        "2026-11-01 0:00:00.0 +00:00:00"
    );
    assert_eq!(
        head(&f, spec.fact_id)
            .await
            .rollup_due_at
            .unwrap()
            .to_string(),
        "2026-11-01 0:00:00.0 +00:00:00"
    );
}

#[tokio::test]
async fn a_redelivery_is_a_no_op_and_another_digest_under_one_version_is_quarantined() {
    let f = fact_fixture(OCT).await;
    let spec = recurring(&party(), BillingTiming::Advance, "10");
    f.receive(spec.payload()).await.unwrap();
    let again = f.receive(spec.payload()).await.unwrap();
    assert!(matches!(again, AcceptOutcome::Duplicate(_)), "{again:?}");
    let conflicting = recurring(&spec.party, BillingTiming::Advance, "11");
    let mut conflicting = conflicting;
    conflicting.fact_id = spec.fact_id;
    let outcome = f.receive(conflicting.payload()).await.unwrap();
    assert_eq!(quarantine(&outcome), QuarantineReason::DigestConflict);
    assert_eq!(
        test_reads::versions(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap(),
        vec![1]
    );
    assert_eq!(f.queue.children().len(), 1, "nothing was applied twice");
}

#[tokio::test]
async fn v60_a_sub_second_instant_is_quarantined_and_creates_nothing() {
    let f = fact_fixture(OCT).await;
    let mut spec = recurring(&party(), BillingTiming::Advance, "10");
    "2026-11-01T00:00:00.5Z".clone_into(&mut spec.anchor_at);
    let outcome = f.receive(spec.payload()).await.unwrap();
    assert_eq!(
        quarantine(&outcome),
        QuarantineReason::InstantNotWholeSecond
    );
    assert!(
        test_reads::head(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        test_reads::versions(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_binding_whose_digest_does_not_verify_is_quarantined_payload_invalid() {
    let f = fact_fixture(OCT).await;
    let mut spec = recurring(&party(), BillingTiming::Advance, "10");
    spec.terms[0].binding.price.money_digest = "ab".repeat(32);
    let outcome = f.receive(spec.payload()).await.unwrap();
    assert_eq!(quarantine(&outcome), QuarantineReason::PayloadInvalid);
    assert!(
        test_reads::head(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_newer_version_bumps_every_child_turns_final_stale_and_deletes_none() {
    let f = fact_fixture(OCT).await;
    let v1 = recurring(&party(), BillingTiming::Advance, "10");
    f.receive(v1.payload()).await.unwrap();
    let child = test_reads::children(&f.db().conn().unwrap(), SELLER, v1.fact_id)
        .await
        .unwrap()[0]
        .clone();
    test_reads::set_child_status(
        &f.db().conn().unwrap(),
        child.child_id,
        ChildStatus::Final.as_str(),
    )
    .await
    .unwrap();

    let mut v2 = v1.next_version();
    v2.slices[0].lots = vec![lot("A", "12", "t1")];
    f.receive(v2.payload()).await.unwrap();

    let children = test_reads::children(&f.db().conn().unwrap(), SELLER, v1.fact_id)
        .await
        .unwrap();
    assert_eq!(
        children.len(),
        1,
        "the same period_line, never a second one"
    );
    assert_eq!(children[0].input_generation, 1);
    assert_eq!(children[0].status, "final_stale");
    assert_eq!(head(&f, v1.fact_id).await.current_version, 2);
    assert_eq!(
        f.queue.children().len(),
        2,
        "enqueued at v1 and bumped at v2"
    );
}

#[tokio::test]
async fn v81_a_lower_version_after_a_higher_one_is_stored_and_touches_nothing() {
    let f = fact_fixture(OCT).await;
    let v1 = recurring(&party(), BillingTiming::Advance, "10");
    let mut v2 = v1.next_version();
    v2.slices[0].lots = vec![lot("A", "12", "t1")];
    // A distinct binding in v1, so a stored v1 answer would show.
    let mut v1 = v1;
    v1.terms[0] = term(
        "t1",
        recurring_binding(
            Uuid::from_u128(0x17),
            Uuid::from_u128(0x99),
            Decimal::new(7, 0),
            BillingTiming::Advance,
        ),
    );
    f.receive(v2.payload()).await.unwrap();
    let answers = test_reads::answers(&f.db().conn().unwrap(), SELLER)
        .await
        .unwrap()
        .len();
    let child = test_reads::children(&f.db().conn().unwrap(), SELLER, v1.fact_id)
        .await
        .unwrap()[0]
        .clone();
    let manifest = head(&f, v1.fact_id).await.expected_manifest_digest;

    let outcome = f.receive(v1.payload()).await.unwrap();
    assert!(matches!(outcome, AcceptOutcome::Applied(_)), "{outcome:?}");
    assert_eq!(
        test_reads::versions(&f.db().conn().unwrap(), SELLER, v1.fact_id)
            .await
            .unwrap(),
        vec![1, 2]
    );
    let head = head(&f, v1.fact_id).await;
    assert_eq!(head.current_version, 2);
    assert_eq!(head.expected_manifest_digest, manifest);
    assert_eq!(
        test_reads::answers(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .len(),
        answers
    );
    let after = test_reads::children(&f.db().conn().unwrap(), SELLER, v1.fact_id)
        .await
        .unwrap()[0]
        .clone();
    assert_eq!(after.input_generation, child.input_generation);
    assert_eq!(f.queue.children().len(), 1);
}

#[tokio::test]
async fn an_hourly_usage_fact_is_bound_with_744_expected_hours_and_a_pending_schedule() {
    let f = fact_fixture(OCT).await;
    let spec = usage(&party(), RatingWindowKind::CalendarHourUtc);
    f.receive(spec.payload()).await.unwrap();

    let head = head(&f, spec.fact_id).await;
    assert_eq!(head.binding, BindingState::Bound);
    assert_eq!(
        head.rollup_due_at.unwrap().to_string(),
        "2026-11-01 0:00:00.0 +00:00:00"
    );
    let schedules = test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].state, "policy_pending");
    assert_eq!(schedules[0].window_policy, "calendar_hour");
    assert_eq!(schedules[0].expected_count, 744);
    // The usage windows are the scheduler's to create when due (feature 06b).
    assert!(
        test_reads::children(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_empty()
    );
    let projections = test_reads::projections(&f.db().conn().unwrap(), SELLER)
        .await
        .unwrap();
    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].meter, METER);
    assert_eq!(projections[0].inputs[0]["name"], "ram");
    assert_eq!(f.declarations.reads(), 1);
}

#[tokio::test]
async fn v77_an_unreadable_declaration_leaves_the_fact_pending_until_it_binds() {
    let f = fact_fixture(OCT).await;
    f.declarations.set_unavailable(true);
    let spec = usage(&party(), RatingWindowKind::CalendarHourUtc);
    let outcome = f.receive(spec.payload()).await.unwrap();
    assert!(matches!(outcome, AcceptOutcome::Applied(_)), "{outcome:?}");

    let pending = head(&f, spec.fact_id).await;
    assert_eq!(
        pending.binding,
        BindingState::Pending(PendingBinding::DerivedDeclarationUnavailable)
    );
    assert!(pending.binding_next_check_at.is_some());
    assert_eq!(pending.expected_manifest_digest, None);
    assert_eq!(pending.rollup_due_at, None);
    assert!(
        test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.queue.binds().len(), 1);

    assert_eq!(
        f.intake.rebind(SELLER, spec.fact_id).await.unwrap(),
        RebindOutcome::StillPending
    );
    f.declarations.set_unavailable(false);
    assert_eq!(
        f.intake.rebind(SELLER, spec.fact_id).await.unwrap(),
        RebindOutcome::Bound
    );
    let bound = head(&f, spec.fact_id).await;
    assert_eq!(bound.binding, BindingState::Bound);
    assert!(bound.expected_manifest_digest.is_some());
    assert_eq!(
        test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.intake.rebind(SELLER, spec.fact_id).await.unwrap(),
        RebindOutcome::NotPending
    );
}

#[tokio::test]
async fn a_declaration_products_cannot_rate_fails_the_binding_with_an_exception() {
    let f = fact_fixture(OCT).await;
    let broken = "products.derived/broken@1";
    let mut declaration = bss_rating::test_providers::products::cloudlet_declaration();
    declaration.inputs.clear();
    f.declarations.declare(broken, declaration);
    let item = Uuid::from_u128(0x18);
    let binding = usage_binding(
        item,
        Uuid::from_u128(0xa),
        RatingWindowKind::CalendarHourUtc,
        broken,
        None,
    );
    let mut spec =
        party()
            .fact("usage", item, OCT, NOV)
            .chargeable(OCT, NOV, term("u1", binding), Vec::new());
    "arrears".clone_into(&mut spec.timing);
    f.receive(spec.payload()).await.unwrap();
    let head = head(&f, spec.fact_id).await;
    assert_eq!(
        head.binding,
        BindingState::Failed(FailedBinding::DerivedDeclarationConflict)
    );
    assert!(
        test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.queue.binds().is_empty(),
        "a failed binding is not retried"
    );
}

#[tokio::test]
async fn v86_and_a_gated_scope_fail_their_child_closed_with_an_exception() {
    let f = fact_fixture(OCT).await;
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
    f.receive(spec.payload()).await.unwrap();
    let children = test_reads::children(&f.db().conn().unwrap(), SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].status, "failed");
    assert_eq!(
        children[0].failure_reason.as_deref(),
        Some("usage_policy_mismatch")
    );
    assert!(
        test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.queue.children().is_empty(),
        "a failed child is not enqueued"
    );
}

#[tokio::test]
async fn v85_an_unresolved_slice_is_stored_bound_and_expected() {
    let f = fact_fixture(OCT).await;
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
    f.receive(spec.payload()).await.unwrap();
    assert_eq!(head(&f, spec.fact_id).await.binding, BindingState::Bound);
    let schedules = test_reads::schedules(&f.db().conn().unwrap(), SELLER, spec.fact_id)
        .await
        .unwrap();
    assert_eq!(schedules[0].expected_count, 744);
}

#[tokio::test]
async fn v84_a_zeroed_version_empties_the_manifest_and_keeps_its_children() {
    let f = fact_fixture(OCT).await;
    let v1 = recurring(&party(), BillingTiming::Advance, "10");
    f.receive(v1.payload()).await.unwrap();
    let manifest = head(&f, v1.fact_id).await.expected_manifest_digest;
    let mut v2 = v1.next_version();
    v2.obligation = "zeroed".to_owned();
    v2.slices.clear();
    v2.terms.clear();
    f.receive(v2.payload()).await.unwrap();
    let head = head(&f, v1.fact_id).await;
    assert_eq!(head.current_version, 2);
    assert_ne!(head.expected_manifest_digest, manifest);
    assert_eq!(
        head.rollup_due_at,
        Some(f.clock_now()),
        "rolled up at once as an empty result"
    );
    let children = test_reads::children(&f.db().conn().unwrap(), SELLER, v1.fact_id)
        .await
        .unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].input_generation, 1);
}

fn question(seller: Uuid, date: Date) -> ResolveQuestion {
    ResolveQuestion {
        catalog_tenant_id: seller,
        revision_id: Uuid::from_u128(0xd),
        date,
        item_id: None,
        pins: vec![PricePin {
            item_id: Uuid::from_u128(0x17),
            dimension_value: None,
            price_id: Uuid::from_u128(0x9),
        }],
    }
}

/// The one binding Pricing answers in these tests: built once, so two answers of it are equal.
fn answered_binding() -> &'static bss_rating_core::engine::current::input::CoreBinding {
    static BINDING: std::sync::OnceLock<bss_rating_core::engine::current::input::CoreBinding> =
        std::sync::OnceLock::new();
    BINDING.get_or_init(|| {
        recurring_binding(
            Uuid::from_u128(0x17),
            Uuid::from_u128(0x9),
            Decimal::new(10, 0),
            BillingTiming::Advance,
        )
    })
}

fn script(f: &FactFixture, seller: Uuid, date: Date) {
    let pricing = to_pricing(answered_binding()).unwrap();
    let query = ResolveQuery {
        catalog: CatalogRef { tenant_id: seller },
        revision_id: Uuid::from_u128(0xd),
        date,
        item_id: None,
        pins: vec![PricingPin {
            item_id: Uuid::from_u128(0x17),
            dimension_value: None,
            price_id: Uuid::from_u128(0x9),
        }],
    };
    f.pricing.on_resolve(
        query,
        Ok(ResolvedBindings {
            plan_id: Uuid::from_u128(0xe),
            revision_id: Uuid::from_u128(0xd),
            cells: vec![ResolvedCell {
                selection: BindingSelection {
                    item_id: pricing.item_id,
                    dimension_value: None,
                },
                binding: Some(pricing),
            }],
        }),
    );
}

fn date(month: Month, day: u8) -> Date {
    Date::from_calendar_date(2026, month, day).unwrap()
}

#[tokio::test]
async fn v82_two_periods_with_the_same_answer_are_two_questions_and_one_answer() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let (sep, oct) = (date(Month::September, 15), date(Month::October, 15));
    script(&f, SELLER, sep);
    script(&f, SELLER, oct);
    for day in [sep, oct] {
        let outcome = f
            .store
            .ensure(SELLER, &question(SELLER, day), None)
            .await
            .unwrap();
        assert!(
            matches!(outcome, ResolveOutcome::Answered { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(
        test_reads::resolves(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        test_reads::answers(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .len(),
        1
    );
    // October's question is found before Pricing is called again.
    let calls = f.pricing.calls().len();
    f.store
        .ensure(SELLER, &question(SELLER, oct), None)
        .await
        .unwrap();
    assert_eq!(f.pricing.calls().len(), calls);
}

#[tokio::test]
async fn a_first_final_needs_an_answer_of_its_window_s_day_or_later() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let oct = date(Month::October, 15);
    script(&f, SELLER, oct);
    f.store
        .ensure(SELLER, &question(SELLER, oct), None)
        .await
        .unwrap();
    f.clock.advance(Duration::from_hours(72));
    let calls = f.pricing.calls().len();
    // The stored answer of 15 October is too old for a window ending on 17 October.
    f.store
        .ensure(
            SELLER,
            &question(SELLER, oct),
            Some(date(Month::October, 17)),
        )
        .await
        .unwrap();
    assert_eq!(f.pricing.calls().len(), calls + 1);
    assert_eq!(
        test_reads::resolves(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn v59_a_resolve_for_another_seller_s_catalog_is_refused_and_stores_nothing() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    f.pricing.enforce_catalog_tenant();
    let (seller_a, seller_b) = (SELLER, Uuid::from_u128(0x5e12));
    let oct = date(Month::October, 15);
    script(&f, seller_a, oct);
    let own = f
        .store
        .ensure(seller_a, &question(seller_a, oct), None)
        .await
        .unwrap();
    assert!(matches!(own, ResolveOutcome::Answered { .. }), "{own:?}");
    let foreign = f
        .store
        .ensure(seller_b, &question(seller_a, oct), None)
        .await
        .unwrap();
    assert_eq!(
        foreign,
        ResolveOutcome::Refused {
            code: "PERMISSION_DENIED".to_owned()
        }
    );
    assert!(
        test_reads::resolves(&f.db().conn().unwrap(), seller_b)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        test_reads::answers(&f.db().conn().unwrap(), seller_b)
            .await
            .unwrap()
            .is_empty()
    );
    // Rating refuses before asking Pricing: only A's resolve was ever sent.
    assert_eq!(f.pricing.calls().len(), 1);
    assert!(matches!(f.pricing.calls()[0], PricingCall::Resolve(_)));
}

#[tokio::test]
async fn v59_a_warm_cache_does_not_answer_a_foreign_catalog() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let oct = date(Month::October, 15);
    script(&f, SELLER, oct);
    let own = f
        .store
        .ensure(SELLER, &question(SELLER, oct), None)
        .await
        .unwrap();
    assert!(matches!(own, ResolveOutcome::Answered { .. }), "{own:?}");
    // The same question with only the catalog changed has the same digest.
    let foreign = question(Uuid::from_u128(0x5e12), oct);
    assert_eq!(foreign.query_digest(), question(SELLER, oct).query_digest());
    let outcome = f.store.ensure(SELLER, &foreign, None).await.unwrap();
    assert_eq!(
        outcome,
        ResolveOutcome::Refused {
            code: "PERMISSION_DENIED".to_owned()
        }
    );
    assert_eq!(f.pricing.calls().len(), 1);
}

/// Pricing answering two concurrent resolves of one question differently: both callers reach it
/// (the barrier) and each gets the next answer.
struct Racing {
    barrier: tokio::sync::Barrier,
    answers: std::sync::Mutex<Vec<ResolvedBindings>>,
}

#[async_trait::async_trait]
impl PricingReadV1 for Racing {
    async fn resolve(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _query: ResolveQuery,
    ) -> Result<ResolvedBindings, toolkit_canonical_errors::CanonicalError> {
        self.barrier.wait().await;
        Ok(self.answers.lock().unwrap().pop().unwrap())
    }

    async fn price(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _query: bss_pricing_sdk::read::PriceQuery,
    ) -> Result<bss_pricing_sdk::read::ImmutablePrice, toolkit_canonical_errors::CanonicalError>
    {
        Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create())
    }

    async fn current_revision(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _query: bss_pricing_sdk::read::PlanQuery,
    ) -> Result<bss_pricing_sdk::read::RevisionRef, toolkit_canonical_errors::CanonicalError> {
        Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create())
    }
}

struct RacingClient(Arc<Racing>);

impl PricingClient for RacingClient {
    fn client(&self) -> Result<Arc<dyn PricingReadV1>, bss_rating::domain::error::DomainError> {
        Ok(Arc::clone(&self.0) as Arc<dyn PricingReadV1>)
    }
}

fn answer_of(binding: &bss_rating_core::engine::current::input::CoreBinding) -> ResolvedBindings {
    let pricing = to_pricing(binding).unwrap();
    ResolvedBindings {
        plan_id: Uuid::from_u128(0xe),
        revision_id: Uuid::from_u128(0xd),
        cells: vec![ResolvedCell {
            selection: BindingSelection {
                item_id: pricing.item_id,
                dimension_value: None,
            },
            binding: Some(pricing),
        }],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_fetches_of_one_question_on_one_day_both_return_the_answer_that_stands() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let oct = date(Month::October, 15);
    // Around an `ends_on` being published: one fetch sees the end, the other does not.
    let mut ended = answered_binding().clone();
    ended.price.ends_on = Some(date(Month::October, 20));
    let ended = bss_rating::test_providers::subscriptions::seal(ended).unwrap();
    let racing = Arc::new(Racing {
        barrier: tokio::sync::Barrier::new(2),
        answers: std::sync::Mutex::new(vec![answer_of(answered_binding()), answer_of(&ended)]),
    });
    let store = PricingBindingStore::new(
        f.db().clone(),
        Arc::new(OrmPricingRepo),
        Arc::new(RacingClient(racing)),
        Arc::new(ServiceGrant::new(f.pdp.enforcer())),
        Arc::clone(&f.clock) as Arc<dyn Clock>,
    );
    let q = question(SELLER, oct);
    let (one, two) = tokio::join!(
        store.ensure(SELLER, &q, None),
        store.ensure(SELLER, &q, None)
    );
    let answered = |outcome: ResolveOutcome| match outcome {
        ResolveOutcome::Answered { resolve, answer } => (*resolve, *answer),
        other => panic!("not answered: {other:?}"),
    };
    let (one, two) = (answered(one.unwrap()), answered(two.unwrap()));
    assert_eq!(
        one.0.binding_digest, two.0.binding_digest,
        "one answer stands for the day"
    );
    assert_eq!(one.1, two.1);
    assert_eq!(one.0.binding_digest, one.1.binding_digest);
    let conn = f.db().conn().unwrap();
    let resolves = test_reads::resolves(&conn, SELLER).await.unwrap();
    assert_eq!(resolves.len(), 1);
    assert_eq!(resolves[0].binding_digest, one.0.binding_digest.as_str());
    assert_eq!(
        test_reads::answers(&conn, SELLER).await.unwrap().len(),
        2,
        "both kept, by digest"
    );
}

#[tokio::test]
async fn two_item_filtered_answers_with_different_uncovered_cells_are_both_stored() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let oct = date(Month::October, 15);
    for item in [0x71_u128, 0x72] {
        let item_id = Uuid::from_u128(item);
        let mut question = question(SELLER, oct);
        question.item_id = Some(item_id);
        question.pins.clear();
        f.pricing.on_resolve(
            ResolveQuery {
                catalog: CatalogRef { tenant_id: SELLER },
                revision_id: question.revision_id,
                date: oct,
                item_id: Some(item_id),
                pins: Vec::new(),
            },
            Ok(ResolvedBindings {
                plan_id: Uuid::from_u128(0xe),
                revision_id: question.revision_id,
                cells: vec![ResolvedCell {
                    selection: BindingSelection {
                        item_id,
                        dimension_value: None,
                    },
                    binding: None,
                }],
            }),
        );
        let outcome = f.store.ensure(SELLER, &question, None).await.unwrap();
        assert!(
            matches!(outcome, ResolveOutcome::Answered { .. }),
            "{outcome:?}"
        );
    }
    let conn = f.db().conn().unwrap();
    assert_eq!(test_reads::answers(&conn, SELLER).await.unwrap().len(), 2);
    assert_eq!(test_reads::resolves(&conn, SELLER).await.unwrap().len(), 2);
}

#[tokio::test]
async fn pricing_unavailable_is_an_outcome_to_retry() {
    let f = fact_fixture("2026-10-15T08:00:00Z").await;
    let oct = date(Month::October, 15);
    let query = ResolveQuery {
        catalog: CatalogRef { tenant_id: SELLER },
        revision_id: Uuid::from_u128(0xd),
        date: oct,
        item_id: None,
        pins: vec![PricingPin {
            item_id: Uuid::from_u128(0x17),
            dimension_value: None,
            price_id: Uuid::from_u128(0x9),
        }],
    };
    f.pricing.on_resolve(
        query,
        Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let outcome = f
        .store
        .ensure(SELLER, &question(SELLER, oct), None)
        .await
        .unwrap();
    assert!(
        matches!(outcome, ResolveOutcome::Unavailable { .. }),
        "{outcome:?}"
    );
    assert!(
        test_reads::resolves(&f.db().conn().unwrap(), SELLER)
            .await
            .unwrap()
            .is_empty()
    );
}

trait ClockNow {
    fn clock_now(&self) -> time::OffsetDateTime;
}

impl ClockNow for FactFixture {
    fn clock_now(&self) -> time::OffsetDateTime {
        self.clock.now()
    }
}

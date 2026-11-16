//! Pure: interests in, a yes or no per event out. No storage, no runtime.

use crate::domain::model::Sequence;
use chrono::Utc;
use gts::{GtsIdPattern, GtsTypeId};
use serde_json::json;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::model::{BarrierMode, Event, FilterSpec, Interest, OwnershipFilter};

use super::filter::{EventFilter, InterestFilter};

fn gts(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("static gts id is valid")
}

fn pattern(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("static gts pattern is valid")
}

fn topic() -> GtsInstanceId {
    gts("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1")
}

fn other_topic() -> GtsInstanceId {
    gts("gts.cf.core.events.topic.v1~x.eb.other.acme.v1")
}

const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.orders.created.v1~";

fn interest(tenant: Uuid, patterns: &[&str]) -> Interest {
    Interest {
        topic: topic(),
        subject_tenant: tenant,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode: BarrierMode::Respect,
        types: patterns.iter().map(|p| pattern(p)).collect(),
        subject_types: Vec::new(),
        filter: None,
    }
}

fn event(tenant: Uuid, on_topic: GtsInstanceId, event_type: &str) -> Event {
    Event {
        id: Uuid::nil(),
        r#type: crate::test_support::event_type_id(event_type),
        topic: on_topic,
        subject_tenant: tenant,
        source: "filter-test".to_owned(),
        subject: "order".to_owned(),
        subject_type: GtsTypeId::new("gts.x.eb.orders.subject.v1~"),
        subject_owner: None,
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!({}),
        meta: None,
        ingest: None,
        partition: Some(0),
        sequence: Some(Sequence::assigned(1)),
        sequence_time: None,
    }
}

#[test]
fn an_exact_type_on_the_right_topic_and_tenant_matches() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED])]);

    assert!(filter.matches(&event(tenant, topic(), CREATED)));
}

#[test]
fn an_event_on_another_topic_is_rejected() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED])]);

    assert!(!filter.matches(&event(tenant, other_topic(), CREATED)));
}

#[test]
fn an_event_of_another_tenant_is_rejected() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED])]);

    // Segments are shared between subscriptions of different tenants, so this is
    // the predicate that keeps one tenant's events out of another's stream.
    assert!(!filter.matches(&event(Uuid::new_v4(), topic(), CREATED)));
}

#[test]
fn an_unmatched_type_is_rejected() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED])]);

    assert!(!filter.matches(&event(
        tenant,
        topic(),
        "gts.cf.core.events.event.v1~x.eb.orders.cancelled.v1~"
    )));
}

#[test]
fn the_all_types_wildcard_matches_every_event_type() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &["gts.cf.core.events.event.v1~*"])]);

    // `…event.v1~*` is the canonical "all event types" pattern that the SDK's
    // `.topics(..)` convenience emits; every concrete event type matches it via
    // the gts matcher's trailing-`*` rule.
    assert!(filter.matches(&event(tenant, topic(), CREATED)));
    assert!(filter.matches(&event(
        tenant,
        topic(),
        "gts.cf.core.events.event.v1~x.eb.orders.cancelled.v1~"
    )));
}

#[test]
fn a_bare_type_id_covers_its_derived_subtree() {
    let tenant = Uuid::new_v4();
    // A concrete type used as a pattern covers itself and anything derived from
    // it (GTS implicit derived-type coverage), and does not match a sibling.
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED])]);

    let derived = "gts.cf.core.events.event.v1~x.eb.orders.created.v1~acme.audit.log.entry.v1~";
    assert!(filter.matches(&event(tenant, topic(), derived)));
    assert!(!filter.matches(&event(
        tenant,
        topic(),
        "gts.cf.core.events.event.v1~x.eb.orders.cancelled.v1~"
    )));
}

#[test]
fn any_one_of_several_patterns_suffices() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(
        tenant,
        &[
            "gts.cf.core.events.event.v1~x.eb.orders.cancelled.v1~",
            CREATED,
        ],
    )]);

    assert!(filter.matches(&event(tenant, topic(), CREATED)));
}

#[test]
fn an_interest_with_no_patterns_matches_nothing() {
    let tenant = Uuid::new_v4();
    let filter = InterestFilter::compile(&[interest(tenant, &[])]);

    // Not "matches everything". An interest that names no types has declared no
    // interest, and defaulting to everything would deliver a tenant's whole
    // topic to a subscription that asked for none of it.
    assert!(!filter.matches(&event(tenant, topic(), CREATED)));
}

#[test]
fn no_interests_at_all_matches_nothing() {
    let filter = InterestFilter::compile(&[]);

    assert_eq!(filter.interest_count(), 0);
    assert!(!filter.matches(&event(Uuid::new_v4(), topic(), CREATED)));
}

// Pattern-grammar rejection (a partial wildcard like `orders*`, or more than
// one wildcard) is not tested here: it belongs to `GtsIdPattern::try_new`,
// which every interest type selector passes through at construction (the REST
// `InterestDto` -> `Interest` conversion maps its error to `BadTypePattern`).
// `InterestFilter::compile` therefore only ever receives already-valid
// patterns and is infallible.

#[test]
fn interests_on_several_topics_are_kept_apart() {
    let tenant = Uuid::new_v4();
    let mut second = interest(tenant, &[CREATED]);
    second.topic = other_topic();
    let filter = InterestFilter::compile(&[interest(tenant, &[CREATED]), second]);

    assert_eq!(filter.interest_count(), 2);
    assert!(filter.matches(&event(tenant, topic(), CREATED)));
    assert!(filter.matches(&event(tenant, other_topic(), CREATED)));
}

#[test]
fn a_filter_spec_compiles_and_does_not_yet_decide() {
    let tenant = Uuid::new_v4();
    let mut carrying = interest(tenant, &[CREATED]);
    carrying.filter = Some(FilterSpec {
        engine: gts("gts.cf.core.events.filter.v1~cf.core.expression.cel.v1"),
        expression: "event.data.total_cents > 100000".to_owned(),
    });
    let filter = InterestFilter::compile(&[carrying]);

    // The expression language and its engine belong to ADR-0005, still proposed.
    // Until an engine exists a `FilterSpec` neither admits nor rejects, so an
    // event matching topic, type and tenant is delivered. Rejecting the spec at
    // compile would fail a JOIN that the scenarios require to succeed.
    assert!(filter.matches(&event(tenant, topic(), CREATED)));
}

// -- The subject-type selector --

const SELECTOR_TENANT: Uuid = Uuid::from_u128(0x1901);
const CUSTOMER: &str = "gts.x.eb.orders.customer.v1~";
const SUPPLIER: &str = "gts.x.eb.orders.supplier.v1~";

/// `interest` with `subject_patterns` as its subject-type selector.
fn selecting(mut interest: Interest, subject_patterns: &[&str]) -> Interest {
    interest.subject_types = subject_patterns.iter().map(|p| pattern(p)).collect();
    interest
}

/// `event` about a subject of `subject_type`.
fn about(mut event: Event, subject_type: GtsTypeId) -> Event {
    event.subject_type = subject_type;
    event
}

#[test]
fn an_interest_with_a_subject_type_selector_matches_only_that_subject_type() {
    let filter = InterestFilter::compile(&[selecting(
        interest(SELECTOR_TENANT, &[CREATED]),
        &[CUSTOMER],
    )]);

    let created = event(SELECTOR_TENANT, topic(), CREATED);
    assert!(filter.matches(&about(created.clone(), GtsTypeId::new(CUSTOMER))));
    assert!(!filter.matches(&about(created, GtsTypeId::new(SUPPLIER))));
}

#[test]
fn an_interest_without_a_subject_type_selector_matches_every_subject_type() {
    let filter = InterestFilter::compile(&[interest(SELECTOR_TENANT, &[CREATED])]);

    let created = event(SELECTOR_TENANT, topic(), CREATED);
    assert!(filter.matches(&about(created.clone(), GtsTypeId::new(CUSTOMER))));
    assert!(filter.matches(&about(created, GtsTypeId::new(SUPPLIER))));
}

#[test]
fn a_subject_type_pattern_matches_its_derived_types() {
    let created = event(SELECTOR_TENANT, topic(), CREATED);
    let derived = GtsTypeId::new("gts.x.eb.orders.customer.v1~acme.retail.loyalty.member.v1~");

    // A bare type id covers its derived subtree, as for `types`.
    let bare = InterestFilter::compile(&[selecting(
        interest(SELECTOR_TENANT, &[CREATED]),
        &[CUSTOMER],
    )]);
    assert!(bare.matches(&about(created.clone(), derived.clone())));

    // A trailing `*` covers its prefix and nothing outside it.
    let wildcard = InterestFilter::compile(&[selecting(
        interest(SELECTOR_TENANT, &[CREATED]),
        &["gts.x.eb.orders.customer.v1~acme.*"],
    )]);
    assert!(wildcard.matches(&about(created.clone(), derived)));
    assert!(!wildcard.matches(&about(created, GtsTypeId::new(SUPPLIER))));
}

#[test]
fn a_subject_type_that_does_not_parse_matches_no_selector() {
    let filter = InterestFilter::compile(&[selecting(
        interest(SELECTOR_TENANT, &[CREATED]),
        &[CUSTOMER],
    )]);

    assert!(!filter.matches(&about(
        event(SELECTOR_TENANT, topic(), CREATED),
        GtsTypeId::new("not-gts"),
    )));
}

#[test]
fn the_subject_type_selector_narrows_but_the_type_still_decides() {
    let filter = InterestFilter::compile(&[selecting(
        interest(SELECTOR_TENANT, &[CREATED]),
        &[CUSTOMER],
    )]);

    assert!(!filter.matches(&about(
        event(
            SELECTOR_TENANT,
            topic(),
            "gts.cf.core.events.event.v1~x.eb.orders.cancelled.v1~"
        ),
        GtsTypeId::new(CUSTOMER),
    )));
}

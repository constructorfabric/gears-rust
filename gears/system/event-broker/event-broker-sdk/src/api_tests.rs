use super::api::{BarrierMode, Filter, InterestTenant, OwnershipFilter, SubscriptionInterest};
use crate::error::EventBrokerError;
use gts::{GtsIdPattern, GtsInstanceId};
use toolkit_gts::{GTS_ID_PREFIX, gts_id};

fn ginst(id: impl AsRef<str>) -> GtsInstanceId {
    GtsInstanceId::try_new(id.as_ref()).expect("valid GTS instance id")
}

fn gpat(id: impl AsRef<str>) -> GtsIdPattern {
    GtsIdPattern::try_new(id.as_ref()).expect("valid GTS pattern")
}

#[test]
fn barrier_mode_default_is_respect() {
    assert_eq!(BarrierMode::default(), BarrierMode::Respect);
}

// The snake_case wire form of `BarrierMode` is a property of the wire DTO
// (`rest::wire::BarrierModeDto`), not of the SDK type, which carries no serde -
// asserted in the rest wire-DTO tests.

#[test]
fn subscription_interest_full_construction() {
    let interest = SubscriptionInterest::builder()
        .topic(ginst(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")))
        .subject_tenant(uuid::Uuid::from_u128(0x1919))
        .ownership_filter(OwnershipFilter::TenantSubtreeIncluded)
        .barrier_mode(BarrierMode::Ignore)
        .types([gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~acme.orders.*"
        ))])
        .filter(
            Filter::new(
                ginst(gts_id!(
                    "cf.core.events.filter.v1~cf.core.expression.cel.v1"
                )),
                "event.data.amount > 100",
            )
            .unwrap(),
        )
        .build()
        .unwrap();

    assert_eq!(
        interest.topic().as_ref(),
        gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")
    );
    assert_eq!(
        interest.subject_tenant(),
        InterestTenant::Tenant(uuid::Uuid::from_u128(0x1919))
    );
    assert_eq!(interest.types().len(), 1);
    assert!(interest.filter().is_some());
    assert_eq!(
        interest.ownership_filter(),
        OwnershipFilter::TenantSubtreeIncluded
    );
    assert_eq!(interest.barrier_mode(), BarrierMode::Ignore);
}

#[test]
fn subscription_interest_rejects_missing_types() {
    let err = SubscriptionInterest::builder()
        .topic(ginst(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")))
        .subject_tenant(uuid::Uuid::from_u128(0x1919))
        .barrier_mode(BarrierMode::Respect)
        .types(std::iter::empty::<GtsIdPattern>())
        .build()
        .unwrap_err();

    assert!(matches!(
        err,
        EventBrokerError::InvalidConsumerOptions { .. }
    ));
    assert!(
        err.to_string().contains("event types must be 1..=32"),
        "unexpected error: {err}"
    );
}

#[test]
fn filter_rejects_empty_expression() {
    let err = Filter::new(
        ginst(gts_id!(
            "cf.core.events.filter.v1~cf.core.expression.cel.v1"
        )),
        "",
    )
    .unwrap_err();

    assert!(matches!(
        err,
        EventBrokerError::InvalidConsumerOptions { .. }
    ));
    assert!(
        err.to_string().contains("filter expression"),
        "unexpected error: {err}"
    );
}

/// An interest on the orders topic in a fixed tenant, selecting every order type.
fn orders_interest() -> super::api::SubscriptionInterestBuilder {
    SubscriptionInterest::builder()
        .topic(ginst(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")))
        .subject_tenant(uuid::Uuid::from_u128(0x1919))
        .types([gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~acme.orders.*"
        ))])
}

#[test]
fn subscription_interest_refuses_a_nil_tenant() {
    let result = orders_interest().subject_tenant(uuid::Uuid::nil()).build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest subject_tenant must not be the nil UUID"
    );
}

#[test]
fn an_interest_missing_its_topic_and_tenant_reports_the_topic() {
    let result = SubscriptionInterest::builder()
        .types([gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~acme.orders.*"
        ))])
        .build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(detail, "subscription interest topic is required");
}

#[test]
fn an_interest_that_names_no_tenant_or_ownership_filter_reads_the_callers_mailbox() {
    let interest = SubscriptionInterest::builder()
        .topic(ginst(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")))
        .types([gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~acme.orders.*"
        ))])
        .build()
        .expect("an interest with no tenant builds");

    assert_eq!(
        (interest.subject_tenant(), interest.ownership_filter()),
        (InterestTenant::Caller, OwnershipFilter::OnlyMine)
    );
}

#[test]
fn an_interest_missing_its_types_reports_the_types() {
    let result = SubscriptionInterest::builder()
        .topic(ginst(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")))
        .build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest event types must be 1..=32 entries (got 0)"
    );
}

#[test]
fn an_interest_with_a_nil_tenant_and_no_types_reports_the_nil_tenant() {
    let result = orders_interest()
        .subject_tenant(uuid::Uuid::nil())
        .types(std::iter::empty::<GtsIdPattern>())
        .build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest subject_tenant must not be the nil UUID"
    );
}

#[test]
fn an_interest_with_no_types_and_too_many_subject_types_reports_the_types() {
    let result = orders_interest()
        .types(std::iter::empty::<GtsIdPattern>())
        .subject_types((0..33).map(|i| gpat(format!("gts.acme.orders.x.subject{i}.v1~"))))
        .build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest event types must be 1..=32 entries (got 0)"
    );
}

#[test]
fn subscription_interest_carries_its_subject_type_selector() {
    let interest = orders_interest()
        .subject_types([gpat("gts.acme.orders.x.customer.v1~")])
        .build()
        .unwrap();

    assert_eq!(
        interest.subject_types(),
        [gpat("gts.acme.orders.x.customer.v1~")]
    );
}

#[test]
fn subscription_interest_without_a_subject_type_selector_has_none() {
    let interest = orders_interest().build().unwrap();

    assert_eq!(interest.subject_types(), Vec::<GtsIdPattern>::new());
}

#[test]
fn subscription_interest_refuses_33_subject_types() {
    let result = orders_interest()
        .subject_types((0..33).map(|i| gpat(format!("gts.acme.orders.x.subject{i}.v1~"))))
        .build();

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest subject types must be at most 32 entries (got 33)"
    );
}

#[test]
fn subscription_interest_accepts_32_subject_types() {
    let selector: Vec<GtsIdPattern> = (0..32)
        .map(|i| gpat(format!("gts.acme.orders.x.subject{i}.v1~")))
        .collect();

    let interest = orders_interest()
        .subject_types(selector.clone())
        .build()
        .unwrap();

    assert_eq!(interest.subject_types(), selector);
}

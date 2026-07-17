use super::api::{BarrierMode, Filter, SubscriptionInterest, TenantTraversalDepth};
use crate::error::EventBrokerError;
use toolkit_gts::{GTS_ID_PREFIX, gts_id};

#[test]
fn barrier_mode_default_is_respect() {
    assert_eq!(BarrierMode::default(), BarrierMode::Respect);
}

#[test]
fn barrier_mode_serialises_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&BarrierMode::Respect).unwrap(),
        "\"respect\""
    );
    assert_eq!(
        serde_json::to_string(&BarrierMode::Ignore).unwrap(),
        "\"ignore\""
    );
}

#[test]
fn subscription_interest_full_construction() {
    let interest = SubscriptionInterest::builder()
        .topic(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1"))
        .tenant_id(uuid::Uuid::nil())
        .tenant_depth(TenantTraversalDepth::direct_children())
        .barrier_mode(BarrierMode::Ignore)
        .types([format!(
            "{GTS_ID_PREFIX}cf.core.events.event_type.v1~acme.orders.*"
        )])
        .filter(
            Filter::new(
                gts_id!("cf.core.events.filter.v1~cf.core.expression.cel.v1"),
                "event.data.amount > 100",
            )
            .unwrap(),
        )
        .build()
        .unwrap();

    assert_eq!(
        interest.topic(),
        gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1")
    );
    assert_eq!(interest.tenant_id(), uuid::Uuid::nil());
    assert_eq!(interest.types().len(), 1);
    assert!(interest.filter().is_some());
    assert_eq!(
        interest.tenant_depth(),
        TenantTraversalDepth::direct_children()
    );
    assert_eq!(interest.barrier_mode(), BarrierMode::Ignore);
}

#[test]
fn subscription_interest_rejects_missing_types() {
    let err = SubscriptionInterest::builder()
        .topic(gts_id!("cf.core.events.topic.v1~acme.orders.x.x.v1"))
        .tenant_id(uuid::Uuid::nil())
        .tenant_depth(TenantTraversalDepth::CurrentTenant)
        .barrier_mode(BarrierMode::Respect)
        .types(std::iter::empty::<&str>())
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
        gts_id!("cf.core.events.filter.v1~cf.core.expression.cel.v1"),
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

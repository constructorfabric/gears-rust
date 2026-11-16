//! The interest's ownership filter and barrier mode on the stored wire form.

use serde_json::json;
use uuid::Uuid;

use super::model::{BarrierMode, Interest, OwnershipFilter};

fn interest_json(overrides: &serde_json::Value) -> serde_json::Value {
    let mut base = json!({
        "topic": "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
        "subject_tenant": "fec5ff3e-4d0d-416e-95e0-c354028c1b12",
        "types": ["gts.cf.core.events.event.v1~x.eb.orders.created.v1~"],
    });
    if let (Some(base), Some(overrides)) = (base.as_object_mut(), overrides.as_object()) {
        for (key, value) in overrides {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn ownership_of(overrides: &serde_json::Value) -> OwnershipFilter {
    serde_json::from_value::<Interest>(interest_json(overrides))
        .expect("interest deserializes")
        .ownership
}

#[test]
fn an_absent_ownership_filter_is_only_mine() {
    assert_eq!(ownership_of(&json!({})), OwnershipFilter::OnlyMine);
    assert_eq!(OwnershipFilter::default(), OwnershipFilter::OnlyMine);
}

#[test]
fn ownership_filter_wire_tokens_agree_with_its_serde_renaming() {
    for (filter, token) in [
        (OwnershipFilter::OnlyMine, "only_mine"),
        (OwnershipFilter::EntireTenant, "entire_tenant"),
        (
            OwnershipFilter::TenantSubtreeIncluded,
            "tenant_subtree_included",
        ),
    ] {
        assert_eq!(filter.as_wire(), token);
        assert_eq!(
            serde_json::to_value(filter).expect("filter serializes"),
            json!(token)
        );
        assert_eq!(ownership_of(&json!({ "ownership_filter": token })), filter);
    }
}

/// A stored `Subscription` round-trips through this shape in the cluster
/// cache, so the serialized form is asserted whole rather than field by field.
#[test]
fn an_interest_serializes_to_the_full_wire_shape() {
    let interest = Interest {
        topic: toolkit_gts::GtsInstanceId::try_new(
            "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
        )
        .expect("static topic id is valid"),
        subject_tenant: Uuid::parse_str("fec5ff3e-4d0d-416e-95e0-c354028c1b12")
            .expect("static uuid"),
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode: BarrierMode::Ignore,
        types: vec![
            gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~x.eb.orders.created.v1~")
                .unwrap(),
        ],
        subject_types: Vec::new(),
        filter: None,
    };

    assert_eq!(
        serde_json::to_value(&interest).expect("interest serializes"),
        json!({
            "topic": "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
            "subject_tenant": "fec5ff3e-4d0d-416e-95e0-c354028c1b12",
            "ownership_filter": "entire_tenant",
            "barrier_mode": "ignore",
            "types": ["gts.cf.core.events.event.v1~x.eb.orders.created.v1~"],
            "filter": null,
        })
    );
}

#[test]
fn barrier_mode_defaults_to_respect_when_absent() {
    let interest: Interest =
        serde_json::from_value(interest_json(&json!({}))).expect("interest deserializes");

    assert_eq!(interest.barrier_mode, BarrierMode::Respect);
    assert_eq!(BarrierMode::default(), BarrierMode::Respect);
}

#[test]
fn barrier_mode_wire_tokens_agree_with_its_serde_renaming() {
    for mode in [BarrierMode::Respect, BarrierMode::Ignore] {
        assert_eq!(
            serde_json::to_value(mode).expect("mode serializes"),
            json!(mode.as_wire()),
            "as_wire disagrees with serde for {mode:?}"
        );
        assert_eq!(BarrierMode::from_wire(mode.as_wire()), Some(mode));
    }
}

#[test]
fn an_unrecognised_barrier_mode_is_rejected_rather_than_defaulted() {
    assert_eq!(BarrierMode::from_wire("traverse"), None);
    assert_eq!(BarrierMode::from_wire("Respect"), None);
    assert_eq!(BarrierMode::from_wire(""), None);
}

#[test]
fn an_interest_with_a_subject_type_selector_keeps_it_through_the_cache_shape() {
    let interest = Interest {
        topic: toolkit_gts::GtsInstanceId::try_new(
            "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
        )
        .expect("static topic id is valid"),
        subject_tenant: Uuid::parse_str("fec5ff3e-4d0d-416e-95e0-c354028c1b12")
            .expect("static uuid"),
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode: BarrierMode::Respect,
        types: vec![
            gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~x.eb.orders.created.v1~")
                .unwrap(),
        ],
        subject_types: vec![gts::GtsIdPattern::try_new("gts.x.eb.orders.customer.v1~").unwrap()],
        filter: None,
    };

    let stored = serde_json::to_value(&interest).expect("interest serializes");
    assert_eq!(
        stored,
        json!({
            "topic": "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
            "subject_tenant": "fec5ff3e-4d0d-416e-95e0-c354028c1b12",
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
            "types": ["gts.cf.core.events.event.v1~x.eb.orders.created.v1~"],
            "subject_types": ["gts.x.eb.orders.customer.v1~"],
            "filter": null,
        })
    );
    let read: Interest = serde_json::from_value(stored).expect("interest deserializes");
    assert_eq!(
        read.subject_types,
        vec![gts::GtsIdPattern::try_new("gts.x.eb.orders.customer.v1~").unwrap()]
    );
}

#[test]
fn a_stored_interest_written_without_subject_types_reads_as_no_selector() {
    let interest: Interest =
        serde_json::from_value(interest_json(&json!({}))).expect("interest deserializes");

    assert_eq!(interest.subject_types, Vec::<gts::GtsIdPattern>::new());
}

//! Routing resolution (feature 0005 §8): a fully qualified key wins over the
//! instance-less key, and a topic no specific key names takes the
//! instance-less route.

use std::collections::BTreeMap;

use toolkit_gts::GtsInstanceId;

use super::{Name, Routing};
use crate::domain::resolution::Sourced;

const AUDIT_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.audit.topic.v1";
const ORDERS_TOPIC: &str = "gts.cf.core.events.topic.v1~vendor.app.orders.topic.v1";

fn name(raw: &str) -> Name {
    Name::parse(raw).expect("a valid name")
}

fn topic(raw: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(raw).expect("a valid topic id")
}

fn routing() -> Routing {
    Routing::new(
        name("main"),
        BTreeMap::from([(AUDIT_TOPIC.to_owned(), name("audit"))]),
    )
}

#[test]
fn a_fully_qualified_key_wins_over_the_instance_less_key() {
    assert_eq!(
        routing().resolve(&topic(AUDIT_TOPIC)),
        Sourced::configured_for_topic(name("audit"))
    );
}

#[test]
fn a_topic_no_specific_key_names_takes_the_instance_less_route() {
    assert_eq!(
        routing().resolve(&topic(ORDERS_TOPIC)),
        Sourced::configured_for_type(name("main"))
    );
}

#[test]
fn every_routed_name_and_only_the_topic_keys_are_listed() {
    assert_eq!(
        routing().names().cloned().collect::<Vec<_>>(),
        vec![name("main"), name("audit")]
    );
    assert_eq!(
        routing().topic_keys().collect::<Vec<_>>(),
        vec![AUDIT_TOPIC]
    );
}

const SPECIAL_TYPE: &str = "gts.cf.core.events.topic.v1~vendor.app.special.topic.v1~";
const SPECIAL_ONE: &str =
    "gts.cf.core.events.topic.v1~vendor.app.special.topic.v1~vendor.app.one.topic.v1";
const SPECIAL_TWO: &str =
    "gts.cf.core.events.topic.v1~vendor.app.special.topic.v1~vendor.app.two.topic.v1";

/// A key may name a topic type derived from the base: it routes every topic
/// of that type, a key naming one of them still wins, and a topic of the
/// base type keeps the instance-less route.
#[test]
fn a_derived_type_key_routes_its_topics_below_their_own_keys() {
    let routing = Routing::new(
        name("main"),
        BTreeMap::from([
            (SPECIAL_TYPE.to_owned(), name("special")),
            (SPECIAL_ONE.to_owned(), name("audit")),
        ]),
    );

    assert_eq!(
        routing.resolve(&topic(SPECIAL_ONE)),
        Sourced::configured_for_topic(name("audit"))
    );
    assert_eq!(
        routing.resolve(&topic(SPECIAL_TWO)),
        Sourced::configured_for_type(name("special"))
    );
    assert_eq!(
        routing.resolve(&topic(ORDERS_TOPIC)),
        Sourced::configured_for_type(name("main"))
    );
    assert_eq!(
        routing.topic_keys().collect::<Vec<_>>(),
        vec![SPECIAL_ONE],
        "a type key names no one topic"
    );
}

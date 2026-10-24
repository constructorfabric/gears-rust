//! The PEP resources the broker asks about: each names the concrete broker
//! resource, so a decision point can match a grant against the type string.

use gts::{GtsIdPattern, GtsTypeId};
use toolkit_gts::GtsInstanceId;

use super::{
    CONSUMER_GROUP_FAMILY_RESOURCE, consumer_group_resource, event_type_interest_resource,
    event_type_resource, topic_resource,
};

const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~";

fn pattern(raw: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(raw).expect("test pattern must be a valid GTS pattern")
}

#[test]
fn a_published_event_is_checked_as_its_own_type() {
    let resource = event_type_resource(&GtsTypeId::new(EVENT_TYPE));
    assert_eq!(resource.name(), EVENT_TYPE);
}

#[test]
fn an_exact_interest_is_checked_as_the_type_it_names() {
    assert_eq!(
        event_type_interest_resource(&pattern(EVENT_TYPE)).name(),
        EVENT_TYPE
    );
    // The event base type is a pattern without a wildcard too: it is
    // checked as itself, which is what a family grant on events covers.
    assert_eq!(
        event_type_interest_resource(&pattern("gts.cf.core.events.event.v1~")).name(),
        "gts.cf.core.events.event.v1~"
    );
}

#[test]
fn a_wildcard_interest_is_checked_on_the_event_base_type() {
    // A wildcard pattern is not a GTS identifier, so it never becomes the
    // resource type; the base type is, and the pattern stays in the
    // `event_type_id` property.
    let resource = event_type_interest_resource(&pattern("gts.cf.core.events.event.v1~x.eb.*"));
    assert_eq!(resource.name(), "gts.cf.core.events.event.v1~");
}

#[test]
fn topic_and_named_group_checks_name_the_concrete_instance() {
    let topic = GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1")
        .expect("valid topic id");
    assert_eq!(topic_resource(&topic).name(), topic.as_ref());

    let group = GtsInstanceId::try_new("gts.cf.core.events.consumer_group.v1~x.eb.t1.audit.v1")
        .expect("valid group id");
    assert_eq!(consumer_group_resource(&group).name(), group.as_ref());
}

#[test]
fn defining_a_group_is_checked_on_the_consumer_group_family() {
    // Nothing concrete exists yet when an anonymous group is created.
    assert_eq!(
        CONSUMER_GROUP_FAMILY_RESOURCE.name(),
        "gts.cf.core.events.consumer_group.v1~"
    );
}

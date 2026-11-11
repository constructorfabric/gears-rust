//! The small types of the activation state.

use super::{ActivationPairs, ActivationStatus, SubscriberTenant, tenant_text};
use uuid::Uuid;

fn pair(subscriber: &str, tenant: &str) -> SubscriberTenant {
    SubscriberTenant {
        subscriber: subscriber.to_owned(),
        tenant: tenant.to_owned(),
    }
}

#[test]
fn the_status_spellings_are_the_stored_ones() {
    for (status, text) in [
        (ActivationStatus::Awaiting, "awaiting"),
        (ActivationStatus::Succeeded, "succeeded"),
        (ActivationStatus::Failed, "failed"),
    ] {
        assert_eq!(status.as_str(), text);
        assert_eq!(ActivationStatus::parse(text), Some(status));
    }
    assert_eq!(ActivationStatus::parse("expired"), None);
}

#[test]
fn a_request_remembers_each_pair_once_in_first_touch_order() {
    let mut pairs = ActivationPairs::new();
    assert!(pairs.is_empty());
    pairs.remember([pair("b", "t1"), pair("a", "t1")]);
    pairs.remember([pair("a", "t1"), pair("a", "t2")]);
    assert_eq!(
        pairs.pairs(),
        &[pair("b", "t1"), pair("a", "t1"), pair("a", "t2")]
    );
}

#[test]
fn a_tenant_is_a_bare_lowercase_uuid_string() {
    let tenant = Uuid::parse_str("AABBCCDD-0000-0000-0000-00000000000F").expect("uuid");
    assert_eq!(tenant_text(tenant), "aabbccdd-0000-0000-0000-00000000000f");
}

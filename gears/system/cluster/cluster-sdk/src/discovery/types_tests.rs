// Created: 2026-06-11 by Constructor Tech
use std::time::SystemTime;

use super::{DiscoveryFilter, InstanceState, MetaMatch, ServiceInstance, StateFilter};

fn instance(state: InstanceState, metadata: &[(&str, &str)]) -> ServiceInstance {
    ServiceInstance {
        instance_id: "i-1".to_owned(),
        address: "10.0.0.1:9000".to_owned(),
        metadata: metadata
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        state,
        registered_at: SystemTime::UNIX_EPOCH,
    }
}

#[test]
fn default_filter_is_enabled_only_with_no_metadata() {
    let filter = DiscoveryFilter::default();
    assert_eq!(filter.state, StateFilter::Enabled);
    assert!(filter.metadata.is_empty());
    assert!(filter.matches(&instance(InstanceState::Enabled, &[])));
    assert!(!filter.matches(&instance(InstanceState::Disabled, &[])));
}

#[test]
fn any_constructor_matches_every_state() {
    let filter = DiscoveryFilter::any();
    assert_eq!(filter.state, StateFilter::Any);
    assert!(filter.matches(&instance(InstanceState::Enabled, &[])));
    assert!(filter.matches(&instance(InstanceState::Disabled, &[])));
}

#[test]
fn disabled_state_filter_selects_only_disabled() {
    let filter = DiscoveryFilter::default().with_state(StateFilter::Disabled);
    assert!(!filter.matches(&instance(InstanceState::Enabled, &[])));
    assert!(filter.matches(&instance(InstanceState::Disabled, &[])));
}

#[test]
fn equals_predicate_requires_exact_value() {
    let filter = DiscoveryFilter::default()
        .require_metadata("region", MetaMatch::Equals("us-east".to_owned()));
    assert!(filter.matches(&instance(InstanceState::Enabled, &[("region", "us-east")])));
    assert!(!filter.matches(&instance(InstanceState::Enabled, &[("region", "us-west")])));
    // Absent key never matches.
    assert!(!filter.matches(&instance(InstanceState::Enabled, &[])));
}

#[test]
fn one_of_predicate_matches_any_listed_value() {
    let predicate = MetaMatch::OneOf(vec!["a".to_owned(), "b".to_owned()]);
    assert!(predicate.matches(Some("a")));
    assert!(predicate.matches(Some("b")));
    assert!(!predicate.matches(Some("c")));
    assert!(!predicate.matches(None));
}

#[test]
fn metadata_predicates_are_and_conjoined() {
    let filter = DiscoveryFilter::default()
        .require_metadata("region", MetaMatch::Equals("us-east".to_owned()))
        .require_metadata(
            "topic-shard",
            MetaMatch::OneOf(vec!["0".to_owned(), "1".to_owned()]),
        );
    // Both predicates satisfied.
    assert!(filter.matches(&instance(
        InstanceState::Enabled,
        &[("region", "us-east"), ("topic-shard", "1")],
    )));
    // First satisfied, second not — AND fails.
    assert!(!filter.matches(&instance(
        InstanceState::Enabled,
        &[("region", "us-east"), ("topic-shard", "2")],
    )));
}

#[test]
fn metadata_match_on_enabled_state_still_honors_state_dimension() {
    // A metadata match on a disabled instance is excluded by the default
    // enabled-only state dimension.
    let filter = DiscoveryFilter::default()
        .require_metadata("region", MetaMatch::Equals("us-east".to_owned()));
    assert!(!filter.matches(&instance(InstanceState::Disabled, &[("region", "us-east")])));
}

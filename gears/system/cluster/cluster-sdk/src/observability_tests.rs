// Created: 2026-06-11 by Constructor Tech
use super::{METRIC_LABEL_ALLOWLIST, fields, logs, metrics, spans};

const ALL_SPANS: &[&str] = &[
    spans::CACHE_GET,
    spans::CACHE_PUT,
    spans::CACHE_DELETE,
    spans::CACHE_CONTAINS,
    spans::CACHE_PUT_IF_ABSENT,
    spans::CACHE_COMPARE_AND_SWAP,
    spans::CACHE_WATCH,
    spans::CACHE_WATCH_PREFIX,
    spans::LEADER_ELECT,
    spans::LEADER_RENEW,
    spans::LEADER_RESIGN,
    spans::LOCK_TRY_LOCK,
    spans::LOCK_LOCK,
    spans::LOCK_RENEW,
    spans::LOCK_RELEASE,
    spans::DISCOVERY_REGISTER,
    spans::DISCOVERY_DISCOVER,
    spans::DISCOVERY_WATCH,
    spans::DISCOVERY_DEREGISTER,
];

const ALL_METRICS: &[&str] = &[
    metrics::CACHE_OPS_TOTAL,
    metrics::CACHE_OP_DURATION_SECONDS,
    metrics::LOCK_OPS_TOTAL,
    metrics::LOCK_OP_DURATION_SECONDS,
    metrics::LEADER_TRANSITIONS_TOTAL,
    metrics::DISCOVERY_OPS_TOTAL,
    metrics::WATCH_RESETS_TOTAL,
    metrics::PROVIDER_ERRORS_TOTAL,
];

const ALL_HIGH_CARDINALITY_FIELDS: &[&str] = &[
    fields::attr::KEY,
    fields::attr::NAME,
    fields::attr::LOCK,
    fields::attr::ELECTION,
    fields::attr::INSTANCE_ID,
    fields::attr::PROFILE,
];

#[test]
fn log_event_names_are_dotted_lowercase() {
    for name in [
        logs::LEADER_TRANSITION,
        logs::WATCH_RESET,
        logs::PROVIDER_ERROR,
    ] {
        assert!(name.starts_with("cluster."), "log event `{name}` namespace");
        assert_eq!(name, name.to_lowercase());
    }
}

#[test]
fn signal_names_are_unique() {
    let mut all: Vec<&str> = Vec::new();
    all.extend_from_slice(ALL_SPANS);
    all.extend_from_slice(ALL_METRICS);
    let count = all.len();
    all.sort_unstable();
    all.dedup();
    assert_eq!(count, all.len(), "signal names must be unique");
}

/// The hard cardinality rule (ADR-004 / acceptance criterion 4): no
/// high-cardinality field may appear in the metric-label allowlist.
#[test]
fn no_high_cardinality_field_is_a_metric_label() {
    for field in ALL_HIGH_CARDINALITY_FIELDS {
        assert!(
            !METRIC_LABEL_ALLOWLIST.contains(field),
            "high-cardinality field `{field}` must never be a metric label"
        );
    }
}

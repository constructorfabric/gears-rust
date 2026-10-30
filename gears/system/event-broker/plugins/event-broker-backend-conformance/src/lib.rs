//! # Event-broker backend conformance
//!
//! The storage backend contract (feature 0007, `event_broker_sdk::backend`)
//! written once as scenarios, and run by every backend plugin against its own
//! medium from its own tests. A plugin implements [`Harness`] and calls
//! [`run_all`], or runs scenarios one by one.
//!
//! Scenarios isolate themselves: each provisions a topic of its own under a
//! random name and fences its appends under an outbox identity of its own, so
//! one medium serves every scenario and two scenarios never see each other's
//! events or fence state.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "a conformance suite: a failed expectation is the test failure"
)]

mod append;
mod dedup;
mod durability;
mod fixture;
mod harness;
mod lifecycle;
mod provision;
mod read;
mod resolve;
mod retention;
mod suite;
mod watch;

pub use append::{
    append_assigns_rising_sequences, append_keeps_key_order, append_reports_heads,
    append_respects_its_limit, concurrent_appenders_assign_distinct_sequences,
};
pub use dedup::{
    dedup_accepts_gaps, dedup_keeps_keys_apart, dedup_refuses_a_late_copy,
    dedup_survives_a_rebuild, dedup_turns_a_retry_into_a_duplicate,
};
pub use durability::{appended_events_survive_a_rebuild, tail_read_sees_another_instance};
pub use harness::{Harness, Unreachable};
pub use lifecycle::{
    start_succeeds_with_the_medium_unreachable, stop_ends_the_workers,
    stop_is_harmless_unstarted_and_repeated,
};
pub use provision::{
    not_provisioned_everywhere, provision_compare_and_set,
    provision_reports_the_held_partition_count,
};
pub use read::{
    read_is_exclusive_and_bounded, read_wait_returns_on_append, read_wait_times_out_empty,
};
pub use resolve::resolve_positions;
pub use retention::{
    retention_ignores_occurred_at, retention_keeps_events_within_bounds,
    retention_removes_the_oldest_first,
};
pub use suite::{SCENARIOS, run_all};
pub use watch::{watch_starts_with_the_current_head, watch_yields_named_partitions_only};

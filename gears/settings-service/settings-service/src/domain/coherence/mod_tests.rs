//! The coherence rules without a broker: accumulation, the outcome, and the
//! consume-side eviction.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::{EventV1, GtsTypeId};
use serde_json::json;
use uuid::Uuid;

use super::{
    BroadcastMetrics, BroadcastResult, BroadcastSink, BroadcastState, ChangeOutcome,
    ConsumeOutcome, PublishError, RequestBroadcast, TouchedTenants, consume_cache_invalidation,
};
use crate::domain::activation::testing::{LogBuf, logged};
use crate::domain::resolution::cache::tests_entry;
use crate::domain::resolution::{EffectiveCache, scope_class};

const READY: BroadcastState = BroadcastState {
    types_registered: true,
    root_tenant_known: true,
};

#[derive(Default)]
struct Counts(Mutex<Vec<&'static str>>);

impl Counts {
    fn all(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

impl BroadcastMetrics for Counts {
    fn broadcast(&self, result: &'static str) {
        self.0.lock().unwrap().push(result);
    }
}

/// A broker double: answers from a script (accepting once it runs out) and
/// keeps what it was handed.
#[derive(Default)]
struct ScriptedSink {
    script: Mutex<VecDeque<Result<(), PublishError>>>,
    seen: Mutex<Vec<EventV1>>,
    /// The publish budget is spent.
    spent: Mutex<bool>,
}

impl ScriptedSink {
    fn with(script: Vec<Result<(), PublishError>>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            seen: Mutex::default(),
            spent: Mutex::default(),
        }
    }
    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

#[async_trait]
impl BroadcastSink for ScriptedSink {
    async fn publish(&self, event: EventV1) -> Result<(), PublishError> {
        self.seen.lock().unwrap().push(event);
        self.script.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }

    fn exhausted(&self) -> bool {
        *self.spent.lock().unwrap()
    }
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// An event of the shape a built broadcast has; the rules here read only its
/// `subject` and `data`.
fn one_event() -> EventV1 {
    EventV1::builder_for(
        GtsTypeId::new("gts.cf.core.events.event.v1~x.y.z.v1~"),
        json!({ "change_set_id": id(0xc5), "tenant": "t", "changed_keys": ["k"] }),
    )
    .tenant_id(id(1))
    .source("settings-service")
    .subject("t")
    .subject_type_id(GtsTypeId::new("gts.cf.core.settings.activation.v1~"))
    .build()
    .expect("a well-formed envelope")
}

// --- accumulation -----------------------------------------------------------

fn write(key: &str, tenant: u128) -> ChangeOutcome {
    ChangeOutcome::value_write(key, id(tenant), scope_class::LOCAL, true)
}

#[test]
fn tenants_are_kept_in_first_touch_order_and_keys_once() {
    let mut acc = TouchedTenants::new();
    acc.record_all([
        write("a", 2),
        write("b", 1),
        write("a", 2),
        write("c", 2),
        write("b", 1),
    ]);
    let resolved = acc.resolve(id(9));
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].tenant, id(2).to_string());
    assert_eq!(resolved[0].keys, ["a", "c"]);
    assert_eq!(resolved[1].tenant, id(1).to_string());
    assert_eq!(resolved[1].keys, ["b"]);
    assert_eq!(acc.touched_count(), 2);
}

#[test]
fn a_refused_change_accumulates_nothing_and_an_empty_request_is_empty() {
    let mut acc = TouchedTenants::new();
    acc.record(ChangeOutcome::value_write(
        "a",
        id(2),
        scope_class::LOCAL,
        false,
    ));
    assert!(acc.is_empty());
    assert!(acc.resolve(id(9)).is_empty());
}

#[test]
fn a_clone_accumulates_its_target_tenant() {
    // The caller passes the clone's target, never its source.
    let mut acc = TouchedTenants::new();
    acc.record(write("k", 7));
    assert_eq!(acc.resolve(id(9))[0].tenant, id(7).to_string());
}

#[test]
fn a_global_write_accumulates_the_root_tenant() {
    let mut acc = TouchedTenants::new();
    acc.record(ChangeOutcome::value_write(
        "g",
        id(2),
        scope_class::GLOBAL,
        true,
    ));
    let resolved = acc.resolve(id(9));
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].tenant, id(9).to_string());
}

#[test]
fn declaration_changes_accumulate_the_root_with_predecessor_and_successor() {
    let mut acc = TouchedTenants::new();
    acc.record_all([
        ChangeOutcome::declaration_change("k.v1"),
        ChangeOutcome::declaration_change("k.v2"),
    ]);
    // A value change at the root tenant merges with the root entry.
    acc.record(write("v", 9));
    let resolved = acc.resolve(id(9));
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].tenant, id(9).to_string());
    assert_eq!(resolved[0].keys, ["k.v1", "k.v2", "v"]);
}

#[test]
fn tenants_are_bare_lowercase_uuid_strings() {
    let mut acc = TouchedTenants::new();
    acc.record(write("k", 0x00AB_CDEF));
    let tenant = &acc.resolve(id(9))[0].tenant;
    assert_eq!(tenant, &tenant.to_lowercase());
    assert!(!tenant.contains(['{', '}']) && Uuid::parse_str(tenant).is_ok());
}

// --- the outcome ------------------------------------------------------------

#[tokio::test]
async fn an_accepted_publish_counts_sent() {
    let sink = ScriptedSink::default();
    let metrics = Counts::default();
    let mut request = RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil());
    assert_eq!(request.publish(one_event()).await, BroadcastResult::Sent);
    assert_eq!(metrics.all(), ["sent"]);
}

#[tokio::test]
async fn a_timeout_counts_failed_and_the_rest_skipped_with_one_attempt() {
    let sink = ScriptedSink::with(vec![Err(PublishError::TimedOut)]);
    let metrics = Counts::default();
    let mut request = RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil());
    let results = request
        .publish_all(vec![one_event(), one_event(), one_event()])
        .await;
    assert_eq!(
        results,
        [
            BroadcastResult::Failed,
            BroadcastResult::Skipped,
            BroadcastResult::Skipped
        ]
    );
    assert_eq!(metrics.all(), ["failed", "skipped", "skipped"]);
    assert_eq!(sink.calls(), 1, "the request pays one timeout");
    assert!(request.has_failed());
}

#[tokio::test]
async fn a_refusal_counts_failed_then_skipped() {
    let sink = ScriptedSink::with(vec![Err(PublishError::Refused("no produce grant".into()))]);
    let metrics = Counts::default();
    let mut request = RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil());
    let results = request.publish_all(vec![one_event(), one_event()]).await;
    assert_eq!(results, [BroadcastResult::Failed, BroadcastResult::Skipped]);
    assert_eq!(metrics.all(), ["failed", "skipped"]);
}

#[tokio::test]
async fn a_failure_in_one_request_does_not_skip_the_next_request() {
    let sink = ScriptedSink::with(vec![Err(PublishError::TimedOut)]);
    let metrics = Counts::default();
    let mut first = RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil());
    first.publish(one_event()).await;
    let mut second = RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil());
    assert_eq!(second.publish(one_event()).await, BroadcastResult::Sent);
}

#[tokio::test]
async fn a_publish_cut_by_the_budget_counts_skipped_not_failed() {
    use crate::domain::broker_sink::NotificationResult;
    let sink = ScriptedSink::with(vec![
        Err(PublishError::BudgetSpent),
        Err(PublishError::BudgetSpent),
    ]);
    let metrics = Counts::default();
    let mut request =
        RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil()).with_notifications(true);
    assert_eq!(request.publish(one_event()).await, BroadcastResult::Skipped);
    assert_eq!(metrics.all(), ["skipped"]);
    assert!(!request.has_failed(), "the broker did not fail");
    assert_eq!(
        request.publish_notification(one_event()).await,
        NotificationResult::Skipped
    );
    assert!(!request.has_failed());
}

#[tokio::test]
async fn a_spent_budget_skips_the_remaining_broadcasts_and_notifications_unattempted() {
    use crate::domain::broker_sink::NotificationResult;
    let sink = ScriptedSink::default();
    let metrics = Counts::default();
    let mut request =
        RequestBroadcast::new(Some(&sink), &metrics, READY, Uuid::nil()).with_notifications(true);
    assert_eq!(request.publish(one_event()).await, BroadcastResult::Sent);
    *sink.spent.lock().unwrap() = true;
    let results = request.publish_all(vec![one_event(), one_event()]).await;
    assert_eq!(results, [BroadcastResult::Skipped; 2]);
    assert_eq!(
        request.publish_notification(one_event()).await,
        NotificationResult::Skipped
    );
    assert_eq!(metrics.all(), ["sent", "skipped", "skipped"]);
    assert_eq!(sink.calls(), 1, "nothing attempted past the budget");
}

#[tokio::test]
async fn no_broker_counts_skipped_per_tenant() {
    let metrics = Counts::default();
    let mut request = RequestBroadcast::new(None, &metrics, READY, Uuid::nil());
    let results = request.publish_all(vec![one_event(), one_event()]).await;
    assert_eq!(results, [BroadcastResult::Skipped; 2]);
    assert_eq!(metrics.all(), ["skipped", "skipped"]);
}

#[tokio::test]
async fn unregistered_types_count_skipped_and_publish_nothing() {
    let sink = ScriptedSink::default();
    let metrics = Counts::default();
    let state = BroadcastState {
        types_registered: false,
        root_tenant_known: true,
    };
    let mut request = RequestBroadcast::new(Some(&sink), &metrics, state, Uuid::nil());
    assert_eq!(request.publish(one_event()).await, BroadcastResult::Skipped);
    assert_eq!(sink.calls(), 0);
    assert_eq!(metrics.all(), ["skipped"]);
}

#[tokio::test]
async fn an_unknown_root_tenant_counts_skipped_per_tenant_touched() {
    let sink = ScriptedSink::default();
    let metrics = Counts::default();
    let state = BroadcastState {
        types_registered: true,
        root_tenant_known: false,
    };
    let mut request = RequestBroadcast::new(Some(&sink), &metrics, state, Uuid::nil());
    assert_eq!(request.publish(one_event()).await, BroadcastResult::Skipped);
    assert_eq!(sink.calls(), 0);
    // Nothing is built when the root is unknown: the request counts per tenant.
    let mut acc = TouchedTenants::new();
    acc.record_all([write("a", 1), write("b", 2)]);
    request.skip(acc.touched_count());
    assert_eq!(metrics.all(), ["skipped", "skipped", "skipped"]);
}

// --- consume ----------------------------------------------------------------

fn cache_with(entries: &[(&str, u128)]) -> EffectiveCache {
    let cache = EffectiveCache::new(Duration::from_secs(60));
    for (key, tenant) in entries {
        cache.seed(Arc::new(tests_entry(key, id(*tenant))));
    }
    cache
}

fn delivery(keys: &[&str]) -> serde_json::Value {
    json!({ "change_set_id": id(0xc5), "tenant": id(2).to_string(), "changed_keys": keys })
}

#[test]
fn eviction_is_key_wide_for_every_scope_class_and_unknown_keys() {
    // Cascading: the named tenant and its descendant; global: a tenant caching
    // under its own scope; local: another tenant; an unknown key: no declaration.
    let cache = cache_with(&[
        ("casc", 2),
        ("casc", 3),
        ("glob", 4),
        ("loc", 2),
        ("loc", 5),
        ("ghost", 6),
        ("other", 2),
    ]);
    let out = consume_cache_invalidation(
        &cache,
        id(1),
        Some(&delivery(&["casc", "glob", "loc", "ghost", "never-cached"])),
    );
    assert_eq!(out, ConsumeOutcome::Evicted { keys: 5 });
    for (key, tenant) in [
        ("casc", 2),
        ("casc", 3),
        ("glob", 4),
        ("loc", 2),
        ("loc", 5),
        ("ghost", 6),
    ] {
        assert!(cache.get(key, id(tenant)).is_none(), "{key} at {tenant}");
    }
    assert!(cache.get("other", id(2)).is_some(), "an unnamed key stays");
}

#[test]
fn a_repeated_delivery_is_harmless() {
    let cache = cache_with(&[("k", 2), ("keep", 2)]);
    let data = delivery(&["k"]);
    assert_eq!(
        consume_cache_invalidation(&cache, id(1), Some(&data)),
        ConsumeOutcome::Evicted { keys: 1 }
    );
    assert_eq!(
        consume_cache_invalidation(&cache, id(1), Some(&data)),
        ConsumeOutcome::Evicted { keys: 1 }
    );
    assert!(cache.get("k", id(2)).is_none());
    assert!(cache.get("keep", id(2)).is_some());
}

#[test]
fn an_unknown_attribute_is_ignored_and_its_keys_are_evicted() {
    let cache = cache_with(&[("k", 2)]);
    let mut data = delivery(&["k"]);
    data["added_later"] = json!({ "x": 1 });
    assert_eq!(
        consume_cache_invalidation(&cache, id(1), Some(&data)),
        ConsumeOutcome::Evicted { keys: 1 }
    );
    assert!(cache.get("k", id(2)).is_none());
}

#[test]
fn a_malformed_delivery_evicts_nothing_and_warns() {
    let malformed = [
        None,
        Some(json!({ "changed_keys": ["k"] })),
        Some(json!({ "tenant": "not-a-uuid", "changed_keys": ["k"] })),
        Some(json!({ "tenant": id(2).to_string() })),
        Some(json!({ "tenant": id(2).to_string(), "changed_keys": "k" })),
        Some(json!({ "tenant": id(2).to_string(), "changed_keys": ["k", 3] })),
    ];
    for data in malformed {
        let cache = cache_with(&[("k", 2)]);
        let buf = LogBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .finish();
        let out = tracing::subscriber::with_default(subscriber, || {
            consume_cache_invalidation(&cache, id(0xe1), data.as_ref())
        });
        assert_eq!(out, ConsumeOutcome::Malformed, "{data:?}");
        assert!(cache.get("k", id(2)).is_some(), "nothing evicted: {data:?}");
        let log = logged(&buf);
        assert!(
            log.contains("WARN") && log.contains(&id(0xe1).to_string()),
            "{log}"
        );
    }
}

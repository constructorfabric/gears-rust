//! The broadcast builder: the envelope, the split, the size accounting.

use event_broker_sdk::Event;
use serde_json::json;
use settings_service_sdk::gts::{ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE};
use uuid::Uuid;

use super::{BROKER_EVENT_LIMIT_BYTES, HEADROOM, build_broadcasts, serialised_len};
use crate::domain::coherence::{BroadcastContext, TenantKeys};

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn ctx(root: Uuid) -> BroadcastContext {
    BroadcastContext {
        root_tenant: root,
        change_set_id: id(0xc5),
        trace_parent: None,
    }
}

fn touched(entries: &[(&str, &[&str])]) -> Vec<TenantKeys> {
    entries
        .iter()
        .map(|(t, keys)| TenantKeys {
            tenant: (*t).to_owned(),
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        })
        .collect()
}

fn one_event() -> Event {
    build_broadcasts(&touched(&[("t", &["k"])]), &ctx(id(1))).remove(0)
}

fn keys_of(event: &Event) -> Vec<String> {
    event.data.as_ref().unwrap()["changed_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_owned())
        .collect()
}

/// Keys that each serialise to 1 KiB exactly, quotes included.
fn kib_keys(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("{i:04}{}", "x".repeat(1018)))
        .collect()
}

#[test]
fn one_broadcast_per_tenant_under_the_root_with_the_full_key_set() {
    let root = id(1);
    let t1 = id(2).to_string();
    let t2 = id(3).to_string();
    let events = build_broadcasts(&touched(&[(&t1, &["a", "b"]), (&t2, &["c"])]), &ctx(root));
    assert_eq!(events.len(), 2);
    for (event, tenant, keys) in [
        (&events[0], &t1, vec!["a", "b"]),
        (&events[1], &t2, vec!["c"]),
    ] {
        assert_eq!(event.tenant_id, root);
        assert_eq!(event.subject, *tenant);
        assert_eq!(event.source, "settings-service");
        assert_eq!(event.type_id.as_ref() as &str, CACHE_INVALIDATE_EVENT_TYPE);
        assert_eq!(event.subject_type.as_ref() as &str, ACTIVATION_SUBJECT_TYPE);
        let data = event.data.as_ref().unwrap();
        assert_eq!(data["tenant"], json!(tenant));
        assert_eq!(data["change_set_id"], json!(id(0xc5)));
        assert_eq!(keys_of(event), keys);
    }
    assert_ne!(events[0].id, events[1].id);
}

#[test]
fn the_trace_context_rides_the_envelope_when_present() {
    let mut c = ctx(id(1));
    c.trace_parent = Some("00-abc-def-01".to_owned());
    let events = build_broadcasts(&touched(&[("t", &["k"])]), &c);
    assert_eq!(events[0].trace_parent.as_deref(), Some("00-abc-def-01"));
    assert!(one_event().trace_parent.is_none());
}

#[test]
fn the_payload_carries_identifiers_only() {
    let event = one_event();
    let data = event.data.unwrap();
    let mut members: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    members.sort_unstable();
    assert_eq!(members, ["change_set_id", "changed_keys", "tenant"]);
}

#[test]
fn a_key_set_over_the_limit_splits_into_parts_under_it() {
    let tenant = id(2).to_string();
    let keys = kib_keys(200);
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let events = build_broadcasts(&touched(&[(&tenant, &refs)]), &ctx(id(1)));
    // 200 KiB of keys over parts packed to under the limit less the headroom
    // (about 62 keys of 1 KiB beside the envelope): four parts.
    assert_eq!(events.len(), 4);
    let mut ids: Vec<Uuid> = events.iter().map(|e| e.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "each part has an id of its own");
    let mut rebuilt = Vec::new();
    for event in &events {
        assert!(serialised_len(event) < BROKER_EVENT_LIMIT_BYTES - HEADROOM);
        assert_eq!(event.subject, tenant);
        assert_eq!(
            event.data.as_ref().unwrap()["change_set_id"],
            json!(id(0xc5))
        );
        rebuilt.extend(keys_of(event));
    }
    assert_eq!(rebuilt, keys, "every key once, in order");
}

#[test]
fn a_set_just_under_the_budget_is_one_event() {
    let tenant = id(2).to_string();
    let keys = kib_keys(60);
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let events = build_broadcasts(&touched(&[(&tenant, &refs)]), &ctx(id(1)));
    assert_eq!(events.len(), 1);
    assert!(serialised_len(&events[0]) < BROKER_EVENT_LIMIT_BYTES - HEADROOM);
}

#[test]
fn a_set_between_the_budget_and_the_limit_splits_so_the_headroom_stays_free() {
    // Sixty-three keys of 1 KiB are about 63 KiB of keys plus the envelope:
    // under the 64 KiB limit but over the limit less the headroom, so the
    // builder splits rather than coming within a kilobyte of the broker's cut.
    let tenant = id(2).to_string();
    let keys = kib_keys(63);
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let events = build_broadcasts(&touched(&[(&tenant, &refs)]), &ctx(id(1)));
    assert_eq!(events.len(), 2);
    for event in &events {
        assert!(serialised_len(event) <= BROKER_EVENT_LIMIT_BYTES - HEADROOM);
    }
}

#[test]
fn a_single_key_over_the_limit_still_travels_alone() {
    let tenant = id(2).to_string();
    let huge = "k".repeat(BROKER_EVENT_LIMIT_BYTES);
    let events = build_broadcasts(
        &touched(&[(&tenant, &[huge.as_str(), "after"])]),
        &ctx(id(1)),
    );
    assert_eq!(events.len(), 2, "no key is dropped, none is split");
    assert_eq!(keys_of(&events[0]), [huge]);
    assert_eq!(keys_of(&events[1]), ["after"]);
}

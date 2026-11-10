// Updated: 2026-10-06 by Constructor Tech
use uuid::Uuid;

use credstore_sdk::{DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion};

use crate::config::StaticCredStorePluginConfig;

use super::Service;

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn svc() -> Service {
    Service::from_config(&StaticCredStorePluginConfig::default()).expect("config builds")
}

fn vv(s: &str) -> ValueVersion {
    ValueVersion::new(s)
}

#[track_caller]
fn assert_value(v: Option<SecretValue>, expected: &str) {
    assert_eq!(
        v.expect("value present").as_bytes(),
        expected.as_bytes(),
        "secret value mismatch"
    );
}

#[test]
fn starts_empty() {
    assert!(svc().get_value(&key(), &vv("1")).is_none());
}

#[test]
fn put_returns_increasing_versions_and_roundtrips() {
    let s = svc();
    let k = key();
    let v1 = s.put_value(&k, SecretValue::from("a"));
    let v2 = s.put_value(&k, SecretValue::from("b"));
    assert_eq!(v1, vv("1"));
    assert_eq!(v2, vv("2"));
    assert_value(s.get_value(&k, &v1), "a");
    assert_value(s.get_value(&k, &v2), "b");
}

#[test]
fn counters_are_per_key_and_keys_are_isolated() {
    let s = svc();
    let (k1, k2) = (key(), key());
    assert_eq!(s.put_value(&k1, SecretValue::from("x")), vv("1"));
    assert_eq!(s.put_value(&k2, SecretValue::from("y")), vv("1"));
    assert_value(s.get_value(&k1, &vv("1")), "x");
    assert_value(s.get_value(&k2, &vv("1")), "y");
    // Same record id under another tenant is a different key.
    let other = StoreKey::new(TenantId(Uuid::new_v4()), k1.record_id);
    assert!(s.get_value(&other, &vv("1")).is_none());
}

#[test]
fn get_of_unknown_or_garbage_version_is_none() {
    let s = svc();
    let k = key();
    s.put_value(&k, SecretValue::from("a"));
    assert!(s.get_value(&k, &vv("2")).is_none());
    assert!(s.get_value(&k, &vv("not-a-number")).is_none());
}

#[test]
fn destroy_below_removes_older_versions_only() {
    let s = svc();
    let k = key();
    for v in ["a", "b", "c"] {
        s.put_value(&k, SecretValue::from(v));
    }
    s.destroy_value(&k, &DestroySelector::Below(vv("3")));
    assert!(s.get_value(&k, &vv("1")).is_none());
    assert!(s.get_value(&k, &vv("2")).is_none());
    assert_value(s.get_value(&k, &vv("3")), "c");
}

#[test]
fn destroy_exactly_removes_one_version() {
    let s = svc();
    let k = key();
    for v in ["a", "b", "c"] {
        s.put_value(&k, SecretValue::from(v));
    }
    s.destroy_value(&k, &DestroySelector::Exactly(vv("2")));
    assert_value(s.get_value(&k, &vv("1")), "a");
    assert!(s.get_value(&k, &vv("2")).is_none());
    assert_value(s.get_value(&k, &vv("3")), "c");
}

#[test]
fn destroy_is_idempotent_and_never_reissues_numbers() {
    let s = svc();
    let k = key();
    s.put_value(&k, SecretValue::from("a"));
    s.destroy_value(&k, &DestroySelector::Exactly(vv("1")));
    s.destroy_value(&k, &DestroySelector::Exactly(vv("1")));
    s.destroy_value(&key(), &DestroySelector::Below(vv("9")));
    assert_eq!(s.put_value(&k, SecretValue::from("b")), vv("2"));
}

#[test]
fn delete_key_removes_all_versions_and_is_idempotent() {
    let s = svc();
    let k = key();
    s.put_value(&k, SecretValue::from("a"));
    s.put_value(&k, SecretValue::from("b"));
    s.delete_key_value(&k);
    assert!(s.get_value(&k, &vv("1")).is_none());
    assert!(s.get_value(&k, &vv("2")).is_none());
    s.delete_key_value(&k);
}

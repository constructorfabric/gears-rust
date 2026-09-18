use std::collections::BTreeMap;
use std::time::Duration;

use quota_enforcement_sdk::{
    Decision, DecisionResult, IdempotencyScope, IdempotencySubjectKey, OperationType, PayloadHash,
    TenantId,
};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{IdempotencyCache, ReplayRecord};

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).expect("valid timestamp")
}

fn scope(key: &str) -> IdempotencyScope {
    IdempotencyScope {
        tenant_id: TenantId::new(Uuid::from_u128(1)),
        subject_key: IdempotencySubjectKey::from_bytes([1; 32]),
        operation_type: OperationType::Debit,
        key: key.to_owned(),
    }
}

fn record(expires_at: OffsetDateTime) -> ReplayRecord {
    ReplayRecord {
        payload_hash: PayloadHash::from_bytes([2; 32]),
        decision: Decision {
            result: DecisionResult::Allowed,
            debit_plan: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
        },
        expires_at,
    }
}

#[test]
fn an_inserted_record_answers_a_later_lookup() {
    let cache = IdempotencyCache::new(4, Duration::from_secs(5));

    cache.insert(scope("k1"), record(at(1_000)), at(0));

    assert_eq!(cache.get(&scope("k1"), at(1)), Some(record(at(1_000))));
    assert_eq!(cache.get(&scope("other"), at(1)), None);
}

#[test]
fn an_entry_expires_when_its_lifetime_elapses() {
    let cache = IdempotencyCache::new(4, Duration::from_secs(5));
    cache.insert(scope("k1"), record(at(1_000)), at(0));

    assert!(cache.get(&scope("k1"), at(4)).is_some());
    assert_eq!(cache.get(&scope("k1"), at(5)), None);
    assert!(cache.is_empty(), "an expired entry is dropped, not kept");
}

#[test]
fn an_entry_never_outlives_the_record_behind_it() {
    let cache = IdempotencyCache::new(4, Duration::from_secs(60));

    // The record expires well before the cache lifetime would.
    cache.insert(scope("k1"), record(at(3)), at(0));

    assert!(cache.get(&scope("k1"), at(2)).is_some());
    assert_eq!(
        cache.get(&scope("k1"), at(3)),
        None,
        "past its retention the database treats the key as new, so the cache must too"
    );
}

#[test]
fn a_record_already_at_its_deadline_is_not_cached_at_all() {
    let cache = IdempotencyCache::new(4, Duration::from_secs(60));

    cache.insert(scope("k1"), record(at(0)), at(0));

    assert!(cache.is_empty());
}

#[test]
fn the_least_recently_used_entry_is_evicted_first() {
    let cache = IdempotencyCache::new(2, Duration::from_secs(60));
    cache.insert(scope("a"), record(at(1_000)), at(0));
    cache.insert(scope("b"), record(at(1_000)), at(0));

    // Touch `a`, then overflow: `b` is the least recently used.
    assert!(cache.get(&scope("a"), at(1)).is_some());
    cache.insert(scope("c"), record(at(1_000)), at(1));

    assert_eq!(cache.len(), 2);
    assert!(cache.get(&scope("a"), at(2)).is_some());
    assert!(cache.get(&scope("c"), at(2)).is_some());
    assert_eq!(cache.get(&scope("b"), at(2)), None);
}

#[test]
fn reinserting_a_key_replaces_it_without_growing_the_cache() {
    let cache = IdempotencyCache::new(1, Duration::from_secs(60));
    cache.insert(scope("k1"), record(at(1_000)), at(0));

    cache.insert(scope("k1"), record(at(2_000)), at(0));

    assert_eq!(cache.len(), 1);
    assert_eq!(cache.get(&scope("k1"), at(1)), Some(record(at(2_000))));
}

#[test]
fn a_versioned_blob_decodes_into_a_replay_record() {
    let stored = quota_enforcement_sdk::IdempotencyRecord {
        scope: scope("k1"),
        payload_hash: PayloadHash::from_bytes([2; 32]),
        decision_blob: serde_json::json!({
            "__version": 1,
            "result": {"outcome": "allowed"},
            "debit_plan": {},
            "diagnostics": {}
        }),
        engine_id: None,
        policy_id: None,
        policy_version: None,
        attribution_hash: None,
        created_at: at(0),
        expires_at: at(1_000),
    };

    let replay = ReplayRecord::try_from(stored).expect("the blob decodes");

    assert_eq!(replay.decision.result, DecisionResult::Allowed);
    assert_eq!(replay.expires_at, at(1_000));
}

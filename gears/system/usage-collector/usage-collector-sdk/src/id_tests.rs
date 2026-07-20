use time::OffsetDateTime;
use toolkit_gts::gts_id;
use uuid::Uuid;

use crate::id::{USAGE_RECORD_ID_NAMESPACE, created_at_micros, derive_usage_record_id};
use crate::models::{IdempotencyKey, UsageTypeGtsId};

fn tenant() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}
fn gts() -> UsageTypeGtsId {
    // Must be a valid derived GTS instance id: the segment after `~` is itself
    // a full vendor.package.namespace.type.vMAJOR[.MINOR] chain.
    UsageTypeGtsId::new(gts_id!(
        "cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1"
    ))
    .unwrap()
}
fn key(s: &str) -> IdempotencyKey {
    IdempotencyKey::new(s).unwrap()
}
fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

#[test]
fn derive_is_deterministic() {
    assert_eq!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
    );
}

#[test]
fn derive_matches_golden_vector() {
    // UUIDv5(NS, "11111111-1111-1111-1111-111111111111" 0x1F
    //            "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1" 0x1F
    //            "1700000000000000" 0x1F
    //            "idem-1")
    // Regenerated in Task 1 Step 4 — DO NOT hand-edit without rerunning.
    assert_eq!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        Uuid::parse_str("a019f808-e219-5bc0-b95f-dd5981b40d51").unwrap(),
    );
}

#[test]
fn derive_produces_a_v5_uuid() {
    assert_eq!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000))
            .get_version_num(),
        5,
    );
}

#[test]
fn distinct_keys_yield_distinct_ids() {
    assert_ne!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        derive_usage_record_id(tenant(), &gts(), &key("idem-2"), at(1_700_000_000)),
    );
}

#[test]
fn distinct_tenants_yield_distinct_ids() {
    let other = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    assert_ne!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        derive_usage_record_id(other, &gts(), &key("idem-1"), at(1_700_000_000)),
    );
}

#[test]
fn distinct_gts_ids_yield_distinct_ids() {
    // Same tenant + idempotency_key + created_at, different gts_id.
    let other = UsageTypeGtsId::new(gts_id!(
        "cf.core.uc.usage_record.v1~cf.mini_chat._.messages_sent.v1"
    ))
    .unwrap();
    assert_ne!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        derive_usage_record_id(tenant(), &other, &key("idem-1"), at(1_700_000_000)),
    );
}

#[test]
fn distinct_created_at_yields_distinct_ids() {
    // The core of ADR-0014: the same 3-tuple at two different `created_at`
    // values MUST derive two distinct ids (so the plugin's two persisted rows
    // no longer share one id).
    assert_ne!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000)),
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_001)),
    );
}

#[test]
fn sub_microsecond_created_at_truncates_to_same_id() {
    // Two `created_at` values that differ only below microsecond precision must
    // derive the SAME id — Postgres stores µs, so a distinct id here would be a
    // false `IdempotencyConflict` on an exact retry.
    let a = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_000_500).unwrap();
    let b = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_000_999).unwrap();
    let c = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_001_000).unwrap();
    assert_eq!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), a),
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), b),
        "sub-us difference must not change the id",
    );
    assert_ne!(
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), a),
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), c),
        "a full-us difference must change the id",
    );
}

#[test]
fn separator_in_key_does_not_alias() {
    // Regenerated in Task 1 Step 4 — DO NOT hand-edit without rerunning.
    let with_us = derive_usage_record_id(tenant(), &gts(), &key("idem\u{1f}1"), at(1_700_000_000));
    assert_eq!(
        with_us,
        Uuid::parse_str("0a1bbbeb-220b-58a1-a73f-5ec86ef74fb0").unwrap()
    );
    assert_ne!(
        with_us,
        derive_usage_record_id(tenant(), &gts(), &key("idem-1"), at(1_700_000_000))
    );
}

#[test]
fn namespace_is_pinned() {
    assert_eq!(
        USAGE_RECORD_ID_NAMESPACE,
        Uuid::parse_str("56313026-863b-4de8-b32b-1f96b67306ed").unwrap(),
    );
}

// ── created_at_micros: the shared µs-canonicalization primitive ───────────────

#[test]
fn created_at_micros_projects_whole_seconds_to_the_microsecond_count() {
    assert_eq!(created_at_micros(at(1_700_000_000)), 1_700_000_000_000_000);
}

#[test]
fn created_at_micros_floors_sub_microsecond_nanos() {
    // The floor is what makes an exact retry that differs only below µs derive
    // the same id — see `sub_microsecond_created_at_truncates_to_same_id`.
    let base = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_000_000).unwrap();
    let plus_500ns = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_000_500).unwrap();
    let plus_1us = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_000_001_000).unwrap();
    assert_eq!(
        created_at_micros(base),
        created_at_micros(plus_500ns),
        "sub-us nanos floor away",
    );
    assert_eq!(
        created_at_micros(plus_1us) - created_at_micros(base),
        1,
        "a full us advances the count by exactly one",
    );
}

#[test]
fn created_at_micros_is_exact_for_pre_epoch_instants() {
    // One microsecond before the unix epoch: 1969-12-31T23:59:59.999999Z.
    // `unix_timestamp() (-1) * 1_000_000 + microsecond() (999_999) = -1` — the
    // "no integer division" composition floors correctly for negatives, where a
    // naive `nanos / 1000` would not.
    let one_micro_pre_epoch = OffsetDateTime::from_unix_timestamp_nanos(-1_000).unwrap();
    assert_eq!(created_at_micros(one_micro_pre_epoch), -1);
}

#[test]
fn created_at_micros_is_offset_invariant() {
    // The client-reproducibility invariant: the same instant in a different
    // offset must project to the same µs count (and thus the same derived id),
    // because both `unix_timestamp()` and the sub-second `microsecond()` are
    // instant-based, not wall-clock-based.
    let utc = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let shifted = utc.to_offset(time::UtcOffset::from_hms(5, 30, 0).unwrap());
    assert_eq!(created_at_micros(utc), created_at_micros(shifted));
}

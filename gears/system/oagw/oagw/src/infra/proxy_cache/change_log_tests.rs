//! The change log's wire format and bounds (ADR-0020).

use proptest::prelude::*;
use uuid::Uuid;

use super::{
    ChangeLog, ChangeV1, ChangeV1Entry, Logged, MAX_BYTES, MAX_CHANGES, Unreadable, VERSION,
};
use crate::domain::services::management::MAX_ALIAS_LENGTH;
use crate::infra::proxy_cache::Change;

/// An upstream change in a fresh tenant, with an alias of `len` bytes.
fn upstream_change(len: usize) -> Change {
    Change::Upstream {
        tenant_id: Uuid::new_v4(),
        id: Uuid::new_v4(),
        alias: "a".repeat(len),
    }
}

/// One change of each kind.
fn every_kind() -> Vec<Change> {
    let [tenant_id, id, upstream_id] = std::array::from_fn(|_| Uuid::new_v4());
    vec![
        Change::Upstream {
            tenant_id,
            id,
            alias: "api.example.com".into(),
        },
        Change::UpstreamDeleted { tenant_id, id },
        Change::Route {
            tenant_id,
            id,
            upstream_id,
        },
        Change::RouteDeleted { tenant_id, id },
        Change::RoutesOfUpstream {
            tenant_id,
            upstream_id,
        },
    ]
}

fn seqs(log: &ChangeLog) -> Vec<u64> {
    log.changes().map(|logged| logged.seq).collect()
}

/// `log` as JSON, to edit.
fn to_json(log: &ChangeLog) -> serde_json::Value {
    serde_json::from_slice(&log.encode()).unwrap()
}

/// What `json` decodes to.
fn decode(json: &serde_json::Value) -> Result<ChangeLog, Unreadable> {
    ChangeLog::decode(&serde_json::to_vec(json).unwrap())
}

/// Every kind of change survives encoding, numbered in order with its
/// origin; `Change::All` is not a write and is not logged. `append` returns
/// the log as `encode` stores it.
///
/// ADR-0020: CACHE-LOG-003.
#[test]
fn changes_round_trip_numbered_in_order() {
    let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
    let changes = every_kind();
    let mut log = ChangeLog::new();
    let first = log.append(me, changes[..2].iter().chain([&Change::All]));
    assert_eq!(first, log.encode());
    let stored = log.append(other, &changes[2..]);
    assert_eq!(stored, log.encode());

    let decoded = ChangeLog::decode(&stored).expect("readable");
    assert_eq!(decoded, log);
    assert_eq!(decoded.id(), log.id());
    assert_eq!(decoded.seq(), 5);
    assert_eq!(decoded.oldest(), Some(1));
    let expected: Vec<Logged> = changes
        .into_iter()
        .enumerate()
        .map(|(i, change)| Logged {
            seq: i as u64 + 1,
            origin: if i < 2 { me } else { other },
            change,
        })
        .collect();
    assert_eq!(decoded.changes().collect::<Vec<_>>(), expected);
    assert_eq!(decoded.changes_after(3).collect::<Vec<_>>(), expected[3..]);
    assert_eq!(decoded.changes_after(5).count(), 0);
}

/// The stored form is version 1 JSON, field for field, with each of the
/// five kinds of change.
#[test]
fn the_wire_format_is_version_1() {
    let [log_id, origin, tenant, id, upstream] = std::array::from_fn(|_| Uuid::new_v4());
    let mut log = ChangeLog::new();
    log.id = log_id;
    let _ = log.append(
        origin,
        &[
            Change::Upstream {
                tenant_id: tenant,
                id,
                alias: "api.example.com".into(),
            },
            Change::UpstreamDeleted {
                tenant_id: tenant,
                id,
            },
            Change::Route {
                tenant_id: tenant,
                id,
                upstream_id: upstream,
            },
            Change::RouteDeleted {
                tenant_id: tenant,
                id,
            },
            Change::RoutesOfUpstream {
                tenant_id: tenant,
                upstream_id: upstream,
            },
        ],
    );
    assert_eq!(
        to_json(&log),
        serde_json::json!({
            "v": 1,
            "log": log_id,
            "seq": 5,
            "changes": [
                { "seq": 1, "origin": origin,
                  "c": { "upstream": { "tenant": tenant, "id": id, "alias": "api.example.com" } } },
                { "seq": 2, "origin": origin,
                  "c": { "upstream_deleted": { "tenant": tenant, "id": id } } },
                { "seq": 3, "origin": origin,
                  "c": { "route": { "tenant": tenant, "id": id, "upstream": upstream } } },
                { "seq": 4, "origin": origin,
                  "c": { "route_deleted": { "tenant": tenant, "id": id } } },
                { "seq": 5, "origin": origin,
                  "c": { "routes_of_upstream": { "tenant": tenant, "upstream": upstream } } },
            ],
        })
    );
}

/// The log keeps the newest 128 changes, also across appends; the
/// sequence numbers go on.
///
/// ADR-0020: CACHE-LOG-003.
#[test]
fn the_log_keeps_the_newest_128_changes() {
    let me = Uuid::new_v4();
    let mut log = ChangeLog::new();
    let full: Vec<Change> = (0..MAX_CHANGES).map(|_| upstream_change(8)).collect();
    let _ = log.append(me, &full);
    assert_eq!(seqs(&log), (1..=128).collect::<Vec<_>>());

    let _ = log.append(me, &[upstream_change(8)]);
    assert_eq!(seqs(&log), (2..=129).collect::<Vec<_>>());

    let burst: Vec<Change> = (0..200).map(|_| upstream_change(8)).collect();
    let _ = log.append(me, &burst);
    assert_eq!(log.seq(), 329);
    assert_eq!(seqs(&log), (202..=329).collect::<Vec<_>>());
    let kept: Vec<Change> = log.changes().map(|logged| logged.change).collect();
    assert_eq!(kept, burst[72..]);
}

/// The encoded log stays within 32 KiB: the oldest changes go, as few as
/// that takes; one change over the limit alone is still kept, and readers
/// then treat the log as unreadable and empty their caches. Aliases are
/// validated to 253 bytes before any write, so this takes a change no
/// write makes.
///
/// ADR-0020: CACHE-LOG-003.
#[test]
fn the_log_stays_within_32_kib() {
    let me = Uuid::new_v4();
    let mut log = ChangeLog::new();
    let changes: Vec<Change> = (0..100).map(|_| upstream_change(400)).collect();
    let stored = log.append(me, &changes);
    assert_eq!(stored, log.encode());
    assert!(stored.len() <= MAX_BYTES, "{} bytes", stored.len());
    let kept = log.changes().count();
    assert!(kept < 100, "the bytes bound applies first: {kept} kept");
    assert_eq!(log.seq(), 100);
    assert_eq!(log.oldest(), Some(100 - kept as u64 + 1));
    let newest: Vec<Change> = log.changes().map(|logged| logged.change).collect();
    assert_eq!(newest, changes[100 - kept..]);

    // As few as that takes: one more change would not fit.
    let mut one_more = log.clone();
    let next_oldest = ChangeV1Entry {
        seq: log.oldest().unwrap() - 1,
        origin: me,
        c: ChangeV1::from_change(&changes[99 - kept]).unwrap(),
    };
    one_more.changes.insert(0, next_oldest);
    assert!(one_more.encode().len() > MAX_BYTES);

    let mut huge = ChangeLog::new();
    let stored = huge.append(me, &[upstream_change(10), upstream_change(MAX_BYTES)]);
    assert_eq!(seqs(&huge), [2]);
    assert!(matches!(
        ChangeLog::decode(&stored),
        Err(Unreadable::Malformed(_))
    ));
}

/// A value of another version, or one that breaks version 1, is
/// unreadable; only a higher version is newer.
///
/// ADR-0020: CACHE-LOG-002.
#[test]
fn other_versions_and_broken_logs_are_unreadable() {
    let mut log = ChangeLog::new();
    let _ = log.append(Uuid::new_v4(), every_kind());
    let mut json = to_json(&log);

    json["v"] = 2.into();
    let newer = decode(&json);
    assert_eq!(newer, Err(Unreadable::Newer(2)));
    json["v"] = 0.into();
    let older = decode(&json);
    assert_eq!(older, Err(Unreadable::Version(0)));

    json["v"] = 1.into();
    json["changes"][1]["seq"] = 7.into();
    let gap = decode(&json);
    assert!(matches!(gap, Err(Unreadable::Malformed(_))), "{gap:?}");

    json["changes"][1]["seq"] = 2.into();
    json["seq"] = 9.into();
    let behind = decode(&json);
    assert!(
        matches!(behind, Err(Unreadable::Malformed(_))),
        "{behind:?}"
    );

    json["seq"] = 5.into();
    json["changes"][0]["c"] = serde_json::json!({ "upstream_renamed": {} });
    let unknown = decode(&json);
    assert!(
        matches!(unknown, Err(Unreadable::Malformed(_))),
        "{unknown:?}"
    );

    for garbage in [&b"not json"[..], b"{}", b"[1]", b""] {
        let result = ChangeLog::decode(garbage);
        assert!(
            matches!(result, Err(Unreadable::Malformed(_))),
            "{result:?}"
        );
    }
}

/// A version 1 value is held to the bounds its writers keep: at most
/// 32 KiB, 128 changes, and aliases of at most 253 bytes. A newer
/// version's value is reported as newer whatever its size, so it is never
/// taken for a malformed one and replaced.
///
/// ADR-0020: CACHE-LOG-002.
#[test]
fn decode_holds_a_version_1_log_to_its_bounds() {
    let mut log = ChangeLog::new();
    let full: Vec<Change> = (0..MAX_CHANGES).map(|_| upstream_change(1)).collect();
    let _ = log.append(Uuid::new_v4(), &full);
    let mut json = to_json(&log);
    assert!(decode(&json).is_ok(), "128 changes are a log");
    let mut extra = json["changes"][MAX_CHANGES - 1].clone();
    extra["seq"] = (MAX_CHANGES as u64 + 1).into();
    json["changes"].as_array_mut().unwrap().push(extra);
    json["seq"] = (MAX_CHANGES as u64 + 1).into();
    let too_many = decode(&json);
    assert!(
        matches!(too_many, Err(Unreadable::Malformed(_))),
        "{too_many:?}"
    );

    let mut log = ChangeLog::new();
    let _ = log.append(Uuid::new_v4(), &[upstream_change(MAX_ALIAS_LENGTH)]);
    let mut json = to_json(&log);
    assert!(decode(&json).is_ok(), "a 253-byte alias is an alias");
    json["changes"][0]["c"]["upstream"]["alias"] = "a".repeat(MAX_ALIAS_LENGTH + 1).into();
    let long = decode(&json);
    assert!(matches!(long, Err(Unreadable::Malformed(_))), "{long:?}");

    json["changes"][0]["c"]["upstream"]["alias"] = "a".into();
    json["padding"] = "x".repeat(MAX_BYTES).into();
    let big = decode(&json);
    assert!(matches!(big, Err(Unreadable::Malformed(_))), "{big:?}");
    json["v"] = 2.into();
    assert_eq!(decode(&json), Err(Unreadable::Newer(2)));
}

/// Sequence numbers never overflow: a log whose numbers wrap is malformed,
/// and a log at the last number starts over with a new ID.
///
/// ADR-0020: CACHE-LOG-002.
#[test]
fn sequence_numbers_never_overflow() {
    let mut log = ChangeLog::new();
    let _ = log.append(Uuid::new_v4(), &every_kind()[..2]);
    let mut json = to_json(&log);
    json["changes"][0]["seq"] = u64::MAX.into();
    json["changes"][1]["seq"] = 0.into();
    json["seq"] = 0.into();
    let wrapped = decode(&json);
    assert!(
        matches!(wrapped, Err(Unreadable::Malformed(_))),
        "{wrapped:?}"
    );

    let mut last = ChangeLog::new();
    last.seq = u64::MAX;
    let old_id = last.id();
    let next = upstream_change(1);
    let stored = last.append(Uuid::new_v4(), std::slice::from_ref(&next));
    assert_ne!(last.id(), old_id);
    assert_eq!(seqs(&last), [1]);
    assert_eq!(ChangeLog::decode(&stored), Ok(last));
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

fn any_uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

/// Any change a write makes, and `Change::All`.
fn any_change() -> impl Strategy<Value = Change> {
    let alias = proptest::string::string_regex("[a-z0-9.:_-]{1,253}").unwrap();
    prop_oneof![
        (any_uuid(), any_uuid(), alias).prop_map(|(tenant_id, id, alias)| Change::Upstream {
            tenant_id,
            id,
            alias
        }),
        (any_uuid(), any_uuid())
            .prop_map(|(tenant_id, id)| Change::UpstreamDeleted { tenant_id, id }),
        (any_uuid(), any_uuid(), any_uuid()).prop_map(|(tenant_id, id, upstream_id)| {
            Change::Route {
                tenant_id,
                id,
                upstream_id,
            }
        }),
        (any_uuid(), any_uuid()).prop_map(|(tenant_id, id)| Change::RouteDeleted { tenant_id, id }),
        (any_uuid(), any_uuid()).prop_map(|(tenant_id, upstream_id)| Change::RoutesOfUpstream {
            tenant_id,
            upstream_id
        }),
        Just(Change::All),
    ]
}

/// A value shaped like a log, with any version, sequence numbers and alias
/// lengths: what a broken or hostile writer could store. Weighted so the
/// sequence check often sees edge numbers (`u64::MAX` next to anything);
/// one in three has valid sequence numbers, so only the bounds decide.
fn any_log_value() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![2 => any_shaped_log_value(), 1 => any_consecutive_log_value()]
}

/// A version 1 log whose sequence numbers count up by one to its last,
/// so only the bounds decide whether it decodes. Its three shapes put each
/// bound at its edge: aliases near 253 bytes, 120 to 140 changes under
/// 32 KiB, and 80 to 128 changes of long aliases near 32 KiB.
fn any_consecutive_log_value() -> impl Strategy<Value = serde_json::Value> {
    let alias_len = |short: u32, long: u32, longest: usize| prop_oneof![short => 1..16_usize, long => 240..longest];
    (
        prop_oneof![0..1_000_000_u64, u64::MAX / 2..u64::MAX / 2 + 1_000],
        any_uuid(),
        prop_oneof![
            1 => proptest::collection::vec(alias_len(4, 1, 260), 0..8),
            2 => proptest::collection::vec(alias_len(60, 1, 254), 120..140),
            2 => proptest::collection::vec(alias_len(1, 4, 254), 80..128),
        ],
    )
        .prop_map(|(first, id, aliases)| {
            let changes: Vec<serde_json::Value> = aliases
                .iter()
                .zip(first..)
                .map(|(alias_len, seq)| {
                    serde_json::json!({
                        "seq": seq, "origin": id,
                        "c": { "upstream": { "tenant": id, "id": id, "alias": "a".repeat(*alias_len) } },
                    })
                })
                .collect();
            let seq = first + changes.len() as u64 - u64::from(!changes.is_empty());
            serde_json::json!({ "v": VERSION, "log": Uuid::nil(), "seq": seq, "changes": changes })
        })
}

/// [`any_log_value`]'s values whose sequence numbers are anything.
fn any_shaped_log_value() -> impl Strategy<Value = serde_json::Value> {
    let seq = prop_oneof![Just(u64::MAX), Just(0), Just(1), any::<u64>()];
    let alias_len = prop_oneof![9 => 1..16_usize, 1 => 250..260_usize];
    let entry = (seq, any_uuid(), any_uuid(), alias_len)
        .prop_map(|(seq, origin, id, alias_len)| {
            serde_json::json!({
                "seq": seq, "origin": origin,
                "c": { "upstream": { "tenant": id, "id": id, "alias": "a".repeat(alias_len) } },
            })
        })
        .boxed();
    (
        prop_oneof![3 => Just(VERSION), 1 => any::<u64>()],
        prop_oneof![Just(u64::MAX), any::<u64>()],
        prop_oneof![
            4 => proptest::collection::vec(entry.clone(), 0..8),
            1 => proptest::collection::vec(entry, 120..140),
        ],
    )
        .prop_map(|(v, seq, changes)| {
            serde_json::json!({ "v": v, "log": Uuid::nil(), "seq": seq, "changes": changes })
        })
}

proptest! {
    /// Any bytes decode to a log or an error, never a panic.
    ///
    /// ADR-0020: CACHE-LOG-002.
    #[test]
    fn decode_never_panics_on_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = ChangeLog::decode(&bytes);
    }

    /// Any log-shaped value decodes to a log or an error, never a panic; a
    /// log it decodes to keeps the bounds and encodes back to itself.
    ///
    /// ADR-0020: CACHE-LOG-002.
    #[test]
    fn decode_never_panics_on_any_log_shaped_value(value in any_log_value()) {
        let bytes = serde_json::to_vec(&value).unwrap();
        if let Ok(log) = ChangeLog::decode(&bytes) {
            prop_assert!(bytes.len() <= MAX_BYTES);
            prop_assert!(log.changes.len() <= MAX_CHANGES);
            let aliases_fit = log.changes().all(|logged| match logged.change {
                Change::Upstream { alias, .. } => alias.len() <= MAX_ALIAS_LENGTH,
                _ => true,
            });
            prop_assert!(aliases_fit, "an alias over {} bytes", MAX_ALIAS_LENGTH);
            prop_assert_eq!(ChangeLog::decode(&log.encode()), Ok(log));
        }
    }

    /// Whatever is appended, the stored log decodes to the log appended to,
    /// stays within its bounds, and holds the newest changes appended, in
    /// order, numbered up to the last.
    ///
    /// ADR-0020: CACHE-LOG-003.
    #[test]
    fn decode_of_an_appended_log_is_that_log(
        batches in proptest::collection::vec(proptest::collection::vec(any_change(), 0..40), 1..6),
    ) {
        let origin = Uuid::new_v4();
        let mut log = ChangeLog::new();
        let mut written = Vec::new();
        for batch in &batches {
            let stored = log.append(origin, batch);
            prop_assert_eq!(&stored, &log.encode());
            prop_assert!(stored.len() <= MAX_BYTES);
            prop_assert_eq!(ChangeLog::decode(&stored), Ok(log.clone()));
            written.extend(batch.iter().filter(|c| **c != Change::All).cloned());
        }
        prop_assert_eq!(log.seq(), written.len() as u64);
        let kept: Vec<Change> = log.changes().map(|logged| logged.change).collect();
        prop_assert!(kept.len() <= MAX_CHANGES);
        prop_assert_eq!(&kept[..], &written[written.len() - kept.len()..]);
    }
}

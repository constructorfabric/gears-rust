use super::*;

#[test]
fn round_up_to_does_not_overflow_on_max_input() {
    for (value, align, expected) in [
        (u64::MAX, DEFAULT_MIN_PART_SIZE, None),
        (u64::MAX, u64::MAX, Some(u64::MAX)),
        (1, u64::MAX, Some(u64::MAX)),
        (7, 5, Some(10)),
        (10, 5, Some(10)),
    ] {
        assert_eq!(round_up_to(value, align), expected, "{value} up to {align}");
    }
}

#[test]
fn replay_bind_state_maps_inputs_to_bind_state() {
    let (file_id, version_id) = (Uuid::from_u128(1), Uuid::from_u128(2));
    // (name, content pointer, bound_on_finalize, auto_bind, state, has etag, has current_etag)
    let cases = [
        (
            "auto-bind that never ran (empty pointer)",
            None,
            false,
            true,
            BindState::Manual,
            false,
            false,
        ),
        (
            "pointer at this version",
            Some(version_id),
            false,
            true,
            BindState::Bound,
            true,
            false,
        ),
        (
            "pointer at another version",
            Some(Uuid::from_u128(3)),
            false,
            true,
            BindState::Conflict,
            false,
            true,
        ),
        (
            "bound on finalize",
            None,
            true,
            true,
            BindState::Bound,
            true,
            false,
        ),
        (
            "no auto-bind",
            None,
            false,
            false,
            BindState::Manual,
            false,
            false,
        ),
    ];
    for (name, content_id, bound, auto_bind, state, has_etag, has_current) in cases {
        let (got, etag, current) =
            replay_bind_state(file_id, content_id, version_id, bound, auto_bind);
        assert_eq!(got, state, "{name}");
        assert_eq!(etag.is_some(), has_etag, "{name}: etag");
        assert_eq!(current.is_some(), has_current, "{name}: current_etag");
    }
}

#[test]
fn compute_plan_rejects_unplannable_sizes() {
    let cases = [
        ("overflowing preferred part size", u64::MAX, Some(u64::MAX)),
        ("absurd declared size", u64::MAX, None),
        (
            "beyond max_part_size * max_part_count",
            MAX_PART_SIZE * MAX_PART_COUNT + 1,
            None,
        ),
    ];
    for (name, declared_size, preferred_part_size) in cases {
        let err = compute_plan(declared_size, preferred_part_size, None).unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{name}: expected Validation, got {err:?}"
        );
    }
}

#[test]
fn compute_plan_stays_within_part_size_and_count_limits() {
    // 15,000 parts at DEFAULT_MIN_PART_SIZE would exceed MAX_PART_COUNT: widen, don't reject.
    let declared_size = 15_000 * DEFAULT_MIN_PART_SIZE;
    let (part_size, parts) = compute_plan(declared_size, Some(DEFAULT_MIN_PART_SIZE), None)
        .expect("must widen instead of rejecting");
    assert!(part_size > DEFAULT_MIN_PART_SIZE && part_size <= MAX_PART_SIZE);
    assert!((parts.len() as u64) <= MAX_PART_COUNT);
    let total: u64 = parts.iter().map(|(_, _, size)| *size).sum();
    assert_eq!(
        total, declared_size,
        "part sizes must add up to declared_size"
    );

    // The largest plannable size is accepted exactly at the boundary.
    let (part_size, parts) = compute_plan(MAX_PART_SIZE * MAX_PART_COUNT, None, None)
        .expect("boundary value must be accepted");
    assert_eq!(part_size, MAX_PART_SIZE);
    assert_eq!(parts.len() as u64, MAX_PART_COUNT);
}

/// Baseline snapshot that the negative tests below mutate one field away from.
fn stored_bound() -> StoredCompleteResult {
    StoredCompleteResult {
        version_id: Uuid::now_v7(),
        size: 11,
        content_hash: hex::encode([0u8; 32]),
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: 1,
        bind_state: BindState::Bound.as_str().to_owned(),
        etag: Some("\"etag-value\"".to_owned()),
        current_etag: None,
    }
}

/// A snapshot differing from [`stored_bound`] in the given fields.
fn stored(
    hash_mode: HashMode,
    part_count: i32,
    bind_state: BindState,
    etag: Option<&str>,
    current_etag: Option<&str>,
) -> StoredCompleteResult {
    StoredCompleteResult {
        hash_mode: hash_mode.as_str().to_owned(),
        part_count,
        bind_state: bind_state.as_str().to_owned(),
        etag: etag.map(str::to_owned),
        current_etag: current_etag.map(str::to_owned),
        ..stored_bound()
    }
}

const MANIFEST: Option<&str> = Some("v1,0:aa");

#[test]
fn into_completed_accepts_valid_snapshots_unchanged() {
    use HashMode::{MultipartCompositeSha256 as Composite, WholeSha256 as Whole};

    let cases = [
        ("whole-sha256 bound", stored_bound(), None),
        (
            "composite conflict",
            stored(
                Composite,
                3,
                BindState::Conflict,
                None,
                Some("\"current-etag\""),
            ),
            MANIFEST,
        ),
        (
            "manual requires neither etag nor current_etag",
            stored(Whole, 1, BindState::Manual, None, None),
            None,
        ),
    ];
    for (name, stored, manifest) in cases {
        let completed = stored
            .clone()
            .into_completed(manifest.map(str::to_owned))
            .unwrap_or_else(|| panic!("{name}: a valid snapshot must be accepted"));
        assert_eq!(completed.hash_mode.as_str(), stored.hash_mode, "{name}");
        assert_eq!(completed.part_count, stored.part_count, "{name}");
        assert_eq!(completed.bind_state.as_str(), stored.bind_state, "{name}");
        assert_eq!(completed.etag, stored.etag, "{name}");
        assert_eq!(completed.current_etag, stored.current_etag, "{name}");
        assert_eq!(completed.manifest.as_deref(), manifest, "{name}");
    }
}

#[test]
fn into_completed_rejects_inconsistent_snapshots() {
    use HashMode::{MultipartCompositeSha256 as Composite, WholeSha256 as Whole};

    let cases = [
        (
            "whole-sha256 with part_count != 1",
            StoredCompleteResult {
                part_count: 2,
                ..stored_bound()
            },
            None,
        ),
        ("whole-sha256 with a manifest", stored_bound(), MANIFEST),
        (
            "composite with zero part_count",
            stored(Composite, 0, BindState::Bound, Some("\"etag-value\""), None),
            MANIFEST,
        ),
        (
            "bound without an etag",
            StoredCompleteResult {
                etag: None,
                ..stored_bound()
            },
            None,
        ),
        (
            "conflict without a current_etag",
            stored(Composite, 2, BindState::Conflict, None, None),
            MANIFEST,
        ),
        (
            "bound with a current_etag",
            StoredCompleteResult {
                current_etag: Some("\"other\"".to_owned()),
                ..stored_bound()
            },
            None,
        ),
        (
            "conflict with an etag",
            stored(
                Composite,
                2,
                BindState::Conflict,
                Some("\"new\""),
                Some("\"cur\""),
            ),
            MANIFEST,
        ),
        (
            "unrecognized bind_state spelling",
            StoredCompleteResult {
                bind_state: "not-a-real-state".to_owned(),
                ..stored_bound()
            },
            None,
        ),
        (
            "manual with an etag",
            stored(Whole, 1, BindState::Manual, Some("\"new\""), None),
            None,
        ),
        (
            "manual with a current_etag",
            stored(Whole, 1, BindState::Manual, None, Some("\"current\"")),
            None,
        ),
        (
            "content_hash not a 32-byte digest",
            StoredCompleteResult {
                content_hash: hex::encode([0u8; 1]),
                ..stored_bound()
            },
            None,
        ),
        // One-part plans finalize as `whole-sha256`, so a one-part composite is never written.
        (
            "one-part composite with a manifest",
            stored(Composite, 1, BindState::Manual, None, None),
            MANIFEST,
        ),
        (
            "one-part composite without a manifest",
            stored(Composite, 1, BindState::Manual, None, None),
            None,
        ),
        (
            "multi-part composite without a manifest",
            stored(Composite, 2, BindState::Manual, None, None),
            None,
        ),
    ];
    for (name, stored, manifest) in cases {
        assert!(
            stored.into_completed(manifest.map(str::to_owned)).is_none(),
            "{name}: must fall back, not be accepted as-is"
        );
    }
}

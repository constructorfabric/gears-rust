//! Unit tests for the manifest-budget helpers behind `list_versions_with_manifests`; the
//! end-to-end pagination behaviour (both directions) is in `tests/cursor_pagination_test.rs`.

use std::collections::HashMap;

use time::OffsetDateTime;
use uuid::Uuid;

use file_storage_sdk::{FileVersion, VersionStatus};

use super::manifest_budget_cutoff;

fn version(hash_mode: &str) -> FileVersion {
    FileVersion {
        file_id: Uuid::nil(),
        version_id: Uuid::now_v7(),
        mime_type: "application/octet-stream".to_owned(),
        size: 1,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0; 32],
        hash_mode: hash_mode.to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: "/p".to_owned(),
        created_at: OffsetDateTime::now_utc(),
        bound_on_finalize: false,
    }
}

/// `n` versions with distinct ids (a `vec![v; n]` would clone one id).
fn versions(n: usize, hash_mode: &str) -> Vec<FileVersion> {
    (0..n).map(|_| version(hash_mode)).collect()
}

fn manifests_of(versions: &[FileVersion], sizes: &[Option<usize>]) -> HashMap<Uuid, String> {
    versions
        .iter()
        .zip(sizes)
        .filter_map(|(v, size)| size.map(|n| (v.version_id, "m".repeat(n))))
        .collect()
}

#[test]
fn manifest_budget_cutoff_counts_manifest_bytes_and_always_keeps_the_first_version() {
    const COMPOSITE: &str = "multipart-composite-sha256";
    // (row, hash mode, manifest size per version (`None`: no manifest), budget, kept versions)
    type Row = (
        &'static str,
        &'static str,
        &'static [Option<usize>],
        u64,
        usize,
    );
    let rows: [Row; 4] = [
        (
            "within budget keeps everything",
            COMPOSITE,
            &[Some(10), Some(10), Some(10)],
            30,
            3,
        ),
        (
            "stops before the version that crosses the budget",
            COMPOSITE,
            &[Some(10), Some(10), Some(10)],
            25,
            2,
        ),
        (
            // The version adjacent to the cursor must survive so a client can make progress.
            "the first version is kept even over budget",
            COMPOSITE,
            &[Some(100), Some(1)],
            10,
            1,
        ),
        (
            "versions without a manifest are free",
            "whole-sha256",
            &[None, None, None, None],
            0,
            4,
        ),
    ];
    for (row, hash_mode, sizes, budget, kept) in rows {
        let versions = versions(sizes.len(), hash_mode);
        let manifests = manifests_of(&versions, sizes);
        assert_eq!(
            manifest_budget_cutoff(&versions, &manifests, budget),
            kept,
            "{row}"
        );
    }
}

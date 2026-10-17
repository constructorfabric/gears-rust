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
fn cutoff_keeps_everything_within_budget() {
    let versions = versions(3, "multipart-composite-sha256");
    let manifests = manifests_of(&versions, &[Some(10), Some(10), Some(10)]);
    assert_eq!(manifest_budget_cutoff(&versions, &manifests, 30), 3);
}

#[test]
fn cutoff_stops_before_the_version_that_crosses_the_budget() {
    let versions = versions(3, "multipart-composite-sha256");
    let manifests = manifests_of(&versions, &[Some(10), Some(10), Some(10)]);
    assert_eq!(manifest_budget_cutoff(&versions, &manifests, 25), 2);
}

#[test]
fn cutoff_always_keeps_the_first_version_even_over_budget() {
    let versions = versions(2, "multipart-composite-sha256");
    let manifests = manifests_of(&versions, &[Some(100), Some(1)]);
    assert_eq!(
        manifest_budget_cutoff(&versions, &manifests, 10),
        1,
        "the version adjacent to the cursor must survive so a client can make progress"
    );
}

#[test]
fn cutoff_charges_nothing_for_versions_without_a_manifest() {
    let versions = versions(4, "whole-sha256");
    let manifests = manifests_of(&versions, &[None, None, None, None]);
    assert_eq!(manifest_budget_cutoff(&versions, &manifests, 0), 4);
}

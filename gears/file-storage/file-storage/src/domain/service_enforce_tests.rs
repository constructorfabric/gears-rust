//! Unit tests for the P2-M2 write-path enforcement helpers in `FileService`.
//!
//! These tests exercise the pure logic helpers (`mime_allowed`,
//! `check_allowed_mime`, `compute_effective_max_bytes`,
//! `check_metadata_limits`) without requiring a database or HTTP stack.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::policy::{EffectivePolicy, MetadataLimits, MimeSizeOverride};
use crate::domain::service::FileService;
use crate::infra::backend::{BackendCapabilities, InMemoryBackend, StorageBackend};

// ── helpers ────────────────────────────────────────────────────────────────────

fn open_policy() -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: None,
        max_bytes: None,
        per_mime_max_bytes: vec![],
        metadata_limits: MetadataLimits::default(),
    }
}

fn policy_with_mimes(mimes: &[&str]) -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: Some(mimes.iter().map(ToString::to_string).collect()),
        max_bytes: None,
        per_mime_max_bytes: vec![],
        metadata_limits: MetadataLimits::default(),
    }
}

fn policy_with_global_max(max_bytes: u64) -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: None,
        max_bytes: Some(max_bytes),
        per_mime_max_bytes: vec![],
        metadata_limits: MetadataLimits::default(),
    }
}

fn policy_with_per_mime(overrides: &[(&str, u64)]) -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: None,
        max_bytes: None,
        per_mime_max_bytes: overrides
            .iter()
            .map(|(m, b)| MimeSizeOverride {
                mime: m.to_string(),
                max_bytes: *b,
            })
            .collect(),
        metadata_limits: MetadataLimits::default(),
    }
}

fn policy_with_meta_limits(
    max_pairs: Option<u32>,
    max_key_len: Option<u32>,
    max_value_len: Option<u32>,
    max_total_bytes: Option<u32>,
) -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: None,
        max_bytes: None,
        per_mime_max_bytes: vec![],
        metadata_limits: MetadataLimits {
            max_pairs,
            max_key_len,
            max_value_len,
            max_total_bytes,
        },
    }
}

fn in_memory_backend() -> Arc<dyn StorageBackend> {
    Arc::new(InMemoryBackend::new("test"))
}

fn backend_with_max(max_size_bytes: u64) -> Arc<dyn StorageBackend> {
    // We can't easily create an InMemoryBackend with a custom max_size_bytes
    // since BackendCapabilities is returned from capabilities(). Use a wrapper.
    struct LimitedBackend(InMemoryBackend, u64);

    #[async_trait::async_trait]
    impl StorageBackend for LimitedBackend {
        fn id(&self) -> &str {
            self.0.id()
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                max_size_bytes: Some(self.1),
                ..BackendCapabilities::default()
            }
        }

        async fn put(&self, path: &str, bytes: bytes::Bytes) -> Result<(), DomainError> {
            self.0.put(path, bytes).await
        }

        async fn get(&self, path: &str) -> Result<bytes::Bytes, DomainError> {
            self.0.get(path).await
        }

        async fn delete(&self, path: &str) -> Result<(), DomainError> {
            self.0.delete(path).await
        }

        async fn exists(&self, path: &str) -> Result<bool, DomainError> {
            self.0.exists(path).await
        }
    }

    Arc::new(LimitedBackend(
        InMemoryBackend::new("limited"),
        max_size_bytes,
    ))
}

// ── mime_allowed ───────────────────────────────────────────────────────────────

#[test]
fn mime_allowed_exact_match() {
    assert!(FileService::mime_allowed(
        "image/jpeg",
        &["image/jpeg".to_owned()]
    ));
}

#[test]
fn mime_allowed_wildcard_subtype() {
    assert!(FileService::mime_allowed(
        "image/jpeg",
        &["image/*".to_owned()]
    ));
    assert!(FileService::mime_allowed(
        "image/png",
        &["image/*".to_owned()]
    ));
}

#[test]
fn mime_allowed_wildcard_does_not_cross_type() {
    assert!(!FileService::mime_allowed(
        "video/mp4",
        &["image/*".to_owned()]
    ));
}

#[test]
fn mime_allowed_empty_list_returns_false() {
    assert!(!FileService::mime_allowed("image/jpeg", &[]));
}

#[test]
fn mime_allowed_multiple_patterns_any_match() {
    assert!(FileService::mime_allowed(
        "text/plain",
        &["image/jpeg".to_owned(), "text/plain".to_owned()]
    ));
}

// ── check_allowed_mime ─────────────────────────────────────────────────────────

#[test]
fn check_allowed_mime_no_restriction_permits_all() {
    let policy = open_policy();
    assert!(FileService::check_allowed_mime(&policy, "image/jpeg").is_ok());
    assert!(FileService::check_allowed_mime(&policy, "application/octet-stream").is_ok());
}

#[test]
fn check_allowed_mime_permits_matching_type() {
    let policy = policy_with_mimes(&["image/*", "text/plain"]);
    assert!(FileService::check_allowed_mime(&policy, "image/jpeg").is_ok());
    assert!(FileService::check_allowed_mime(&policy, "text/plain").is_ok());
}

#[test]
fn check_allowed_mime_rejects_non_matching_type() {
    let policy = policy_with_mimes(&["image/jpeg"]);
    let err = FileService::check_allowed_mime(&policy, "video/mp4").unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { mime_type } if mime_type == "video/mp4")
    );
}

#[test]
fn check_allowed_mime_empty_list_rejects_all() {
    let policy = policy_with_mimes(&[]);
    let err = FileService::check_allowed_mime(&policy, "image/jpeg").unwrap_err();
    assert!(matches!(err, DomainError::PolicyMimeNotAllowed { .. }));
}

// ── compute_effective_max_bytes ────────────────────────────────────────────────

#[test]
fn compute_effective_max_bytes_no_restrictions_returns_none() {
    let policy = open_policy();
    let backend = in_memory_backend();
    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, None);
}

#[test]
fn compute_effective_max_bytes_policy_global_wins() {
    let policy = policy_with_global_max(1_000_000);
    let backend = in_memory_backend();
    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, Some(1_000_000));
}

#[test]
fn compute_effective_max_bytes_backend_ceiling_wins_over_policy() {
    let mut policy = policy_with_global_max(10_000_000);
    // backend max is more restrictive
    let backend = backend_with_max(5_000_000);
    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, Some(5_000_000));

    // policy max is more restrictive than backend
    policy.max_bytes = Some(2_000_000);
    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, Some(2_000_000));
}

#[test]
fn compute_effective_max_bytes_per_mime_override() {
    let policy = policy_with_per_mime(&[("image/*", 500_000), ("video/*", 50_000_000)]);
    let backend = in_memory_backend();

    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, Some(500_000));

    let result = FileService::compute_effective_max_bytes(&policy, "video/mp4", &backend);
    assert_eq!(result, Some(50_000_000));

    let result = FileService::compute_effective_max_bytes(&policy, "text/plain", &backend);
    assert_eq!(result, None); // no per-mime match, no global
}

#[test]
fn compute_effective_max_bytes_per_mime_takes_min_with_global() {
    let mut policy = policy_with_per_mime(&[("image/*", 500_000)]);
    policy.max_bytes = Some(200_000); // global is more restrictive than per-mime
    let backend = in_memory_backend();

    let result = FileService::compute_effective_max_bytes(&policy, "image/jpeg", &backend);
    assert_eq!(result, Some(200_000));
}

// ── check_metadata_limits ──────────────────────────────────────────────────────

#[test]
fn check_metadata_limits_no_limits_permits_any() {
    let policy = open_policy();
    let entries = vec![
        ("k1".to_owned(), "v1".to_owned()),
        ("k2".to_owned(), "v2".to_owned()),
    ];
    assert!(FileService::check_metadata_limits(&policy, &entries).is_ok());
}

#[test]
fn check_metadata_limits_max_pairs_violated() {
    let policy = policy_with_meta_limits(Some(1), None, None, None);
    let entries = vec![
        ("k1".to_owned(), "v1".to_owned()),
        ("k2".to_owned(), "v2".to_owned()),
    ];
    let err = FileService::check_metadata_limits(&policy, &entries).unwrap_err();
    assert!(matches!(err, DomainError::PolicyMetadataExceeded { .. }));
}

#[test]
fn check_metadata_limits_max_pairs_ok() {
    let policy = policy_with_meta_limits(Some(2), None, None, None);
    let entries = vec![
        ("k1".to_owned(), "v1".to_owned()),
        ("k2".to_owned(), "v2".to_owned()),
    ];
    assert!(FileService::check_metadata_limits(&policy, &entries).is_ok());
}

#[test]
fn check_metadata_limits_key_len_violated() {
    let policy = policy_with_meta_limits(None, Some(3), None, None);
    let entries = vec![("toolong_key".to_owned(), "v".to_owned())];
    let err = FileService::check_metadata_limits(&policy, &entries).unwrap_err();
    assert!(matches!(err, DomainError::PolicyMetadataExceeded { .. }));
}

#[test]
fn check_metadata_limits_value_len_violated() {
    let policy = policy_with_meta_limits(None, None, Some(5), None);
    let entries = vec![("k".to_owned(), "too_long_value".to_owned())];
    let err = FileService::check_metadata_limits(&policy, &entries).unwrap_err();
    assert!(matches!(err, DomainError::PolicyMetadataExceeded { .. }));
}

#[test]
fn check_metadata_limits_total_bytes_violated() {
    let policy = policy_with_meta_limits(None, None, None, Some(10));
    let entries = vec![
        ("key1".to_owned(), "value1".to_owned()), // 4+6=10
        ("k2".to_owned(), "v2".to_owned()),       // 2+2=4 → total=14
    ];
    let err = FileService::check_metadata_limits(&policy, &entries).unwrap_err();
    assert!(matches!(err, DomainError::PolicyMetadataExceeded { .. }));
}

#[test]
fn check_metadata_limits_total_bytes_ok() {
    let policy = policy_with_meta_limits(None, None, None, Some(20));
    let entries = vec![("key1".to_owned(), "value1".to_owned())]; // 4+6=10
    assert!(FileService::check_metadata_limits(&policy, &entries).is_ok());
}

#[test]
fn check_metadata_limits_empty_entries_always_ok() {
    let policy = policy_with_meta_limits(Some(0), Some(0), Some(0), Some(0));
    assert!(FileService::check_metadata_limits(&policy, &[]).is_ok());
}

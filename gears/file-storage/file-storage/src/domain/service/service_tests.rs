use crate::domain::error::DomainError;
use crate::domain::policy::{EffectivePolicy, MetadataLimits, MimeSizeOverride, PolicyResolver};
use crate::domain::service::content_verb;
use crate::infra::signed_url::Op;

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

fn pat(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(ToString::to_string).collect()
}

#[test]
fn mime_allowed_matches_exact_and_wildcard_subtype_patterns() {
    // (row, mime, patterns, allowed)
    type Row = (&'static str, &'static str, &'static [&'static str], bool);
    let rows: [Row; 7] = [
        ("exact", "image/jpeg", &["image/jpeg"], true),
        ("wildcard subtype", "image/jpeg", &["image/*"], true),
        ("wildcard other subtype", "image/png", &["image/*"], true),
        (
            "wildcard does not cross type",
            "video/mp4",
            &["image/*"],
            false,
        ),
        ("empty list", "image/jpeg", &[], false),
        (
            "any of several patterns",
            "text/plain",
            &["image/jpeg", "text/plain"],
            true,
        ),
        (
            "no pattern matches",
            "text/plain",
            &["image/jpeg", "image/*"],
            false,
        ),
    ];
    for (row, mime, patterns, allowed) in rows {
        assert_eq!(
            PolicyResolver::mime_allowed(mime, &pat(patterns)),
            allowed,
            "{row}"
        );
    }
}

#[test]
fn check_allowed_mime_applies_the_allow_list() {
    // (row, policy, mime, `None`: permitted, `Some(m)`: rejected as `PolicyMimeNotAllowed { m }`)
    let rows = [
        ("no restriction", open_policy(), "image/jpeg", None),
        (
            "no restriction, any type",
            open_policy(),
            "application/octet-stream",
            None,
        ),
        (
            "wildcard match",
            policy_with_mimes(&["image/*", "text/plain"]),
            "image/jpeg",
            None,
        ),
        (
            "exact match",
            policy_with_mimes(&["image/*", "text/plain"]),
            "text/plain",
            None,
        ),
        (
            "non-matching type",
            policy_with_mimes(&["image/jpeg"]),
            "video/mp4",
            Some("video/mp4"),
        ),
        (
            "empty list rejects all",
            policy_with_mimes(&[]),
            "image/jpeg",
            Some("image/jpeg"),
        ),
    ];
    for (row, policy, mime, rejected) in rows {
        match (PolicyResolver::check_allowed_mime(&policy, mime), rejected) {
            (Ok(()), None) => {}
            (Err(DomainError::PolicyMimeNotAllowed { mime_type }), Some(expected)) => {
                assert_eq!(mime_type, expected, "{row}");
            }
            (other, _) => panic!("{row}: unexpected outcome {other:?}"),
        }
    }
}

#[test]
fn compute_effective_max_bytes_takes_the_tightest_limit() {
    let mut global_below_ceiling = policy_with_global_max(10_000_000);
    global_below_ceiling.max_bytes = Some(2_000_000);
    let mut per_mime_above_global = policy_with_per_mime(&[("image/*", 500_000)]);
    per_mime_above_global.max_bytes = Some(200_000);
    let per_mime = policy_with_per_mime(&[("image/*", 500_000), ("video/*", 50_000_000)]);

    // (row, policy, mime, backend ceiling, expected)
    let rows = [
        ("no restrictions", open_policy(), "image/jpeg", None, None),
        (
            "policy global",
            policy_with_global_max(1_000_000),
            "image/jpeg",
            None,
            Some(1_000_000),
        ),
        (
            "backend ceiling below the policy",
            policy_with_global_max(10_000_000),
            "image/jpeg",
            Some(5_000_000),
            Some(5_000_000),
        ),
        (
            "policy below the backend ceiling",
            global_below_ceiling,
            "image/jpeg",
            Some(5_000_000),
            Some(2_000_000),
        ),
        (
            "per-mime image",
            per_mime.clone(),
            "image/jpeg",
            None,
            Some(500_000),
        ),
        (
            "per-mime video",
            per_mime.clone(),
            "video/mp4",
            None,
            Some(50_000_000),
        ),
        (
            "no per-mime match, no global",
            per_mime,
            "text/plain",
            None,
            None,
        ),
        (
            "global below per-mime",
            per_mime_above_global,
            "image/jpeg",
            None,
            Some(200_000),
        ),
    ];
    for (row, policy, mime, ceiling, expected) in rows {
        assert_eq!(
            PolicyResolver::compute_effective_max_bytes(&policy, mime, ceiling),
            expected,
            "{row}"
        );
    }
}

#[test]
fn check_metadata_limits_enforces_each_limit() {
    let pairs = |kv: &[(&str, &str)]| -> Vec<(String, String)> {
        kv.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let two = [("k1", "v1"), ("k2", "v2")];
    // (row, policy, entries, permitted)
    let rows = [
        ("no limits", open_policy(), pairs(&two), true),
        (
            "max_pairs violated",
            policy_with_meta_limits(Some(1), None, None, None),
            pairs(&two),
            false,
        ),
        (
            "max_pairs ok",
            policy_with_meta_limits(Some(2), None, None, None),
            pairs(&two),
            true,
        ),
        (
            "key_len violated",
            policy_with_meta_limits(None, Some(3), None, None),
            pairs(&[("toolong_key", "v")]),
            false,
        ),
        (
            "value_len violated",
            policy_with_meta_limits(None, None, Some(5), None),
            pairs(&[("k", "too_long_value")]),
            false,
        ),
        (
            // 4+6 + 2+2 = 14 > 10
            "total_bytes violated",
            policy_with_meta_limits(None, None, None, Some(10)),
            pairs(&[("key1", "value1"), ("k2", "v2")]),
            false,
        ),
        (
            // 4+6 = 10 <= 20
            "total_bytes ok",
            policy_with_meta_limits(None, None, None, Some(20)),
            pairs(&[("key1", "value1")]),
            true,
        ),
        (
            "empty entries always ok",
            policy_with_meta_limits(Some(0), Some(0), Some(0), Some(0)),
            vec![],
            true,
        ),
    ];
    for (row, policy, entries, permitted) in rows {
        match PolicyResolver::check_metadata_limits(&policy, &entries) {
            Ok(()) => assert!(permitted, "{row}: must be rejected"),
            Err(DomainError::PolicyMetadataExceeded { .. }) => {
                assert!(!permitted, "{row}: must be permitted");
            }
            Err(other) => panic!("{row}: unexpected error {other:?}"),
        }
    }
}

#[test]
fn sign_url_rejects_or_correctly_maps_multipart_part() {
    // `Op::MultipartPart` has no two-segment mapping; `sign_url` must reject it
    // rather than mint a URL that would 404 on the sidecar.
    let err = content_verb(Op::MultipartPart).unwrap_err();
    assert!(matches!(err, DomainError::InternalError), "got {err:?}");

    assert_eq!(content_verb(Op::Get).unwrap(), "download");
    assert_eq!(content_verb(Op::Put).unwrap(), "upload");
}

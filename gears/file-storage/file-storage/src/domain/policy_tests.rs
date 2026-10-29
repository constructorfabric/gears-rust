use super::*;
use crate::domain::error::DomainError;

fn body_with_mimes(mimes: &[&str]) -> PolicyBody {
    PolicyBody {
        allowed_mime_types: mimes.iter().map(ToString::to_string).collect(),
        ..PolicyBody::default()
    }
}

fn body_with_size(max_bytes: u64) -> PolicyBody {
    PolicyBody {
        size_limits: SizeLimits {
            max_bytes: Some(max_bytes),
            ..Default::default()
        },
        ..PolicyBody::default()
    }
}

fn body_with_meta_limits(max_pairs: u32, max_key_len: u32, max_value_len: u32) -> PolicyBody {
    PolicyBody {
        metadata_limits: MetadataLimits {
            max_pairs: Some(max_pairs),
            max_key_len: Some(max_key_len),
            max_value_len: Some(max_value_len),
            max_total_bytes: None,
        },
        ..PolicyBody::default()
    }
}

fn limit_for(ep: &EffectivePolicy, mime: &str) -> u64 {
    ep.per_mime_max_bytes
        .iter()
        .find(|e| e.mime == mime)
        .unwrap_or_else(|| panic!("no per-mime entry for {mime}"))
        .max_bytes
}

fn override_for(mime: &str, max_bytes: u64) -> MimeSizeOverride {
    MimeSizeOverride {
        mime: mime.to_owned(),
        max_bytes,
    }
}

fn open_policy() -> EffectivePolicy {
    EffectivePolicy {
        allowed_mime_types: None,
        max_bytes: None,
        per_mime_max_bytes: vec![],
        metadata_limits: MetadataLimits::default(),
    }
}

fn mimes(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(ToString::to_string).collect()
}

#[test]
fn resolve_allowed_mime_types() {
    // (row, tenant, user, expected); `None` policy = absent, `Some(&[])` = present but
    // unrestricted. The result is the intersection, wildcards narrowed to the specific type.
    type Mimes = Option<&'static [&'static str]>;
    #[allow(clippy::type_complexity)]
    let rows: [(&str, Mimes, Mimes, Option<&[&str]>); 8] = [
        ("no policies", None, None, None),
        ("both unrestricted", Some(&[]), Some(&[]), None),
        (
            "tenant restricted only",
            Some(&["image/jpeg", "image/png"]),
            Some(&[]),
            Some(&["image/jpeg", "image/png"]),
        ),
        (
            "user restricted only",
            Some(&[]),
            Some(&["text/plain"]),
            Some(&["text/plain"]),
        ),
        (
            "only tenant present",
            Some(&["image/png"]),
            None,
            Some(&["image/png"]),
        ),
        (
            "only user present",
            None,
            Some(&["image/png"]),
            Some(&["image/png"]),
        ),
        (
            "wildcard narrowed to specific",
            Some(&["image/*", "video/mp4"]),
            Some(&["image/jpeg", "video/mp4"]),
            Some(&["image/jpeg", "video/mp4"]),
        ),
        (
            "disjoint",
            Some(&["image/jpeg"]),
            Some(&["text/plain"]),
            Some(&[]),
        ),
    ];
    for (row, tenant, user, expected) in rows {
        let body = |m: Mimes| m.map(body_with_mimes);
        let ep = PolicyResolver::resolve(body(tenant).as_ref(), body(user).as_ref());
        let got = ep.allowed_mime_types.map(|mut v| {
            v.sort();
            v
        });
        assert_eq!(got, expected.map(mimes), "{row}");
    }
}

#[test]
fn resolve_global_size_limit_takes_the_smallest_present_value() {
    const MB: u64 = 1024 * 1024;
    // (tenant, user) -> effective limit; an absent limit is unlimited and never wins.
    for (tenant, user, expected) in [
        (Some(100 * MB), Some(10 * MB), Some(10 * MB)),
        (None, Some(50 * MB), Some(50 * MB)),
        (None, None, None),
    ] {
        let body = |max: Option<u64>| max.map_or_else(PolicyBody::default, body_with_size);
        let ep = PolicyResolver::resolve(Some(&body(tenant)), Some(&body(user)));
        assert_eq!(ep.max_bytes, expected, "tenant={tenant:?} user={user:?}");
    }
    let body = body_with_size(5 * MB);
    assert_eq!(
        PolicyResolver::resolve(Some(&body), None).max_bytes,
        Some(5 * MB)
    );
    assert_eq!(
        PolicyResolver::resolve(None, Some(&body)).max_bytes,
        Some(5 * MB)
    );
}

#[test]
fn resolve_per_mime_overrides_merged_most_restrictive() {
    let mut tenant = PolicyBody::default();
    tenant.size_limits.per_mime = vec![
        override_for("video/*", 1_000_000_000),
        override_for("image/jpeg", 5_000_000),
    ];
    let mut user = PolicyBody::default();
    user.size_limits.per_mime = vec![
        override_for("video/*", 500_000_000), // more restrictive
        override_for("text/plain", 1_000_000),
    ];
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(limit_for(&ep, "video/*"), 500_000_000);
    assert_eq!(limit_for(&ep, "image/jpeg"), 5_000_000);
    assert_eq!(limit_for(&ep, "text/plain"), 1_000_000);
}

#[test]
fn resolve_per_mime_specific_is_tightened_by_covering_wildcard() {
    let mut tenant = PolicyBody::default();
    tenant.size_limits.per_mime = vec![override_for("image/*", 10_000_000)];
    let mut user = PolicyBody::default();
    user.size_limits.per_mime = vec![override_for("image/png", 50_000_000)];
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(limit_for(&ep, "image/png"), 10_000_000);
    assert_eq!(limit_for(&ep, "image/*"), 10_000_000);
}

#[test]
fn resolve_metadata_limits_takes_most_restrictive_per_field() {
    let tenant = body_with_meta_limits(50, 128, 512);
    let user = body_with_meta_limits(20, 256, 256);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(ep.metadata_limits.max_pairs, Some(20));
    assert_eq!(ep.metadata_limits.max_key_len, Some(128));
    assert_eq!(ep.metadata_limits.max_value_len, Some(256));

    // A limit present on one side only is kept.
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&PolicyBody::default()));
    assert_eq!(ep.metadata_limits.max_pairs, Some(50));
    assert_eq!(ep.metadata_limits.max_key_len, Some(128));
}

#[test]
fn mime_allowed_matches_exact_and_subtype_wildcards_only() {
    // (mime, allowed patterns, expected). A stored pattern without `/` (e.g. "image") must not
    // act as a wildcard for `image/...`.
    let rows: [(&str, &[&str], bool); 10] = [
        ("image/jpeg", &["image/jpeg"], true),
        ("image/jpeg", &["image/*"], true),
        ("video/mp4", &["image/*"], false),
        ("image/jpeg", &["image"], false),
        ("text/plain", &["text"], false),
        ("image/jpeg", &[], false),
        ("text/plain", &["image/jpeg", "text/plain"], true),
        ("text/plain", &["image/jpeg", "image/*"], false),
        ("image/png", &["image/*"], true),
        ("application/pdf", &["application/pdf", "image/*"], true),
    ];
    for (mime, patterns, expected) in rows {
        assert_eq!(
            PolicyResolver::mime_allowed(mime, &mimes(patterns)),
            expected,
            "{mime} against {patterns:?}"
        );
    }
}

#[test]
fn check_allowed_mime_applies_the_allow_list() {
    // (row, allow list, mime, `None`: permitted, `Some(m)`: rejected as `PolicyMimeNotAllowed { m }`)
    #[allow(clippy::type_complexity)]
    let rows: [(&str, Option<&[&str]>, &str, Option<&str>); 5] = [
        ("no restriction", None, "application/octet-stream", None),
        (
            "wildcard match",
            Some(&["image/*", "text/plain"]),
            "image/jpeg",
            None,
        ),
        (
            "exact match",
            Some(&["image/*", "text/plain"]),
            "text/plain",
            None,
        ),
        (
            "non-matching type",
            Some(&["image/jpeg"]),
            "video/mp4",
            Some("video/mp4"),
        ),
        (
            "empty list rejects all",
            Some(&[]),
            "image/jpeg",
            Some("image/jpeg"),
        ),
    ];
    for (row, allowed, mime, rejected) in rows {
        let policy = EffectivePolicy {
            allowed_mime_types: allowed.map(mimes),
            ..open_policy()
        };
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
    let policy = |max_bytes, per_mime: &[(&str, u64)]| EffectivePolicy {
        max_bytes,
        per_mime_max_bytes: per_mime.iter().map(|(m, b)| override_for(m, *b)).collect(),
        ..open_policy()
    };
    let per_mime: &[(&str, u64)] = &[("image/*", 500_000), ("video/*", 50_000_000)];

    // (row, global policy limit, per-mime overrides, mime, backend ceiling, expected)
    #[allow(clippy::type_complexity)]
    let rows: [(
        &str,
        Option<u64>,
        &[(&str, u64)],
        &str,
        Option<u64>,
        Option<u64>,
    ); 8] = [
        ("no restrictions", None, &[], "image/jpeg", None, None),
        (
            "policy global",
            Some(1_000_000),
            &[],
            "image/jpeg",
            None,
            Some(1_000_000),
        ),
        (
            "backend ceiling below the policy",
            Some(10_000_000),
            &[],
            "image/jpeg",
            Some(5_000_000),
            Some(5_000_000),
        ),
        (
            "policy below the backend ceiling",
            Some(2_000_000),
            &[],
            "image/jpeg",
            Some(5_000_000),
            Some(2_000_000),
        ),
        (
            "per-mime image",
            None,
            per_mime,
            "image/jpeg",
            None,
            Some(500_000),
        ),
        (
            "per-mime video",
            None,
            per_mime,
            "video/mp4",
            None,
            Some(50_000_000),
        ),
        (
            "no per-mime match, no global",
            None,
            per_mime,
            "text/plain",
            None,
            None,
        ),
        (
            "global below per-mime",
            Some(200_000),
            &[("image/*", 500_000)],
            "image/jpeg",
            None,
            Some(200_000),
        ),
    ];
    for (row, global, overrides, mime, ceiling, expected) in rows {
        assert_eq!(
            PolicyResolver::compute_effective_max_bytes(&policy(global, overrides), mime, ceiling),
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
    let limits = |max_pairs, max_key_len, max_value_len, max_total_bytes| EffectivePolicy {
        metadata_limits: MetadataLimits {
            max_pairs,
            max_key_len,
            max_value_len,
            max_total_bytes,
        },
        ..open_policy()
    };
    let two = [("k1", "v1"), ("k2", "v2")];
    // (row, policy, entries, permitted)
    let rows = [
        ("no limits", open_policy(), pairs(&two), true),
        (
            "max_pairs violated",
            limits(Some(1), None, None, None),
            pairs(&two),
            false,
        ),
        (
            "max_pairs ok",
            limits(Some(2), None, None, None),
            pairs(&two),
            true,
        ),
        (
            "key_len violated",
            limits(None, Some(3), None, None),
            pairs(&[("toolong_key", "v")]),
            false,
        ),
        (
            "value_len violated",
            limits(None, None, Some(5), None),
            pairs(&[("k", "too_long_value")]),
            false,
        ),
        (
            // 4+6 + 2+2 = 14 > 10
            "total_bytes violated",
            limits(None, None, None, Some(10)),
            pairs(&[("key1", "value1"), ("k2", "v2")]),
            false,
        ),
        (
            // 4+6 = 10 <= 20
            "total_bytes ok",
            limits(None, None, None, Some(20)),
            pairs(&[("key1", "value1")]),
            true,
        ),
        (
            "empty entries always ok",
            limits(Some(0), Some(0), Some(0), Some(0)),
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
fn bodies_deserialize_with_defaults_and_omit_absent_rules() {
    // `#[serde(default)]` keeps stored rows written before a field existed readable.
    let body: PolicyBody = serde_json::from_str("{}").unwrap();
    assert_eq!(body, PolicyBody::default());

    // `skip_serializing_if` keeps absent rules out of the stored JSON.
    let rule = RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 365 }),
        ..RetentionRuleBody::default()
    };
    let json = serde_json::to_value(&rule).unwrap();
    assert_eq!(json, serde_json::json!({"age": {"max_age_days": 365}}));
}

#[test]
fn scope_parse_accepts_only_known_spellings() {
    assert_eq!(PolicyScope::parse("tenant"), Some(PolicyScope::Tenant));
    assert_eq!(PolicyScope::parse("user"), Some(PolicyScope::User));
    assert_eq!(PolicyScope::parse("file"), None);
    assert_eq!(RetentionScope::parse("file"), Some(RetentionScope::File));
    assert_eq!(RetentionScope::parse("bad"), None);
}

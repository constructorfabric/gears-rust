//! Unit tests for `PolicyResolver` and `PolicyBody` serde round-trips.

use super::*;

// ── helper builders ────────────────────────────────────────────────────────────

fn body_unrestricted() -> PolicyBody {
    PolicyBody::default()
}

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

// ── allowed mime types ─────────────────────────────────────────────────────────

#[test]
fn resolve_both_unrestricted_returns_none() {
    let ep = PolicyResolver::resolve(Some(&body_unrestricted()), Some(&body_unrestricted()));
    assert!(
        ep.allowed_mime_types.is_none(),
        "no restriction from either level"
    );
}

#[test]
fn resolve_no_policies_returns_none() {
    let ep = PolicyResolver::resolve(None, None);
    assert!(ep.allowed_mime_types.is_none());
    assert!(ep.max_bytes.is_none());
}

#[test]
fn resolve_tenant_restricted_user_unrestricted_returns_tenant_set() {
    let tenant = body_with_mimes(&["image/jpeg", "image/png"]);
    let user = body_unrestricted();
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    let mimes = ep.allowed_mime_types.expect("should be restricted");
    assert!(mimes.contains(&"image/jpeg".to_owned()));
    assert!(mimes.contains(&"image/png".to_owned()));
}

#[test]
fn resolve_user_restricted_tenant_unrestricted_returns_user_set() {
    let tenant = body_unrestricted();
    let user = body_with_mimes(&["text/plain"]);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    let mimes = ep.allowed_mime_types.expect("should be restricted");
    assert_eq!(mimes, vec!["text/plain".to_owned()]);
}

#[test]
fn resolve_intersection_of_mime_types() {
    // Tenant allows image/* and video/mp4; user allows image/jpeg and video/mp4.
    // The intersection must resolve to the NARROWER pattern: image/* ∩ image/jpeg
    // = image/jpeg (NOT image/*, which would keep admitting image/png once the
    // list is enforced as an allow-list), and video/mp4 is an exact match.
    let tenant = body_with_mimes(&["image/*", "video/mp4"]);
    let user = body_with_mimes(&["image/jpeg", "video/mp4"]);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    let mimes = ep.allowed_mime_types.expect("should be restricted");
    assert!(
        mimes.contains(&"image/jpeg".to_owned()),
        "intersection must narrow image/* down to image/jpeg, got {mimes:?}"
    );
    assert!(
        !mimes.contains(&"image/*".to_owned()),
        "the broader image/* wildcard must not survive the intersection, got {mimes:?}"
    );
    assert!(mimes.contains(&"video/mp4".to_owned()));
}

#[test]
fn resolve_disjoint_mime_types_gives_empty_intersection() {
    let tenant = body_with_mimes(&["image/jpeg"]);
    let user = body_with_mimes(&["text/plain"]);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    let mimes = ep.allowed_mime_types.expect("should be restricted");
    assert!(mimes.is_empty(), "disjoint sets -> nothing allowed");
}

// ── size limits ────────────────────────────────────────────────────────────────

#[test]
fn resolve_takes_smallest_global_size_limit() {
    let tenant = body_with_size(100 * 1024 * 1024); // 100 MB
    let user = body_with_size(10 * 1024 * 1024); // 10 MB
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(ep.max_bytes, Some(10 * 1024 * 1024));
}

#[test]
fn resolve_size_limit_none_plus_some_returns_some() {
    let tenant = PolicyBody::default(); // unlimited
    let user = body_with_size(50 * 1024 * 1024);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(ep.max_bytes, Some(50 * 1024 * 1024));
}

#[test]
fn resolve_both_size_limits_none_returns_none() {
    let ep = PolicyResolver::resolve(Some(&body_unrestricted()), Some(&body_unrestricted()));
    assert!(ep.max_bytes.is_none());
}

#[test]
fn resolve_per_mime_overrides_merged_most_restrictive() {
    let mut tenant = PolicyBody::default();
    tenant.size_limits.per_mime = vec![
        MimeSizeOverride {
            mime: "video/*".to_owned(),
            max_bytes: 1_000_000_000,
        },
        MimeSizeOverride {
            mime: "image/jpeg".to_owned(),
            max_bytes: 5_000_000,
        },
    ];
    let mut user = PolicyBody::default();
    user.size_limits.per_mime = vec![
        MimeSizeOverride {
            mime: "video/*".to_owned(),
            max_bytes: 500_000_000,
        }, // more restrictive
        MimeSizeOverride {
            mime: "text/plain".to_owned(),
            max_bytes: 1_000_000,
        },
    ];
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    let video = ep
        .per_mime_max_bytes
        .iter()
        .find(|e| e.mime == "video/*")
        .unwrap();
    assert_eq!(video.max_bytes, 500_000_000);
    let jpeg = ep
        .per_mime_max_bytes
        .iter()
        .find(|e| e.mime == "image/jpeg")
        .unwrap();
    assert_eq!(jpeg.max_bytes, 5_000_000);
    let text = ep
        .per_mime_max_bytes
        .iter()
        .find(|e| e.mime == "text/plain")
        .unwrap();
    assert_eq!(text.max_bytes, 1_000_000);
}

#[test]
fn resolve_per_mime_specific_is_tightened_by_covering_wildcard() {
    // A broader wildcard cap must tighten the more-specific entry it covers,
    // otherwise the most-specific-match consumer would ignore the stricter cap.
    let mut tenant = PolicyBody::default();
    tenant.size_limits.per_mime = vec![MimeSizeOverride {
        mime: "image/*".to_owned(),
        max_bytes: 10_000_000, // 10 MB wildcard cap
    }];
    let mut user = PolicyBody::default();
    user.size_limits.per_mime = vec![MimeSizeOverride {
        mime: "image/png".to_owned(),
        max_bytes: 50_000_000, // 50 MB — looser than the covering wildcard
    }];
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));

    let png = ep
        .per_mime_max_bytes
        .iter()
        .find(|e| e.mime == "image/png")
        .expect("image/png entry present");
    assert_eq!(
        png.max_bytes, 10_000_000,
        "image/png must be capped by the covering image/* wildcard (10MB), not its own 50MB"
    );
    // The wildcard entry itself is retained for other image subtypes.
    let wildcard = ep
        .per_mime_max_bytes
        .iter()
        .find(|e| e.mime == "image/*")
        .expect("image/* entry present");
    assert_eq!(wildcard.max_bytes, 10_000_000);
}

// ── metadata limits ────────────────────────────────────────────────────────────

#[test]
fn resolve_metadata_limits_takes_most_restrictive_per_field() {
    let tenant = body_with_meta_limits(50, 128, 512);
    let user = body_with_meta_limits(20, 256, 256);
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    // max_pairs: min(50, 20) = 20
    assert_eq!(ep.metadata_limits.max_pairs, Some(20));
    // max_key_len: min(128, 256) = 128
    assert_eq!(ep.metadata_limits.max_key_len, Some(128));
    // max_value_len: min(512, 256) = 256
    assert_eq!(ep.metadata_limits.max_value_len, Some(256));
}

#[test]
fn resolve_metadata_limits_none_plus_some_returns_some() {
    let tenant = body_with_meta_limits(100, 64, 256);
    let user = PolicyBody::default(); // no metadata limits at user level
    let ep = PolicyResolver::resolve(Some(&tenant), Some(&user));
    assert_eq!(ep.metadata_limits.max_pairs, Some(100));
    assert_eq!(ep.metadata_limits.max_key_len, Some(64));
}

// ── only-one-level present ─────────────────────────────────────────────────────

#[test]
fn resolve_only_tenant_policy_present() {
    let tenant = PolicyBody {
        allowed_mime_types: vec!["image/png".to_owned()],
        size_limits: SizeLimits {
            max_bytes: Some(20 * 1024 * 1024),
            ..Default::default()
        },
        metadata_limits: MetadataLimits {
            max_pairs: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let ep = PolicyResolver::resolve(Some(&tenant), None);
    assert_eq!(ep.allowed_mime_types, Some(vec!["image/png".to_owned()]));
    assert_eq!(ep.max_bytes, Some(20 * 1024 * 1024));
    assert_eq!(ep.metadata_limits.max_pairs, Some(10));
}

#[test]
fn resolve_only_user_policy_present() {
    let user = body_with_size(5 * 1024 * 1024);
    let ep = PolicyResolver::resolve(None, Some(&user));
    assert_eq!(ep.max_bytes, Some(5 * 1024 * 1024));
    assert!(ep.allowed_mime_types.is_none());
}

// ── serde round-trips ──────────────────────────────────────────────────────────

#[test]
fn policy_body_serde_round_trip() {
    let body = PolicyBody {
        allowed_mime_types: vec!["image/jpeg".to_owned(), "video/*".to_owned()],
        size_limits: SizeLimits {
            max_bytes: Some(100 * 1024 * 1024),
            per_mime: vec![MimeSizeOverride {
                mime: "video/*".to_owned(),
                max_bytes: 1_000_000_000,
            }],
        },
        metadata_limits: MetadataLimits {
            max_pairs: Some(50),
            max_key_len: Some(128),
            max_value_len: Some(512),
            max_total_bytes: Some(4096),
        },
        enabled_event_types: vec!["file.created".to_owned()],
    };
    let json = serde_json::to_string(&body).expect("serialize");
    let round_tripped: PolicyBody = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(body, round_tripped);
}

#[test]
fn retention_rule_body_serde_round_trip() {
    let body = RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 365 }),
        inactivity: Some(InactivityRetention {
            inactivity_days: 90,
        }),
        metadata: Some(MetadataRetention {
            key: "status".to_owned(),
            value: "expired".to_owned(),
        }),
    };
    let json = serde_json::to_string(&body).expect("serialize");
    let round_tripped: RetentionRuleBody = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(body, round_tripped);
}

#[test]
fn empty_policy_body_serializes_to_valid_json() {
    let body = PolicyBody::default();
    let json = serde_json::to_string(&body).expect("serialize");
    // Must be valid JSON; empty vecs are preserved.
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    assert!(parsed.is_object());
}

// ── PolicyScope / RetentionScope parse round-trips ─────────────────────────────

#[test]
fn policy_scope_parse_roundtrip() {
    assert_eq!(PolicyScope::parse("tenant"), Some(PolicyScope::Tenant));
    assert_eq!(PolicyScope::parse("user"), Some(PolicyScope::User));
    assert_eq!(PolicyScope::parse("unknown"), None);
    assert_eq!(PolicyScope::Tenant.as_str(), "tenant");
    assert_eq!(PolicyScope::User.as_str(), "user");
}

#[test]
fn retention_scope_parse_roundtrip() {
    assert_eq!(
        RetentionScope::parse("tenant"),
        Some(RetentionScope::Tenant)
    );
    assert_eq!(RetentionScope::parse("user"), Some(RetentionScope::User));
    assert_eq!(RetentionScope::parse("file"), Some(RetentionScope::File));
    assert_eq!(RetentionScope::parse("bad"), None);
}

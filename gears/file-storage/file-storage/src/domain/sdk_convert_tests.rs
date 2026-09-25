use super::*;
use crate::domain::multipart;
use crate::domain::policy;
use crate::infra::backend::BackendCapabilities;

#[test]
fn policy_body_round_trips_through_sdk() {
    let original = policy::PolicyBody {
        allowed_mime_types: vec!["image/*".to_owned(), "video/mp4".to_owned()],
        size_limits: policy::SizeLimits {
            max_bytes: Some(1024),
            per_mime: vec![policy::MimeSizeOverride {
                mime: "video/*".to_owned(),
                max_bytes: 2048,
            }],
        },
        metadata_limits: policy::MetadataLimits {
            max_pairs: Some(10),
            max_key_len: Some(64),
            max_value_len: Some(256),
            max_total_bytes: Some(4096),
        },
        enabled_event_types: vec!["file.created".to_owned()],
    };

    let sdk = policy_body_to_sdk(original.clone());
    let back = policy_body_to_domain(sdk);
    assert_eq!(original, back);
}

#[test]
fn policy_scope_round_trips_through_sdk() {
    for scope in [policy::PolicyScope::Tenant, policy::PolicyScope::User] {
        let sdk = policy_scope_to_sdk(&scope);
        let back = policy_scope_to_domain(sdk);
        assert_eq!(scope, back);
    }
}

#[test]
fn retention_scope_round_trips_through_sdk() {
    for scope in [
        policy::RetentionScope::Tenant,
        policy::RetentionScope::User,
        policy::RetentionScope::File,
    ] {
        let sdk = retention_scope_to_sdk(&scope);
        let back = retention_scope_to_domain(sdk);
        assert_eq!(scope, back);
    }
}

#[test]
fn retention_rule_body_round_trips_through_sdk() {
    let original = policy::RetentionRuleBody {
        age: Some(policy::AgeRetention { max_age_days: 30 }),
        inactivity: Some(policy::InactivityRetention {
            inactivity_days: 90,
        }),
        metadata: Some(policy::MetadataRetention {
            key: "archive".to_owned(),
            value: "true".to_owned(),
        }),
    };

    let sdk = retention_rule_body_to_sdk(original.clone());
    let back = retention_rule_body_to_domain(sdk);
    assert_eq!(original, back);
}

#[test]
fn bind_state_maps_every_variant() {
    assert_eq!(
        bind_state(multipart::BindState::Bound),
        file_storage_sdk::BindState::Bound
    );
    assert_eq!(
        bind_state(multipart::BindState::Conflict),
        file_storage_sdk::BindState::Conflict
    );
    assert_eq!(
        bind_state(multipart::BindState::Manual),
        file_storage_sdk::BindState::Manual
    );
}

#[test]
fn multipart_upload_state_maps_every_variant() {
    assert_eq!(
        multipart_upload_state(&multipart::MultipartUploadState::InProgress),
        file_storage_sdk::MultipartUploadState::InProgress
    );
    assert_eq!(
        multipart_upload_state(&multipart::MultipartUploadState::Completing),
        file_storage_sdk::MultipartUploadState::Completing
    );
    assert_eq!(
        multipart_upload_state(&multipart::MultipartUploadState::Completed),
        file_storage_sdk::MultipartUploadState::Completed
    );
    assert_eq!(
        multipart_upload_state(&multipart::MultipartUploadState::Aborted),
        file_storage_sdk::MultipartUploadState::Aborted
    );
}

#[test]
fn storage_carries_the_three_public_capability_flags() {
    let caps = BackendCapabilities {
        multipart_native: true,
        encryption_native: false,
        range_native: true,
        presigned_url_internal: false,
        max_size_bytes: Some(999),
        durable: true,
    };
    let s = storage("mem".to_owned(), caps);
    assert_eq!(s.id, "mem");
    assert!(s.capabilities.multipart_native);
    assert!(!s.capabilities.encryption_native);
    assert!(s.capabilities.range_native);
}

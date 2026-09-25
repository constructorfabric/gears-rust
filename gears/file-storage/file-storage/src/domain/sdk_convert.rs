//! Domain type ⟷ SDK model conversions for [`super::local_client::FileStorageLocalClient`].
//!
//! `file_storage_sdk` cannot depend on this crate (it would be a dependency
//! cycle — this crate already depends on the SDK), so several domain types
//! that are richer than their SDK counterpart (`domain::policy::PolicyBody`
//! carries `serde`, for its `policies.body` JSON-column storage; the SDK
//! crate is deliberately `serde`-free — see `file_storage_sdk::models`'s
//! module doc) cannot simply be re-exported from the SDK crate. This module
//! is the one place their field-for-field mapping lives, so it happens
//! exactly once instead of being reimplemented ad hoc at each
//! [`super::local_client::FileStorageLocalClient`] method.

use crate::domain::multipart;
use crate::domain::policy;
use crate::infra::backend::BackendCapabilities;

// ── multipart upload ─────────────────────────────────────────────────────────

pub(super) fn multipart_part_plan(
    p: multipart::MultipartPartPlan,
) -> file_storage_sdk::MultipartPartPlan {
    file_storage_sdk::MultipartPartPlan {
        part_number: p.part_number,
        offset: p.offset,
        size: p.size,
        upload_url: p.upload_url,
    }
}

pub(super) fn multipart_plan(p: multipart::MultipartPlan) -> file_storage_sdk::MultipartPlan {
    file_storage_sdk::MultipartPlan {
        upload_id: p.upload_id,
        version_id: p.version_id,
        part_hash_algorithm: p.part_hash_algorithm,
        part_size: p.part_size,
        parts: p.parts.into_iter().map(multipart_part_plan).collect(),
        expires_at: p.expires_at,
    }
}

pub(super) fn multipart_upload_state(
    s: &multipart::MultipartUploadState,
) -> file_storage_sdk::MultipartUploadState {
    match s {
        multipart::MultipartUploadState::InProgress => {
            file_storage_sdk::MultipartUploadState::InProgress
        }
        multipart::MultipartUploadState::Completing => {
            file_storage_sdk::MultipartUploadState::Completing
        }
        multipart::MultipartUploadState::Completed => {
            file_storage_sdk::MultipartUploadState::Completed
        }
        multipart::MultipartUploadState::Aborted => file_storage_sdk::MultipartUploadState::Aborted,
    }
}

pub(super) fn received_part(p: &multipart::ReceivedPart) -> file_storage_sdk::ReceivedPart {
    file_storage_sdk::ReceivedPart {
        part_number: p.part_number,
        size: p.size,
        uploaded_at: p.uploaded_at,
    }
}

pub(super) fn missing_part(p: multipart::MissingPart) -> file_storage_sdk::MissingPart {
    file_storage_sdk::MissingPart {
        part_number: p.part_number,
        offset: p.offset,
        size: p.size,
        upload_url: p.upload_url,
    }
}

pub(super) fn multipart_status(
    s: multipart::MultipartUploadStatus,
) -> file_storage_sdk::MultipartStatus {
    file_storage_sdk::MultipartStatus {
        upload_id: s.upload_id,
        version_id: s.version_id,
        state: multipart_upload_state(&s.state),
        declared_mime: s.declared_mime,
        declared_size: s.declared_size,
        part_size: s.part_size,
        created_at: s.created_at,
        expires_at: s.expires_at,
        received: s.received.iter().map(received_part).collect(),
        missing: s.missing.into_iter().map(missing_part).collect(),
    }
}

pub(super) fn bind_state(b: multipart::BindState) -> file_storage_sdk::BindState {
    match b {
        multipart::BindState::Bound => file_storage_sdk::BindState::Bound,
        multipart::BindState::Conflict => file_storage_sdk::BindState::Conflict,
        multipart::BindState::Manual => file_storage_sdk::BindState::Manual,
    }
}

pub(super) fn completed_multipart_upload(
    c: multipart::CompletedMultipartUpload,
) -> file_storage_sdk::CompletedMultipartUpload {
    file_storage_sdk::CompletedMultipartUpload {
        version_id: c.version_id,
        size: c.size,
        hash_algorithm: c.hash_algorithm.to_owned(),
        content_hash: c.content_hash,
        hash_mode: c.hash_mode.as_str().to_owned(),
        part_count: c.part_count,
        manifest: c.manifest,
        bind_state: bind_state(c.bind_state),
        etag: c.etag,
        current_etag: c.current_etag,
    }
}

pub(super) fn multipart_complete_outcome(
    o: multipart::MultipartCompleteOutcome,
) -> file_storage_sdk::MultipartCompleteOutcome {
    match o {
        multipart::MultipartCompleteOutcome::Completed(c) => {
            file_storage_sdk::MultipartCompleteOutcome::Completed(completed_multipart_upload(c))
        }
        multipart::MultipartCompleteOutcome::Completing { retry_after_secs } => {
            file_storage_sdk::MultipartCompleteOutcome::Completing { retry_after_secs }
        }
    }
}

// ── storage backends ─────────────────────────────────────────────────────────

pub(super) fn storage(id: String, caps: BackendCapabilities) -> file_storage_sdk::Storage {
    file_storage_sdk::Storage {
        id,
        capabilities: file_storage_sdk::StorageCapabilities {
            multipart_native: caps.multipart_native,
            encryption_native: caps.encryption_native,
            range_native: caps.range_native,
        },
    }
}

// ── policy ───────────────────────────────────────────────────────────────────

pub(super) fn policy_scope_to_domain(s: file_storage_sdk::PolicyScope) -> policy::PolicyScope {
    match s {
        file_storage_sdk::PolicyScope::Tenant => policy::PolicyScope::Tenant,
        file_storage_sdk::PolicyScope::User => policy::PolicyScope::User,
    }
}

pub(super) fn policy_scope_to_sdk(s: &policy::PolicyScope) -> file_storage_sdk::PolicyScope {
    match s {
        policy::PolicyScope::Tenant => file_storage_sdk::PolicyScope::Tenant,
        policy::PolicyScope::User => file_storage_sdk::PolicyScope::User,
    }
}

pub(super) fn mime_size_override_to_domain(
    o: file_storage_sdk::MimeSizeOverride,
) -> policy::MimeSizeOverride {
    policy::MimeSizeOverride {
        mime: o.mime,
        max_bytes: o.max_bytes,
    }
}

pub(super) fn mime_size_override_to_sdk(
    o: policy::MimeSizeOverride,
) -> file_storage_sdk::MimeSizeOverride {
    file_storage_sdk::MimeSizeOverride {
        mime: o.mime,
        max_bytes: o.max_bytes,
    }
}

pub(super) fn size_limits_to_domain(l: file_storage_sdk::SizeLimits) -> policy::SizeLimits {
    policy::SizeLimits {
        max_bytes: l.max_bytes,
        per_mime: l
            .per_mime
            .into_iter()
            .map(mime_size_override_to_domain)
            .collect(),
    }
}

pub(super) fn size_limits_to_sdk(l: policy::SizeLimits) -> file_storage_sdk::SizeLimits {
    file_storage_sdk::SizeLimits {
        max_bytes: l.max_bytes,
        per_mime: l
            .per_mime
            .into_iter()
            .map(mime_size_override_to_sdk)
            .collect(),
    }
}

pub(super) fn metadata_limits_to_domain(
    l: &file_storage_sdk::MetadataLimits,
) -> policy::MetadataLimits {
    policy::MetadataLimits {
        max_pairs: l.max_pairs,
        max_key_len: l.max_key_len,
        max_value_len: l.max_value_len,
        max_total_bytes: l.max_total_bytes,
    }
}

pub(super) fn metadata_limits_to_sdk(
    l: &policy::MetadataLimits,
) -> file_storage_sdk::MetadataLimits {
    file_storage_sdk::MetadataLimits {
        max_pairs: l.max_pairs,
        max_key_len: l.max_key_len,
        max_value_len: l.max_value_len,
        max_total_bytes: l.max_total_bytes,
    }
}

pub(super) fn policy_body_to_domain(b: file_storage_sdk::PolicyBody) -> policy::PolicyBody {
    policy::PolicyBody {
        allowed_mime_types: b.allowed_mime_types,
        size_limits: size_limits_to_domain(b.size_limits),
        metadata_limits: metadata_limits_to_domain(&b.metadata_limits),
        enabled_event_types: b.enabled_event_types,
    }
}

pub(super) fn policy_body_to_sdk(b: policy::PolicyBody) -> file_storage_sdk::PolicyBody {
    file_storage_sdk::PolicyBody {
        allowed_mime_types: b.allowed_mime_types,
        size_limits: size_limits_to_sdk(b.size_limits),
        metadata_limits: metadata_limits_to_sdk(&b.metadata_limits),
        enabled_event_types: b.enabled_event_types,
    }
}

pub(super) fn stored_policy(p: policy::StoredPolicy) -> file_storage_sdk::Policy {
    file_storage_sdk::Policy {
        policy_id: p.policy_id,
        tenant_id: p.tenant_id,
        scope: policy_scope_to_sdk(&p.scope),
        scope_owner_id: p.scope_owner_id,
        body: policy_body_to_sdk(p.body),
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

pub(super) fn effective_policy(p: policy::EffectivePolicy) -> file_storage_sdk::EffectivePolicy {
    file_storage_sdk::EffectivePolicy {
        allowed_mime_types: p.allowed_mime_types,
        max_bytes: p.max_bytes,
        per_mime_max_bytes: p
            .per_mime_max_bytes
            .into_iter()
            .map(mime_size_override_to_sdk)
            .collect(),
        metadata_limits: metadata_limits_to_sdk(&p.metadata_limits),
    }
}

// ── retention rules ──────────────────────────────────────────────────────────

pub(super) fn retention_scope_to_domain(
    s: file_storage_sdk::RetentionScope,
) -> policy::RetentionScope {
    match s {
        file_storage_sdk::RetentionScope::Tenant => policy::RetentionScope::Tenant,
        file_storage_sdk::RetentionScope::User => policy::RetentionScope::User,
        file_storage_sdk::RetentionScope::File => policy::RetentionScope::File,
    }
}

pub(super) fn retention_scope_to_sdk(
    s: &policy::RetentionScope,
) -> file_storage_sdk::RetentionScope {
    match s {
        policy::RetentionScope::Tenant => file_storage_sdk::RetentionScope::Tenant,
        policy::RetentionScope::User => file_storage_sdk::RetentionScope::User,
        policy::RetentionScope::File => file_storage_sdk::RetentionScope::File,
    }
}

pub(super) fn retention_rule_body_to_domain(
    b: file_storage_sdk::RetentionRuleBody,
) -> policy::RetentionRuleBody {
    policy::RetentionRuleBody {
        age: b.age.map(|a| policy::AgeRetention {
            max_age_days: a.max_age_days,
        }),
        inactivity: b.inactivity.map(|i| policy::InactivityRetention {
            inactivity_days: i.inactivity_days,
        }),
        metadata: b.metadata.map(|m| policy::MetadataRetention {
            key: m.key,
            value: m.value,
        }),
    }
}

pub(super) fn retention_rule_body_to_sdk(
    b: policy::RetentionRuleBody,
) -> file_storage_sdk::RetentionRuleBody {
    file_storage_sdk::RetentionRuleBody {
        age: b.age.map(|a| file_storage_sdk::AgeRetention {
            max_age_days: a.max_age_days,
        }),
        inactivity: b.inactivity.map(|i| file_storage_sdk::InactivityRetention {
            inactivity_days: i.inactivity_days,
        }),
        metadata: b.metadata.map(|m| file_storage_sdk::MetadataRetention {
            key: m.key,
            value: m.value,
        }),
    }
}

pub(super) fn stored_retention_rule(
    r: policy::StoredRetentionRule,
) -> file_storage_sdk::RetentionRule {
    file_storage_sdk::RetentionRule {
        rule_id: r.rule_id,
        tenant_id: r.tenant_id,
        scope: retention_scope_to_sdk(&r.scope),
        scope_target_id: r.scope_target_id,
        body: retention_rule_body_to_sdk(r.body),
        created_at: r.created_at,
    }
}

#[cfg(test)]
#[path = "sdk_convert_tests.rs"]
mod sdk_convert_tests;

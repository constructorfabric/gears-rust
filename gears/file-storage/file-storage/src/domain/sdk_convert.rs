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
//!
//! Every one-way domain-to-SDK conversion below destructures its source
//! struct field-by-field (no `..`) instead of reading fields off it
//! (`p.field`), so that adding a field to the domain type is a compile error
//! here rather than a silently-never-exported SDK field. A field
//! deliberately not exported is bound `_` with a short comment saying why.

use crate::domain::multipart;
use crate::domain::policy;
use crate::infra::backend::BackendCapabilities;

// ── multipart upload ─────────────────────────────────────────────────────────

pub(super) fn multipart_part_plan(
    p: multipart::MultipartPartPlan,
) -> file_storage_sdk::MultipartPartPlan {
    let multipart::MultipartPartPlan {
        part_number,
        offset,
        size,
        upload_url,
    } = p;
    file_storage_sdk::MultipartPartPlan {
        part_number,
        offset,
        size,
        upload_url,
    }
}

pub(super) fn multipart_plan(p: multipart::MultipartPlan) -> file_storage_sdk::MultipartPlan {
    let multipart::MultipartPlan {
        upload_id,
        version_id,
        part_hash_algorithm,
        part_size,
        parts,
        expires_at,
    } = p;
    file_storage_sdk::MultipartPlan {
        upload_id,
        version_id,
        part_hash_algorithm,
        part_size,
        parts: parts.into_iter().map(multipart_part_plan).collect(),
        expires_at,
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
    // `p` is a shared reference, so this binds each field by reference
    // (match ergonomics) -- dereferenced below, same zero-clone semantics as
    // the plain `p.field` reads this replaces (every field here is `Copy`).
    let multipart::ReceivedPart {
        part_number,
        size,
        uploaded_at,
    } = p;
    file_storage_sdk::ReceivedPart {
        part_number: *part_number,
        size: *size,
        uploaded_at: *uploaded_at,
    }
}

pub(super) fn missing_part(p: multipart::MissingPart) -> file_storage_sdk::MissingPart {
    let multipart::MissingPart {
        part_number,
        offset,
        size,
        upload_url,
    } = p;
    file_storage_sdk::MissingPart {
        part_number,
        offset,
        size,
        upload_url,
    }
}

pub(super) fn multipart_status(
    s: multipart::MultipartUploadStatus,
) -> file_storage_sdk::MultipartStatus {
    let multipart::MultipartUploadStatus {
        upload_id,
        version_id,
        state,
        declared_mime,
        declared_size,
        part_size,
        created_at,
        expires_at,
        received,
        missing,
    } = s;
    file_storage_sdk::MultipartStatus {
        upload_id,
        version_id,
        state: multipart_upload_state(&state),
        declared_mime,
        declared_size,
        part_size,
        created_at,
        expires_at,
        received: received.iter().map(received_part).collect(),
        missing: missing.into_iter().map(missing_part).collect(),
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
    // `bind_state` is renamed on the way out -- it would otherwise shadow the
    // `bind_state` conversion function called a few lines down.
    let multipart::CompletedMultipartUpload {
        version_id,
        size,
        hash_algorithm,
        content_hash,
        hash_mode,
        part_count,
        manifest,
        bind_state: source_bind_state,
        etag,
        current_etag,
    } = c;
    file_storage_sdk::CompletedMultipartUpload {
        version_id,
        size,
        hash_algorithm: hash_algorithm.to_owned(),
        content_hash,
        hash_mode: hash_mode.as_str().to_owned(),
        part_count,
        manifest,
        bind_state: bind_state(source_bind_state),
        etag,
        current_etag,
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
    let BackendCapabilities {
        multipart_native,
        encryption_native,
        range_native,
        // Backend-internal / operational details, deliberately not part of
        // the public SDK-facing capability surface.
        presigned_url_internal: _,
        max_size_bytes: _,
        durable: _,
    } = caps;
    file_storage_sdk::Storage {
        id,
        capabilities: file_storage_sdk::StorageCapabilities {
            multipart_native,
            encryption_native,
            range_native,
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
    let policy::StoredPolicy {
        policy_id,
        tenant_id,
        scope,
        scope_owner_id,
        body,
        created_at,
        updated_at,
    } = p;
    file_storage_sdk::Policy {
        policy_id,
        tenant_id,
        scope: policy_scope_to_sdk(&scope),
        scope_owner_id,
        body: policy_body_to_sdk(body),
        created_at,
        updated_at,
    }
}

pub(super) fn effective_policy(p: policy::EffectivePolicy) -> file_storage_sdk::EffectivePolicy {
    let policy::EffectivePolicy {
        allowed_mime_types,
        max_bytes,
        per_mime_max_bytes,
        metadata_limits,
    } = p;
    file_storage_sdk::EffectivePolicy {
        allowed_mime_types,
        max_bytes,
        per_mime_max_bytes: per_mime_max_bytes
            .into_iter()
            .map(mime_size_override_to_sdk)
            .collect(),
        metadata_limits: metadata_limits_to_sdk(&metadata_limits),
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
    let policy::StoredRetentionRule {
        rule_id,
        tenant_id,
        scope,
        scope_target_id,
        body,
        created_at,
    } = r;
    file_storage_sdk::RetentionRule {
        rule_id,
        tenant_id,
        scope: retention_scope_to_sdk(&scope),
        scope_target_id,
        body: retention_rule_body_to_sdk(body),
        created_at,
    }
}

#[cfg(test)]
#[path = "sdk_convert_tests.rs"]
mod sdk_convert_tests;

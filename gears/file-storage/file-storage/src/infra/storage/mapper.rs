//! Mapping between `SeaORM` entity models and SDK domain types.
//!
//! `owner_kind` / `status` are stored as text guarded by DB CHECK constraints,
//! so an unparseable value indicates DB corruption; we fall back to the safe
//! default and log rather than panic.

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, OwnerKind, VersionStatus};

use crate::infra::storage::entity::{custom_metadata, file, file_version};

impl From<file::Model> for File {
    fn from(e: file::Model) -> Self {
        let owner_kind = OwnerKind::parse(&e.owner_kind).unwrap_or_else(|| {
            tracing::error!(value = %e.owner_kind, file_id = %e.file_id, "invalid owner_kind in DB");
            OwnerKind::User
        });
        Self {
            file_id: e.file_id,
            tenant_id: e.tenant_id,
            owner_kind,
            owner_id: e.owner_id,
            name: e.name,
            gts_file_type: e.gts_file_type,
            content_id: e.content_id,
            meta_version: e.meta_version,
            created_at: e.created_at,
            last_modified_at: e.last_modified_at,
        }
    }
}

impl From<file_version::Model> for FileVersion {
    fn from(e: file_version::Model) -> Self {
        let status = VersionStatus::parse(&e.status).unwrap_or_else(|| {
            tracing::error!(value = %e.status, version_id = %e.version_id, "invalid version status in DB");
            VersionStatus::Pending
        });
        Self {
            file_id: e.file_id,
            version_id: e.version_id,
            mime_type: e.mime_type,
            size: e.size,
            hash_algorithm: e.hash_algorithm,
            hash_value: e.hash_value,
            status,
            is_current: e.is_current,
            backend_id: e.backend_id,
            backend_path: e.backend_path,
            created_at: e.created_at,
        }
    }
}

impl From<custom_metadata::Model> for CustomMetadataEntry {
    fn from(e: custom_metadata::Model) -> Self {
        Self {
            key: e.key,
            value: e.value,
        }
    }
}

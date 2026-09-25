//! Mapping between `SeaORM` entity models and SDK domain types.
//!
//! `owner_kind` / `status` are stored as text guarded by DB CHECK constraints, so
//! reaching an unparseable value here means the row was corrupted in a way the
//! CHECK constraint did not catch (e.g. written by an out-of-band tool, or a
//! future migration widening the CHECK without updating this parser). That is
//! a data-integrity fault, not a "pick a safe default and carry on" situation --
//! silently substituting a default would change how the caller reasons about
//! ownership/lifecycle state on a row that is, in fact, unreadable. So this
//! surfaces a `DomainError::Database` instead, the same shape
//! `policy_repo::map_model`/`retention_rule_repo::map_model`/
//! `multipart_repo::session_from_model` already use for the same class of
//! unparseable-enum-column fault elsewhere in this gear.
//!
//! Plain fallible functions, not `TryFrom` impls, matching those three
//! mappers' own convention in this gear.

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, OwnerKind, VersionStatus};

use crate::domain::error::DomainError;
use crate::infra::storage::entity::{custom_metadata, file, file_version};

/// Map a `files` row to the SDK `File`, or `Err` on an unparseable `owner_kind`.
pub(crate) fn file_from_model(e: file::Model) -> Result<File, DomainError> {
    let owner_kind = OwnerKind::parse(&e.owner_kind).ok_or_else(|| {
        DomainError::database(format!(
            "invalid owner_kind in DB for file {}: {}",
            e.file_id, e.owner_kind
        ))
    })?;
    Ok(File {
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
    })
}

/// Map a `file_versions` row to the SDK `FileVersion`, or `Err` on an
/// unparseable `status`.
pub(crate) fn file_version_from_model(e: file_version::Model) -> Result<FileVersion, DomainError> {
    let status = VersionStatus::parse(&e.status).ok_or_else(|| {
        DomainError::database(format!(
            "invalid version status in DB for version {}: {}",
            e.version_id, e.status
        ))
    })?;
    Ok(FileVersion {
        file_id: e.file_id,
        version_id: e.version_id,
        mime_type: e.mime_type,
        size: e.size,
        hash_algorithm: e.hash_algorithm,
        hash_value: e.hash_value,
        hash_mode: e.hash_mode,
        part_count: e.part_count,
        status,
        is_current: e.is_current,
        backend_id: e.backend_id,
        backend_path: e.backend_path,
        created_at: e.created_at,
        bound_on_finalize: e.bound_on_finalize,
    })
}

impl From<custom_metadata::Model> for CustomMetadataEntry {
    fn from(e: custom_metadata::Model) -> Self {
        Self {
            key: e.key,
            value: e.value,
        }
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{file_from_model, file_version_from_model};
    use crate::domain::error::DomainError;
    use crate::infra::storage::entity::{file, file_version};

    /// A DB CHECK constraint keeps `owner_kind`/`status` to their known
    /// values on any write that goes through this gear's own repos (see
    /// `tests/migration_test.rs::files_rejects_invalid_owner_kind`) -- so a
    /// garbage value can only ever reach [`file_from_model`]/
    /// [`file_version_from_model`] via a row corrupted out-of-band. That
    /// makes the DB round trip the wrong tool for this test: it would just
    /// fail the CHECK before the mapper is ever reached. These two tests
    /// exercise the mapper functions directly against a hand-built `Model`
    /// instead.
    #[test]
    fn file_from_model_errors_on_unparseable_owner_kind() {
        let now = OffsetDateTime::now_utc();
        let model = file::Model {
            file_id: Uuid::now_v7(),
            tenant_id: Uuid::now_v7(),
            owner_kind: "robot".to_owned(),
            owner_id: Uuid::now_v7(),
            name: "doc.bin".to_owned(),
            gts_file_type: "cf.fstorage.file.type.v1~x.test.file.type.v1~".to_owned(),
            content_id: None,
            meta_version: 0,
            created_at: now,
            last_modified_at: now,
        };
        let err = file_from_model(model)
            .expect_err("an unparseable owner_kind must surface as an error, not a default");
        assert!(
            matches!(err, DomainError::Database { .. }),
            "expected DomainError::Database, got {err:?}"
        );
    }

    #[test]
    fn file_version_from_model_errors_on_unparseable_status() {
        let now = OffsetDateTime::now_utc();
        let model = file_version::Model {
            file_id: Uuid::now_v7(),
            version_id: Uuid::now_v7(),
            mime_type: "text/plain".to_owned(),
            size: 0,
            hash_algorithm: "SHA-256".to_owned(),
            hash_value: vec![0u8; 32],
            hash_mode: "whole-sha256".to_owned(),
            part_count: None,
            status: "quantum-superposition".to_owned(),
            is_current: false,
            backend_id: "mem".to_owned(),
            backend_path: "/f/v".to_owned(),
            created_at: now,
            bound_on_finalize: false,
        };
        let err = file_version_from_model(model)
            .expect_err("an unparseable status must surface as an error, not a default");
        assert!(
            matches!(err, DomainError::Database { .. }),
            "expected DomainError::Database, got {err:?}"
        );
    }
}

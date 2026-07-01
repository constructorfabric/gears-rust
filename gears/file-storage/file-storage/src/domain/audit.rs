//! Transactional-outbox domain records for the file-storage gear.
//!
//! An [`AuditEntry`] is inserted into the `audit_outbox` table, and a
//! [`FileEvent`] into the `events_outbox` table, in the **same DB transaction**
//! as every write mutation, guaranteeing 100% coverage with no silent drops
//! (the transactional-outbox pattern). Both are pure domain records: the
//! control-plane services build them and hand them to the [`Store`] facade,
//! which persists them — so neither the services nor the store depend on the
//! persistence repo layer for these types.
//!
//! [`Store`]: crate::infra::storage::Store
//!
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness
//! @cpt-cf-file-storage-fr-file-events

#![allow(unknown_lints, de0309_must_have_domain_model)]

use time::OffsetDateTime;
use uuid::Uuid;

/// The canonical set of write operations that are audited.
///
/// @cpt-cf-file-storage-fr-audit-trail
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOperation {
    /// `POST /files` — a new file record was created.
    Create,
    /// `POST /files/{id}/versions/bind` — the content pointer was swapped.
    PatchContent,
    /// `PATCH /files/{id}` — custom metadata was updated.
    PatchMetadata,
    /// `DELETE /files/{id}` — the file (and all versions) was removed.
    DeleteFile,
    /// `DELETE /files/{id}/versions/{vid}` — a single version was removed.
    DeleteVersion,
    /// `POST /files/{id}/multipart/{uid}/complete` — multipart assembly finished.
    MultipartComplete,
    /// `DELETE /files/{id}/multipart/{uid}` — multipart session was aborted.
    MultipartAbort,
    /// `POST /files/{id}/versions/{vid}/finalize` — version bytes finalised.
    FinalizeVersion,
    /// Background sweep deleted a version or file due to a retention policy.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    RetentionDelete,
    /// A file's content was moved from one backend to another.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    BackendMigrate,
    /// A pending version or multipart session was cleaned up by the orphan
    /// reconciliation sweep.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    OrphanReconcile,
    /// Ownership of a file was transferred from one owner to another.
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    TransferOwnership,
}

impl AuditOperation {
    /// Stable string representation stored in the `operation` column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::PatchContent => "patch_content",
            Self::PatchMetadata => "patch_metadata",
            Self::DeleteFile => "delete_file",
            Self::DeleteVersion => "delete_version",
            Self::MultipartComplete => "multipart_complete",
            Self::MultipartAbort => "multipart_abort",
            Self::FinalizeVersion => "finalize_version",
            Self::RetentionDelete => "retention_delete",
            Self::BackendMigrate => "backend_migrate",
            Self::OrphanReconcile => "orphan_reconcile",
            Self::TransferOwnership => "transfer_ownership",
        }
    }
}

/// Outcome of an audited operation.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Success,
    Failure,
}

impl AuditOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

/// A file-event to be enqueued in the `events_outbox` table.
///
/// Built by the control-plane services (and the cleanup engine) and handed to
/// the `Store`, which enqueues it in the same transaction as the mutation it
/// describes — the file-event counterpart to [`AuditEntry`].
///
/// @cpt-cf-file-storage-fr-file-events
#[derive(Debug, Clone)]
pub struct FileEvent {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub file_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
}

/// All data needed to emit one audit row.
///
/// Build with [`AuditEntry::new`]; the `Store` inserts it transactionally.
///
/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub tenant_id: Uuid,
    pub actor_kind: String,
    pub actor_id: Uuid,
    pub file_id: Option<Uuid>,
    pub operation: AuditOperation,
    pub outcome: AuditOutcome,
    /// JSON object with operation-specific detail (`version_id`, etc.).
    pub detail: serde_json::Value,
    pub occurred_at: OffsetDateTime,
}

impl AuditEntry {
    /// Create an audit entry for a successful write.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub fn success(
        tenant_id: Uuid,
        actor_kind: impl Into<String>,
        actor_id: Uuid,
        file_id: Option<Uuid>,
        operation: AuditOperation,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            tenant_id,
            actor_kind: actor_kind.into(),
            actor_id,
            file_id,
            operation,
            outcome: AuditOutcome::Success,
            detail,
            occurred_at: OffsetDateTime::now_utc(),
        }
    }

    /// Create an audit entry for a failed write attempt.
    pub fn failure(
        tenant_id: Uuid,
        actor_kind: impl Into<String>,
        actor_id: Uuid,
        file_id: Option<Uuid>,
        operation: AuditOperation,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            tenant_id,
            actor_kind: actor_kind.into(),
            actor_id,
            file_id,
            operation,
            outcome: AuditOutcome::Failure,
            detail,
            occurred_at: OffsetDateTime::now_utc(),
        }
    }
}

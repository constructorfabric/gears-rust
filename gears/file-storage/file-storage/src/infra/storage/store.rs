//! Unit-of-work persistence facade — the single touch-point for `toolkit_db`.
//!
//! [`Store`] owns the `DBProvider`, the tenant-scoped repositories, and all
//! connection-lifecycle / transaction logic. Nothing outside this module needs to
//! import `toolkit_db`, open a `conn()`, or call `transaction_ref_mapped`.
//!
//! Intent-level methods mirror the operations the domain services need without
//! exposing `DBRunner`, `conn`, or `transaction_ref_mapped` to callers. The
//! bind and metadata-patch atomicity (DESIGN §3.7) are preserved verbatim —
//! the transaction code moved here unchanged from `service.rs`.
//!
//! ETag/If-Match semantics and the `AccessScope` decisions live here because
//! they are persistence concerns (which scope to use when querying each table),
//! not authorization decisions (those stay in `FileService`).
//!
//! P2-M1 adds policy store intent-level methods (`get_policy`, `upsert_policy`,
//! `list_retention_rules`, `get_retention_rule`, `insert_retention_rule`,
//! `delete_retention_rule`).
//!
//! P2-M4 adds transactional audit recording. Every mutating method that runs
//! (or wraps) a DB transaction inserts an [`AuditEntry`] row in the **same**
//! transaction, guaranteeing 100% coverage with no silent drops.
//!
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness
//!
//! ## Accepted Henry-Kafura hub (do not fragment further)
//!
//! `Store` is the **single unit-of-work persistence facade** — the one type that
//! holds the `DBProvider` and drives connections/transactions. The transaction
//! boundary is the natural seam, so the facade itself is deliberately kept whole
//! rather than fragmented into per-context store slices (a cross-cutting flow
//! such as a multipart *complete* touches files, versions, the multipart
//! session, and the audit outbox in one transaction, so slicing would only
//! relocate the crossroads).
//!
//! Two structural remedies keep its Henry-Kafura coupling in check without
//! fragmenting the transaction logic:
//!
//! - **fan-out** collapses the nine repositories behind a single [`Repos`]
//!   aggregate (the DIP remedy), and takes the repo-layer row / param types
//!   (`AuditRow`, `FileEventRow`, `InsertRetentionRule`) via the `repo` facade
//!   rather than reaching into `entity::*` or individual repo submodules.
//! - **fan-in** is held down by the domain-owned capability ports in
//!   [`crate::domain::ports`] ([`CleanupStore`](crate::domain::ports::CleanupStore),
//!   [`MultipartStore`](crate::domain::ports::MultipartStore)): narrow consumers
//!   depend on the segregated trait they actually use (ISP), so only `FileService`
//!   and the composition root (`gear`) name the concrete `Store`. New background
//!   or bounded-context consumers should take a port, not the concrete facade.

// Domain terms (ETag, If-Match) appear in the module docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use hex;
use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{
    CustomMetadataEntry, CustomMetadataPatch, File, FileVersion, NewFile, OwnerFilter,
    VersionStatus,
};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::idempotency::IdempotencyRecord;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::domain::policy::{
    PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope, StoredPolicy, StoredRetentionRule,
};
use crate::infra::content::hash;
use crate::infra::storage::db::db_err;
use crate::infra::storage::repo::{AuditRow, FileEventRow, InsertRetentionRule, Repos};

/// An idempotency-key row to persist in the **same** transaction as a file
/// creation, so a committed `POST /files` always leaves a replay record behind
/// (no window where the file exists but the key does not).
pub struct IdempotencyInsert {
    pub tenant_id: Uuid,
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub key: String,
    pub response_status: i32,
    pub response_body: String,
    pub response_etag: String,
    pub expires_at: OffsetDateTime,
}

/// Persistence facade — the only type that holds `DBProvider` and drives
/// transactions. Cheap to clone (an `Arc` + a bundle of unit-struct repos).
///
/// The repositories are held together in a single [`Repos`] aggregate rather
/// than as nine separate fields, so this module depends on one collaborator
/// instead of naming every repo type — the coupling to the individual repo
/// modules lives on `Repos`, not on this crossroads.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
pub struct Store {
    db: Arc<DBProvider<DbError>>,
    repos: Repos,
}

impl Store {
    /// Construct a `Store` from the shared `DBProvider`.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>) -> Self {
        Self {
            db,
            repos: Repos::default(),
        }
    }

    // ── file queries ─────────────────────────────────────────────────────────

    /// Fetch a file by `(scope, file_id)`. Returns `None` when absent.
    pub async fn get_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Option<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.files.get(&conn, scope, file_id).await
    }

    /// Like [`get_file`] but errors with `FileNotFound` when absent.
    pub async fn require_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        self.get_file(scope, file_id)
            .await?
            .ok_or_else(|| DomainError::file_not_found(file_id))
    }

    /// List files for an owner filter, newest-first, offset-paginated.
    pub async fn list_files(
        &self,
        scope: &AccessScope,
        owner: OwnerFilter,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .files
            .list(&conn, scope, owner, limit, offset)
            .await
    }

    /// Delete a file row (FK cascade removes versions + custom metadata) and
    /// write an audit row — both in a single transaction.
    ///
    /// Returns `true` if a row was removed.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let del_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = files.delete(tx, &del_scope, file_id).await?;
                    if removed {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    // ── create ───────────────────────────────────────────────────────────────

    /// Insert a new file row + a pending version row + any initial custom-
    /// metadata entries in ONE transaction, so a failure partway through cannot
    /// leave a visible file with no version (or partial metadata) behind.
    ///
    /// An audit row is written in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_with_pending_version(
        &self,
        new: &NewFile,
        file_id: Uuid,
        version_id: Uuid,
        tenant_id: Uuid,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
        audit: AuditEntry,
    ) -> Result<(), DomainError> {
        let file = File {
            file_id,
            tenant_id,
            owner_kind: new.owner_kind,
            owner_id: new.owner_id,
            name: new.name.clone(),
            gts_file_type: new.gts_file_type.clone(),
            content_id: None,
            meta_version: 0,
            created_at: now,
            last_modified_at: now,
        };
        let pending = pending_version(
            file_id,
            version_id,
            &new.mime_type,
            backend_id,
            backend_path,
            now,
        );
        // Own the initial metadata entries so the transaction closure can move them.
        let metadata_entries: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();

        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    files.create(tx, &AccessScope::allow_all(), &file).await?;
                    versions
                        .insert(tx, &AccessScope::allow_all(), &pending)
                        .await?;
                    for (key, value) in &metadata_entries {
                        metadata
                            .upsert(tx, &AccessScope::allow_all(), file_id, key, value, now)
                            .await?;
                    }
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    // ── version management ───────────────────────────────────────────────────

    /// Insert a pending version row (for `presign_version`).
    pub async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        let pending = pending_version(
            file_id,
            version_id,
            mime_type,
            backend_id,
            backend_path,
            now,
        );
        self.repos
            .versions
            .insert(&conn, &AccessScope::allow_all(), &pending)
            .await
    }

    /// Fetch a single version by `(file_id, version_id)`.
    pub async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .get(&conn, &AccessScope::allow_all(), file_id, version_id)
            .await
    }

    /// List all versions of a file, newest first.
    pub async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_by_file(&conn, &AccessScope::allow_all(), file_id)
            .await
    }

    /// Return the MIME type of the file's current (bound) version, if any.
    /// `Ok(None)` means there is genuinely no bound content; a DB/connection
    /// failure is propagated as `Err` (never silently treated as "no mime").
    pub async fn current_version_mime(&self, file: &File) -> Result<Option<String>, DomainError> {
        let Some(content_id) = file.content_id else {
            return Ok(None);
        };
        Ok(self
            .get_version(file.file_id, content_id)
            .await?
            .map(|v| v.mime_type))
    }

    /// Record a version's size + hash and mark it `available`.
    /// Returns `true` if the version row existed and was updated.
    ///
    /// An audit row is written in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = versions
                        .finalize(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            version_id,
                            size,
                            hash_value,
                        )
                        .await?;
                    if updated {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Delete a single version row and record an audit row in the same
    /// transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = versions
                        .delete(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    if removed {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    // ── custom metadata ──────────────────────────────────────────────────────

    /// List all custom-metadata entries for a file, ordered by key.
    pub async fn list_metadata(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .metadata
            .list(&conn, &AccessScope::allow_all(), file_id)
            .await
    }

    // ── atomic multi-step operations ─────────────────────────────────────────

    /// Swap the content pointer + promote `version_id` as current, in a single
    /// transaction (the bind CAS — DESIGN §3.7). An audit row is written in the
    /// same transaction on a successful swap.
    ///
    /// The `scope` used for the CAS update must be the authorized scope
    /// (returned by the authorizer); the `is_current` flip uses
    /// `allow_all()` because the version row has no tenant column and the
    /// parent file was already checked.
    ///
    /// Returns `true` on a successful swap, `false` on a concurrent CAS
    /// conflict (caller maps to 412 PreconditionFailed).
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn bind_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_content_id: Option<Uuid>,
        version_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let bind_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let swapped = files
                        .bind_content_cas(
                            tx,
                            &bind_scope,
                            file_id,
                            expected_content_id,
                            version_id,
                            now,
                        )
                        .await?;
                    if !swapped {
                        return Ok(false);
                    }
                    // Promote the new version as current (unique-current index honoured).
                    versions
                        .clear_current(tx, &AccessScope::allow_all(), file_id)
                        .await?;
                    versions
                        .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    /// Bump `meta_version` and apply a JSON-merge patch, in a single
    /// transaction (DESIGN §3.7 metadata CAS). An audit row is written in the
    /// same transaction on a successful patch.
    ///
    /// Returns `false` when `expected_meta_version` does not match the current
    /// row (caller maps to 412 PreconditionFailed with "metadata revision
    /// changed concurrently").
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn patch_metadata_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_meta_version: Option<i64>,
        patch: CustomMetadataPatch,
        now: OffsetDateTime,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        let patch_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let bumped = files
                        .touch_meta(tx, &patch_scope, file_id, expected_meta_version, now)
                        .await?;
                    if !bumped {
                        return Ok(false);
                    }
                    for (key, value) in &patch.entries {
                        match value {
                            Some(v) => {
                                metadata
                                    .upsert(tx, &AccessScope::allow_all(), file_id, key, v, now)
                                    .await?;
                            }
                            None => {
                                metadata
                                    .delete_key(tx, &AccessScope::allow_all(), file_id, key)
                                    .await?;
                            }
                        }
                    }
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    // ── policy store (P2-M1) ─────────────────────────────────────────────────

    /// Fetch the policy for a given `(policy_scope, scope_owner_id)` within a
    /// tenant. Returns `None` when no policy has been configured for that scope.
    pub async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .policies
            .get(&conn, scope, tenant_id, policy_scope, scope_owner_id)
            .await
    }

    /// Upsert (replace) the policy for a given `(policy_scope, scope_owner_id)`.
    /// Returns the new `policy_id`.
    pub async fn upsert_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .policies
            .upsert(
                &conn,
                scope,
                tenant_id,
                policy_scope,
                scope_owner_id,
                body,
                now,
            )
            .await
    }

    /// List all retention rules for a tenant (all scopes).
    pub async fn list_retention_rules(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_for_tenant(&conn, scope, tenant_id)
            .await
    }

    /// Fetch a single retention rule by `rule_id`.
    pub async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.retention_rules.get(&conn, scope, rule_id).await
    }

    /// Insert a new retention rule. Returns the assigned `rule_id`.
    pub async fn insert_retention_rule(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .insert(
                &conn,
                scope,
                InsertRetentionRule {
                    tenant_id,
                    retention_scope,
                    scope_target_id,
                    body,
                    now,
                },
            )
            .await
    }

    /// Delete a retention rule by `rule_id`. Returns `true` if a row was removed.
    pub async fn delete_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .delete(&conn, scope, rule_id)
            .await
    }

    // ── multipart uploads (P2-M3) ─────────────────────────────────────────────

    /// Create a multipart upload session row.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    #[allow(clippy::too_many_arguments)]
    pub async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        declared_mime: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .create(
                &conn,
                upload_id,
                file_id,
                version_id,
                backend_upload_handle,
                declared_mime,
                expires_at,
                now,
            )
            .await
    }

    /// Fetch a multipart upload session by `upload_id`.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    pub async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.get(&conn, upload_id).await
    }

    /// Insert or replace a multipart upload part.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .upsert_part(
                &conn,
                upload_id,
                part_number,
                backend_etag,
                part_hash,
                size,
                now,
            )
            .await
    }

    /// List all parts for a multipart upload.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    pub async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<MultipartPart>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.list_parts(&conn, upload_id).await
    }

    /// Mark a multipart upload session as `completed` and record the audit row
    /// in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = multipart
                        .update_state(tx, upload_id, "in_progress", "completed")
                        .await?;
                    if updated {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Mark a multipart upload session as `aborted` and record the audit row
    /// in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = multipart
                        .update_state(tx, upload_id, "in_progress", "aborted")
                        .await?;
                    if updated {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    // ── idempotency keys (P2-M3) ──────────────────────────────────────────────

    /// Fetch an idempotency record if it exists and has not expired.
    ///
    /// @cpt-cf-file-storage-fr-upload-idempotency
    pub async fn get_idempotency_key(
        &self,
        tenant_id: Uuid,
        owner_kind: &str,
        owner_id: Uuid,
        key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<IdempotencyRecord>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .idempotency_keys
            .get(&conn, tenant_id, owner_kind, owner_id, key, now)
            .await
    }

    // ── audit outbox (P2-M4) ──────────────────────────────────────────────────

    /// List audit rows for a specific file, ordered by occurrence time.
    ///
    /// Intended for testing; not exposed on the REST API.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn list_audit(&self, file_id: Uuid) -> Result<Vec<AuditRow>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.audit.list_for_file(&conn, file_id).await
    }

    // ── cleanup engine (P2-M4 lifecycle) ─────────────────────────────────────

    /// List all `pending` version rows older than `older_than` (system scope).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    pub async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_pending_older_than(&conn, &AccessScope::allow_all(), older_than)
            .await
    }

    /// List all non-current version rows older than `older_than` (system scope).
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_non_current_versions_older_than(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_non_current_older_than(&conn, &AccessScope::allow_all(), older_than)
            .await
    }

    /// List all `in_progress` multipart sessions whose `expires_at` is before `now`.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    pub async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.list_expired(&conn, now).await
    }

    /// Verify that `blob` matches `expected_hash` (SHA-256).
    ///
    /// Returns `Ok(())` on a match; `Err(DomainError::hash_mismatch)` on a
    /// digest mismatch. The hash computation is confined here because this
    /// module already owns the SHA-256 allow-list usage (see `hash.rs` docs),
    /// keeping `FileService` free of a direct `hash` import.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub fn verify_content_hash(blob: &[u8], expected_hash: &[u8]) -> Result<(), DomainError> {
        let computed = hash::sha256(blob);
        if computed != expected_hash {
            return Err(DomainError::hash_mismatch(
                hex::encode(expected_hash),
                hex::encode(&computed),
            ));
        }
        Ok(())
    }

    /// Transactionally update `backend_id` and `backend_path` for a version row,
    /// and write a `BackendMigrate` audit row in the same transaction.
    ///
    /// Returns `true` if the version row was found and updated.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub async fn rebind_version_backend(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        new_backend_id: &str,
        new_backend_path: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let new_backend_id = new_backend_id.to_owned();
        let new_backend_path = new_backend_path.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = versions
                        .rebind_backend(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            version_id,
                            &new_backend_id,
                            &new_backend_path,
                        )
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    // ── ownership transfer (P2-M5) ────────────────────────────────────────────

    /// Update `owner_kind` + `owner_id` for a file, enqueue an optional event
    /// row, and record an audit row — all in one transaction.
    ///
    /// Returns `true` if the file row was found and updated.
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    /// @cpt-cf-file-storage-fr-file-events
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer_ownership_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        new_owner_kind: &str,
        new_owner_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let transfer_scope = scope.clone();
        let new_owner_kind = new_owner_kind.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = files
                        .update_owner(
                            tx,
                            &transfer_scope,
                            file_id,
                            &new_owner_kind,
                            new_owner_id,
                            now,
                        )
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                        if let Some(ev) = event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    // ── file-events outbox (P2-M5) ────────────────────────────────────────────

    /// Delete a file row (FK cascade removes versions + custom metadata),
    /// optionally enqueue a file-event, and write an audit row — all in a
    /// single transaction.
    ///
    /// Returns `true` if a row was removed.
    ///
    /// This is the events-aware variant of [`delete_file`]; the original method
    /// is preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let del_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = files.delete(tx, &del_scope, file_id).await?;
                    if removed {
                        audit_repo.insert(tx, &audit).await?;
                        if let Some(ev) = event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    /// Swap the content pointer + promote `version_id` as current, optionally
    /// enqueue a file-event — all in a single transaction.
    ///
    /// This is the events-aware variant of [`bind_atomic`]; the original is
    /// preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_atomic_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_content_id: Option<Uuid>,
        version_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let bind_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let swapped = files
                        .bind_content_cas(
                            tx,
                            &bind_scope,
                            file_id,
                            expected_content_id,
                            version_id,
                            now,
                        )
                        .await?;
                    if !swapped {
                        return Ok(false);
                    }
                    versions
                        .clear_current(tx, &AccessScope::allow_all(), file_id)
                        .await?;
                    versions
                        .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    /// Create a new file + pending version + initial metadata + optional event,
    /// all in one transaction.
    ///
    /// This is the events-aware variant of [`create_file_with_pending_version`];
    /// the original is preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_with_pending_version_and_event(
        &self,
        new: &NewFile,
        file_id: Uuid,
        version_id: Uuid,
        tenant_id: Uuid,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
        idempotency: Option<IdempotencyInsert>,
    ) -> Result<(), DomainError> {
        let file = File {
            file_id,
            tenant_id,
            owner_kind: new.owner_kind,
            owner_id: new.owner_id,
            name: new.name.clone(),
            gts_file_type: new.gts_file_type.clone(),
            content_id: None,
            meta_version: 0,
            created_at: now,
            last_modified_at: now,
        };
        let pending = pending_version(
            file_id,
            version_id,
            &new.mime_type,
            backend_id,
            backend_path,
            now,
        );
        let metadata_entries: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();

        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let idempotency_repo = self.repos.idempotency_keys.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    files.create(tx, &AccessScope::allow_all(), &file).await?;
                    versions
                        .insert(tx, &AccessScope::allow_all(), &pending)
                        .await?;
                    for (key, value) in &metadata_entries {
                        metadata
                            .upsert(tx, &AccessScope::allow_all(), file_id, key, value, now)
                            .await?;
                    }
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    // Persist the idempotency record in the same transaction, so
                    // a committed create always has a replay record. A PK
                    // conflict (concurrent duplicate) is tolerated inside the
                    // repo; any real DB error rolls the whole creation back.
                    if let Some(idem) = idempotency {
                        idempotency_repo
                            .insert(
                                tx,
                                idem.tenant_id,
                                &idem.owner_kind,
                                idem.owner_id,
                                &idem.key,
                                file_id,
                                idem.response_status,
                                &idem.response_body,
                                &idem.response_etag,
                                idem.expires_at,
                                now,
                            )
                            .await?;
                    }
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    /// List file-event rows for a specific file ordered by occurrence time.
    ///
    /// Intended for testing; not exposed on the REST API.
    ///
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn list_file_events(&self, file_id: Uuid) -> Result<Vec<FileEventRow>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.events_outbox.list_for_file(&conn, file_id).await
    }

    /// List files across all tenants for the retention sweep, keyset-paginated
    /// by `file_id` (see [`FileRepo::list_all_for_sweep`]). `after = None` starts
    /// from the beginning; the caller loops until it gets fewer than `limit`.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .files
            .list_all_for_sweep(&conn, &AccessScope::allow_all(), after, limit)
            .await
    }

    /// List retention rules for a specific file (`scope = 'file'`), across all
    /// tenants. Used by the retention sweep engine.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_file_retention_rules(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_by_file_scope(&conn, &AccessScope::allow_all(), file_id)
            .await
    }

    /// List all retention rules across all tenants and scopes — for the sweep
    /// engine.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_all(&conn, &AccessScope::allow_all())
            .await
    }
}

// ── trait implementations ─────────────────────────────────────────────────────

use crate::domain::ports::{CleanupStore, MultipartStore};
use async_trait::async_trait;

#[async_trait]
impl CleanupStore for Store {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        Store::list_abandoned_pending_versions(self, older_than).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::delete_version(self, file_id, version_id, audit).await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<crate::domain::multipart::MultipartUploadSession>, DomainError> {
        Store::list_expired_multipart_uploads(self, now).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::abort_multipart_upload(self, upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        Store::get_version(self, file_id, version_id).await
    }

    async fn list_all_retention_rules(
        &self,
    ) -> Result<Vec<crate::domain::policy::StoredRetentionRule>, DomainError> {
        Store::list_all_retention_rules(self).await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<file_storage_sdk::File>, DomainError> {
        Store::list_all_files_for_sweep(self, after, limit).await
    }

    async fn list_metadata(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<file_storage_sdk::CustomMetadataEntry>, DomainError> {
        Store::list_metadata(self, file_id).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        Store::list_versions(self, file_id).await
    }

    async fn delete_file_with_event(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
        event: Option<crate::domain::audit::FileEvent>,
    ) -> Result<bool, DomainError> {
        Store::delete_file_with_event(self, scope, file_id, audit, event).await
    }
}

#[async_trait]
impl MultipartStore for Store {
    async fn require_file(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
    ) -> Result<file_storage_sdk::File, DomainError> {
        Store::require_file(self, scope, file_id).await
    }

    async fn get_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<crate::domain::policy::StoredPolicy>, DomainError> {
        Store::get_policy(self, scope, tenant_id, policy_scope, scope_owner_id).await
    }

    async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::insert_pending_version(
            self,
            file_id,
            version_id,
            mime_type,
            backend_id,
            backend_path,
            now,
        )
        .await
    }

    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        declared_mime: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::create_multipart_upload(
            self,
            upload_id,
            file_id,
            version_id,
            backend_upload_handle,
            declared_mime,
            expires_at,
            now,
        )
        .await
    }

    async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<crate::domain::multipart::MultipartUploadSession>, DomainError> {
        Store::get_multipart_upload(self, upload_id).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        Store::get_version(self, file_id, version_id).await
    }

    async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        Store::upsert_multipart_part(
            self,
            upload_id,
            part_number,
            backend_etag,
            part_hash,
            size,
            now,
        )
        .await
    }

    async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<crate::domain::multipart::MultipartPart>, DomainError> {
        Store::list_multipart_parts(self, upload_id).await
    }

    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::finalize_version(self, file_id, version_id, size, hash_value, audit).await
    }

    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::complete_multipart_upload(self, upload_id, audit).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::abort_multipart_upload(self, upload_id, audit).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: crate::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        Store::delete_version(self, file_id, version_id, audit).await
    }
}

#[async_trait]
impl crate::domain::ports::PolicyStore for Store {
    async fn get_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<crate::domain::policy::StoredPolicy>, DomainError> {
        Store::get_policy(self, scope, tenant_id, policy_scope, scope_owner_id).await
    }

    async fn upsert_policy(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        policy_scope: &crate::domain::policy::PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &crate::domain::policy::PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        Store::upsert_policy(
            self,
            scope,
            tenant_id,
            policy_scope,
            scope_owner_id,
            body,
            now,
        )
        .await
    }

    async fn list_retention_rules(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<crate::domain::policy::StoredRetentionRule>, DomainError> {
        Store::list_retention_rules(self, scope, tenant_id).await
    }

    async fn insert_retention_rule(
        &self,
        scope: &toolkit_security::AccessScope,
        tenant_id: Uuid,
        retention_scope: &crate::domain::policy::RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &crate::domain::policy::RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        Store::insert_retention_rule(
            self,
            scope,
            tenant_id,
            retention_scope,
            scope_target_id,
            body,
            now,
        )
        .await
    }

    async fn delete_retention_rule(
        &self,
        scope: &toolkit_security::AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        Store::delete_retention_rule(self, scope, rule_id).await
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a `pending` version row with placeholder size/hash (filled at finalize).
fn pending_version(
    file_id: Uuid,
    version_id: Uuid,
    mime_type: &str,
    backend_id: &str,
    backend_path: &str,
    now: OffsetDateTime,
) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: mime_type.to_owned(),
        size: 0,
        hash_algorithm: hash::ALGORITHM.to_owned(),
        // 32 zero bytes — satisfies the NOT NULL + length-32 CHECK until finalize.
        hash_value: vec![0u8; 32],
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        created_at: now,
    }
}

//! Custom-metadata queries and the atomic patch operation.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::CustomMetadataEntry;
use file_storage_sdk::CustomMetadataPatch;

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::Store;

impl Store {
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

    /// Batched counterpart of `list_metadata`: fetch custom-metadata entries
    /// for a page of files in one query, grouped by `file_id`. `GET /files`
    /// listing uses this instead of calling `list_metadata` once per file
    /// (an N+1 query pattern that would scale with the page size).
    pub async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .metadata
            .list_for_files(&conn, &AccessScope::allow_all(), file_ids)
            .await
    }

    // ── atomic multi-step operations ─────────────────────────────────────────

    /// Bump `meta_version` and apply a JSON-merge patch, in a single
    /// transaction (DESIGN §3.7 metadata CAS). An audit row is written in the
    /// same transaction on a successful patch, and an optional `FileEvent`
    /// (`file.metadata_updated`) is enqueued alongside it — the events-aware
    /// counterpart of the audit row, mirroring `bind_atomic_with_event` /
    /// `transfer_ownership_atomic`.
    ///
    /// Returns `false` when `expected_meta_version` does not match the current
    /// row (caller maps to PreconditionFailed with "metadata revision changed
    /// concurrently"; REST maps that canonical error to HTTP 400). No audit
    /// row and no event are written in that case.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn patch_metadata_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_meta_version: Option<i64>,
        patch: CustomMetadataPatch,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let patch_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let Some(new_meta_version) = files
                        .touch_meta(tx, &patch_scope, file_id, expected_meta_version, now)
                        .await?
                    else {
                        return Ok(false);
                    };
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
                    if let Some(mut ev) = event {
                        // Stamp the authoritative post-bump revision the CAS
                        // actually committed. The domain builds the event
                        // before the transaction and cannot know the committed
                        // value for an unconditional patch that raced another,
                        // so the `meta_version` payload field is filled here.
                        if let Some(obj) = ev.payload.as_object_mut() {
                            obj.insert(
                                "meta_version".to_owned(),
                                serde_json::Value::from(new_meta_version),
                            );
                        }
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }
}

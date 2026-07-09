//! Custom-metadata queries and the atomic patch operation.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::CustomMetadataEntry;
use file_storage_sdk::CustomMetadataPatch;

use crate::domain::audit::AuditEntry;
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

    // ── atomic multi-step operations ─────────────────────────────────────────

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
}

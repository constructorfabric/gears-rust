//! Unit-of-work persistence facade: the single touch-point for `toolkit_db`.
//!
//! `Store` owns the `DBProvider`, the tenant-scoped repositories (bundled in `Repos`)
//! and all connection/transaction logic, so nothing else opens a connection or a
//! transaction. It decides which `AccessScope` each table is queried with and handles
//! ETag/If-Match semantics; authorization decisions stay in `FileService`.
//!
//! Every mutating method that runs a transaction inserts its audit row in the
//! **same** transaction. Narrow consumers should depend on the domain ports
//! (`CleanupStore`, `MultipartStore`) rather than the concrete `Store`.
//!
//! The impl is split across sibling files (`files`, `versions`, `metadata`, `policy`,
//! `multipart`, `lifecycle`, `traits`) to keep each file small.

// Domain terms (ETag, If-Match) appear in the module docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::infra::content::hash;
use crate::infra::content::hash_mode::HashMode;
use crate::infra::storage::repo::Repos;

mod files;
mod lifecycle;
mod metadata;
mod multipart;
mod policy;
mod traits;
mod versions;

pub use crate::infra::storage::repo::{AuditRow, FileEventRow};

/// An idempotency-key row persisted in the **same** transaction as the file creation,
/// so a committed `POST /files` always leaves a replay record.
pub struct IdempotencyInsert {
    pub tenant_id: Uuid,
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub key: String,
    /// Subject creating this record; verified on replay so one caller's key never
    /// surfaces another caller's ticket.
    pub subject_id: Uuid,
    pub response_status: i32,
    pub response_body: String,
    pub response_etag: String,
    /// SHA-256 of the canonicalized request (`domain::idempotency::compute_request_hash`);
    /// compared on replay so a different body never surfaces a stored ticket.
    pub request_hash: Vec<u8>,
    pub expires_at: OffsetDateTime,
}

/// Persistence facade: the only type that holds `DBProvider` and drives
/// transactions. Cheap to clone.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone)]
pub struct Store {
    pub(super) db: Arc<DBProvider<DbError>>,
    pub(super) repos: Repos,
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

    /// Mode-aware content-hash verification.
    ///
    /// `whole-sha256`: `manifest` must be `None`; `sha256(blob)` must equal
    /// `hash_value`. `multipart-composite-sha256`: `manifest` is required; each part
    /// slice is re-hashed against the manifest, and the rebuilt manifest's root must
    /// equal `hash_value`.
    ///
    /// Returns `DomainError::hash_mismatch` on mismatch, a validation error for a
    /// malformed or absent manifest. Lives here to keep `hash` imports out of
    /// `FileService`.
    pub fn verify_content_hash(
        blob: &[u8],
        hash_mode: HashMode,
        hash_value: &[u8],
        manifest: Option<&str>,
    ) -> Result<(), crate::domain::error::DomainError> {
        use crate::domain::error::DomainError;
        match hash_mode {
            HashMode::WholeSha256 => {
                if manifest.is_some() {
                    return Err(DomainError::validation(
                        "manifest",
                        "whole-sha256 versions carry no manifest",
                    ));
                }
                let computed = hash::sha256(blob);
                if computed != hash_value {
                    return Err(DomainError::hash_mismatch(
                        hex::encode(hash_value),
                        hex::encode(&computed),
                    ));
                }
                Ok(())
            }
            HashMode::MultipartCompositeSha256 => {
                let manifest = manifest.ok_or_else(|| {
                    DomainError::validation(
                        "manifest",
                        "multipart-composite-sha256 verification requires the stored manifest",
                    )
                })?;
                Self::verify_multipart_composite(blob, hash_value, manifest)
            }
        }
    }

    /// Split-rehash-rebuild-compare for `multipart-composite-sha256`, derived from
    /// `blob` and the stored `manifest` alone.
    fn verify_multipart_composite(
        blob: &[u8],
        root: &[u8],
        manifest: &str,
    ) -> Result<(), crate::domain::error::DomainError> {
        use crate::domain::error::DomainError;
        use crate::infra::content::hash_mode::{Manifest, ManifestEntry};

        let parsed = Manifest::from_wire_string(manifest)?;
        let entries = parsed.entries();
        let blob_len = blob.len() as u64;

        let mut rebuilt = Vec::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            // Part spans [offset, next_offset); the final part runs to the end of the blob.
            let start = entry.offset;
            let end = entries.get(i + 1).map_or(blob_len, |next| next.offset);
            if start > end || end > blob_len {
                return Err(DomainError::hash_mismatch(
                    hex::encode(root),
                    format!("manifest offset {start} out of range for object of {blob_len} bytes"),
                ));
            }
            let slice = &blob[usize::try_from(start).unwrap_or(usize::MAX)
                ..usize::try_from(end).unwrap_or(usize::MAX)];
            let digest = hash::digest_to_array(hash::sha256(slice));
            if digest != entry.digest {
                return Err(DomainError::hash_mismatch(
                    hex::encode(entry.digest),
                    format!(
                        "recomputed part digest at offset {start}: {}",
                        hex::encode(digest)
                    ),
                ));
            }
            rebuilt.push(ManifestEntry {
                offset: entry.offset,
                digest,
            });
        }

        let rebuilt_root = Manifest::new(rebuilt)?.root();
        if rebuilt_root.as_slice() != root {
            return Err(DomainError::hash_mismatch(
                hex::encode(root),
                hex::encode(rebuilt_root),
            ));
        }
        Ok(())
    }
}

/// Build a `pending` version row with placeholder size/hash (filled at finalize).
pub(super) fn pending_version(
    file_id: Uuid,
    version_id: Uuid,
    mime_type: &str,
    backend_id: &str,
    backend_path: &str,
    now: OffsetDateTime,
) -> file_storage_sdk::FileVersion {
    use file_storage_sdk::VersionStatus;
    file_storage_sdk::FileVersion {
        file_id,
        version_id,
        mime_type: mime_type.to_owned(),
        size: 0,
        hash_algorithm: hash::ALGORITHM.to_owned(),
        // 32 zero bytes — satisfies the NOT NULL + length-32 CHECK until finalize.
        hash_value: vec![0u8; 32],
        // Mode is decided at finalize time, so a pending row defaults to `whole-sha256`.
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: None,
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        created_at: now,
    }
}

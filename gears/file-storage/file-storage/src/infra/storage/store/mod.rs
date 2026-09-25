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
//! Policy store intent-level methods (`get_policy`, `upsert_policy`,
//! `list_retention_rules`, `get_retention_rule`, `insert_retention_rule`,
//! `delete_retention_rule`) live alongside the rest.
//!
//! Every mutating method that runs (or wraps) a DB transaction inserts an
//! [`AuditEntry`] row in the **same** transaction, guaranteeing 100% coverage
//! with no silent drops.
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
//!
//! ## Module layout (path-split to stay ≤ 600 SLOC per file)
//!
//! The impl block is spread across sibling files; the type itself lives here:
//! - `files.rs`     — file queries, create, delete (with events)
//! - `versions.rs`  — version insert / finalize / delete / queries
//! - `metadata.rs`  — metadata list + patch_metadata_atomic
//! - `policy.rs`    — policy + retention-rule intent methods
//! - `multipart.rs` — multipart upload session methods
//! - `lifecycle.rs` — lifecycle/cleanup/sweep + idempotency key query
//! - `traits.rs`    — CleanupStore / MultipartStore / PolicyStore impls

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

/// An idempotency-key row to persist in the **same** transaction as a file
/// creation, so a committed `POST /files` always leaves a replay record behind
/// (no window where the file exists but the key does not).
pub struct IdempotencyInsert {
    pub tenant_id: Uuid,
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub key: String,
    /// The authenticated subject (`ctx.subject_id()`) creating this record —
    /// verified against on replay so one caller's key can never surface
    /// another caller's ticket.
    pub subject_id: Uuid,
    pub response_status: i32,
    pub response_body: String,
    pub response_etag: String,
    /// SHA-256 over `domain::idempotency::compute_request_hash`'s
    /// canonicalized encoding of the request — compared against on replay so
    /// a caller can never surface a stored ticket for a materially different
    /// request body.
    pub request_hash: Vec<u8>,
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
}

// ── helpers ──────────────────────────────────────────────────────────────────

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
        // ADR-0006: mode is decided at *finalize* time (a pending row does not
        // yet know whether the upload will complete single- or multi-part), so
        // a pending row defaults to `whole-sha256` / no part count. The
        // multipart-complete path overwrites `hash_mode`/`part_count` at
        // finalize via `finalize_version`.
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: None,
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        created_at: now,
        // A pending row has not been through finalize yet, let alone won a
        // bind CAS there.
        bound_on_finalize: false,
    }
}

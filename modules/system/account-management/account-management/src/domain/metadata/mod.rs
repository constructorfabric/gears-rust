//! Tenant-metadata domain module.
//!
//! Implements FEATURE `tenant-metadata` (see
//! `modules/system/account-management/docs/features/feature-tenant-metadata.md`).
//!
//! This module owns the storage seam ([`repo::MetadataRepo`]) and the
//! pure value types ([`MetadataRow`], [`UpsertOutcome`]) that the future
//! `MetadataService` (Phase 3) and `MetadataRepoImpl` (Phase 4) will
//! compose against.
//!
//! Layering (mirrors [`crate::domain::conversion`]):
//!
//! * [`MetadataRow`] / [`UpsertOutcome`] — pure value types projected by
//!   the repo trait. The row mirrors the `tenant_metadata` entity 1:1
//!   (`tenant_id`, `schema_uuid`, opaque `value`, `created_at`,
//!   `updated_at`); `UpsertOutcome` carries the discriminator the
//!   service layer maps to HTTP 200 / 201 in Phase 3.
//! * [`repo`] — the [`repo::MetadataRepo`] trait that the service layer
//!   talks to. The `SeaORM`-backed implementation lands in Phase 4
//!   under `crate::infra::storage::repo_impl::metadata`; an in-memory
//!   fake for unit tests lives under [`test_support`].
//!
//! No service / SDK / REST surface is wired in this module yet. Phase 3
//! introduces `MetadataService::{list, get, put, delete, resolve}`;
//! Phase 4 introduces `MetadataRepoImpl` and the cross-tenant cascade
//! hook used by `TenantRepoImpl::hard_delete_one` on `SQLite`.

use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use modkit_macros::domain_model;

pub mod registry;
pub mod repo;
pub mod schema_id;
pub mod service;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

/// One direct-on-tenant metadata entry.
///
/// Mirrors [`crate::infra::storage::entity::tenant_metadata::Model`]
/// column-for-column. The `value` column carries an opaque
/// GTS-validated payload; the storage entity types it as `Json` while
/// this domain model uses [`serde_json::Value`] so the service layer
/// (Phase 3) can pass payloads from `account-management-sdk::metadata`
/// without dragging the `SeaORM` `Json` newtype into the public surface.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MetadataRow {
    pub tenant_id: Uuid,
    pub schema_uuid: Uuid,
    pub value: Value,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Monotonic version surfaced through the optimistic-lock
    /// contract on `UpsertMetadataRequest::expected_version`. New
    /// rows start at `1`; every UPDATE bumps `current + 1`.
    pub version: i64,
}

/// Discriminated upsert result returned by
/// [`repo::MetadataRepo::upsert_for_tenant`].
///
/// The service layer (Phase 3) maps the discriminator onto HTTP 201 vs
/// 200 per FEATURE §3.3 / §6 AC line 393. Both arms carry the
/// post-upsert row snapshot so the handler can build the response body
/// without a follow-up `SELECT`.
#[domain_model]
#[derive(Debug, Clone)]
pub enum UpsertOutcome {
    /// The row did not exist before this call — maps to HTTP 201.
    Inserted(MetadataRow),
    /// The row already existed and was updated — maps to HTTP 200.
    Updated(MetadataRow),
}

impl UpsertOutcome {
    /// Borrow the post-upsert row snapshot regardless of arm.
    #[must_use]
    pub fn row(&self) -> &MetadataRow {
        match self {
            Self::Inserted(row) | Self::Updated(row) => row,
        }
    }

    /// Convert into the post-upsert row, dropping the insert/update
    /// discriminator. Useful for unit tests that only need to assert on
    /// the column shape.
    #[must_use]
    pub fn into_row(self) -> MetadataRow {
        match self {
            Self::Inserted(row) | Self::Updated(row) => row,
        }
    }

    /// Returns `true` iff the upsert created a new row.
    #[must_use]
    pub const fn was_inserted(&self) -> bool {
        matches!(self, Self::Inserted(_))
    }
}

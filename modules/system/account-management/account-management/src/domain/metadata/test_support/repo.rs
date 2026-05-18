//! In-memory [`FakeMetadataRepo`] covering the
//! [`crate::domain::metadata::repo::MetadataRepo`] trait contract.
//!
//! Production semantics this fake mirrors:
//!
//! * Composite PK `(tenant_id, schema_uuid)` — at most one row per
//!   pair. `upsert_for_tenant` rewrites in place when the key exists
//!   and inserts otherwise; the discriminator carried by
//!   [`UpsertOutcome`] reflects which path ran.
//! * `created_at` is stamped only on insert. `updated_at` is stamped on
//!   every upsert (insert or update). On insert both timestamps are
//!   equal to the supplied `now`.
//! * `delete_for_tenant` is non-idempotent on missing rows: returns
//!   [`DomainError::MetadataEntryNotFound`] when the `(tenant_id,
//!   schema_uuid)` pair does not exist, mirroring the SQL impl
//!   contract.
//! * `delete_all_for_tenant` removes every row for `tenant_id` and
//!   returns the count, matching the cascade-hook seam used by the
//!   `SQLite` tenant hard-delete path.
//!
//! State is stored behind `Arc<Mutex<…>>` so the fake is `Clone + Send +
//! Sync` and can be shared across tasks the way `FakeConversionRepo` is.

#![allow(
    dead_code,
    reason = "test-support fake; not every public helper has a caller yet, later phases add service-level test sites"
)]
#![allow(
    clippy::must_use_candidate,
    reason = "test-support fake; every constructor/getter is intended for ad-hoc test wiring, the compiler nag is noise"
)]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    reason = "test-support fake; doc-equivalent error/panic semantics live on the trait this impl mirrors"
)]
#![allow(
    clippy::new_without_default,
    reason = "test-support fake: explicit `new()` is the canonical entry point; a Default impl would only obscure that"
)]
#![allow(
    clippy::module_name_repetitions,
    reason = "FakeMetadataRepo follows the FakeConversionRepo / FakeTenantRepo naming pattern"
)]
#![allow(
    clippy::expect_used,
    reason = "test-support fake; mutex `lock().expect(\"lock\")` is the canonical pattern, panics on poisoned mutex are acceptable in fakes"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use modkit_odata::{ODataQuery, Page, PageInfo};
use modkit_security::AccessScope;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::{MetadataRow, UpsertOutcome};

/// In-memory state shared behind `Arc<Mutex<…>>`.
///
/// Keyed by `(tenant_id, schema_uuid)` — the same composite PK the
/// production table uses. A nested map per tenant would buy O(1) bulk
/// `delete_all_for_tenant` but at the cost of doubling lookups for
/// every other method; the flat layout is faster for the common case
/// and the bulk delete is still O(n) over a small fixture.
struct State {
    rows: HashMap<(Uuid, Uuid), MetadataRow>,
}

impl State {
    fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }
}

/// Cloneable test repo that satisfies [`MetadataRepo`].
///
/// `Clone` only clones the `Arc`, so multiple cloned handles share the
/// same `State`. This matches the production `Arc<dyn MetadataRepo>`
/// shape used by the service layer in later phases.
#[derive(Clone)]
pub struct FakeMetadataRepo {
    inner: Arc<Mutex<State>>,
}

impl FakeMetadataRepo {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::new())),
        }
    }

    /// Build a fake pre-populated with `rows`. Duplicate
    /// `(tenant_id, schema_uuid)` keys in the seed input are
    /// last-write-wins per `HashMap::insert` semantics.
    pub fn with_seed(rows: Vec<MetadataRow>) -> Self {
        let repo = Self::new();
        {
            let mut state = repo.inner.lock().expect("lock");
            for row in rows {
                state.rows.insert((row.tenant_id, row.schema_uuid), row);
            }
        }
        repo
    }

    /// Snapshot every row currently held by the fake. Used by tests
    /// that assert on bulk-delete side effects without going through
    /// the trait `list_for_tenant` method.
    pub fn snapshot_all(&self) -> Vec<MetadataRow> {
        self.inner
            .lock()
            .expect("lock")
            .rows
            .values()
            .cloned()
            .collect()
    }
}

#[async_trait]
impl MetadataRepo for FakeMetadataRepo {
    async fn list_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataRow>, DomainError> {
        let state = self.inner.lock().expect("lock");
        let mut all: Vec<MetadataRow> = state
            .rows
            .values()
            .filter(|r| r.tenant_id == tenant_id)
            .cloned()
            .collect();
        // Stable order on `schema_uuid` so cursor re-reads are
        // deterministic. The production `MetadataRepoImpl` routes
        // through `modkit_db::odata::paginate_odata` which honors
        // `query.order` + tiebreaker; this fake fixture mirrors the
        // tiebreaker only — the `updated_at` order is best-effort here
        // since the fake's intended use is integration coverage, not
        // ordering edge cases.
        all.sort_by_key(|r| r.schema_uuid);

        // Honor `query.limit` (default to 50 — same as the SDK's
        // `DEFAULT_TOP` used by the tenant-CRUD listings). Cursor
        // tokens are not parsed by the fake — production cursor
        // mechanics live in `paginate_odata`; service-level unit tests
        // assert on the first page only.
        let limit_u64 = query.limit.unwrap_or(50);
        let take_n = usize::try_from(limit_u64).unwrap_or(usize::MAX);
        let rows: Vec<MetadataRow> = all.into_iter().take(take_n).collect();
        Ok(Page {
            items: rows,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: limit_u64,
            },
        })
    }

    async fn get_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<Option<MetadataRow>, DomainError> {
        let state = self.inner.lock().expect("lock");
        Ok(state.rows.get(&(tenant_id, schema_uuid)).cloned())
    }

    async fn upsert_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
        value: Value,
        now: OffsetDateTime,
        expected_version: Option<i64>,
    ) -> Result<UpsertOutcome, DomainError> {
        let mut state = self.inner.lock().expect("lock");
        let key = (tenant_id, schema_uuid);
        if let Some(existing) = state.rows.get_mut(&key) {
            // Optimistic-lock precondition (mirrors production: see
            // `repo_impl::metadata::upsert_for_tenant_once`).
            if let Some(expected) = expected_version
                && existing.version != expected
            {
                return Err(DomainError::MetadataVersionMismatch {
                    entry: format!("({tenant_id}, {schema_uuid})"),
                    expected,
                    current: existing.version,
                });
            }
            // Update path: preserve `created_at`, advance
            // `updated_at`, rewrite the opaque value, bump version.
            existing.value = value;
            existing.updated_at = now;
            existing.version += 1;
            let snap = existing.clone();
            Ok(UpsertOutcome::Updated(snap))
        } else {
            // Missing-row precondition: a non-zero `expected_version`
            // implies the caller thought a row existed.
            if let Some(expected) = expected_version
                && expected != 0
            {
                return Err(DomainError::MetadataVersionMismatch {
                    entry: format!("({tenant_id}, {schema_uuid})"),
                    expected,
                    current: 0,
                });
            }
            // Insert path: stamp `created_at == updated_at == now`,
            // seed `version = 1`.
            let row = MetadataRow {
                tenant_id,
                schema_uuid,
                value,
                created_at: now,
                updated_at: now,
                version: 1,
            };
            state.rows.insert(key, row.clone());
            Ok(UpsertOutcome::Inserted(row))
        }
    }

    async fn delete_for_tenant(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<(), DomainError> {
        let mut state = self.inner.lock().expect("lock");
        let key = (tenant_id, schema_uuid);
        if state.rows.remove(&key).is_some() {
            Ok(())
        } else {
            Err(DomainError::MetadataEntryNotFound {
                detail: format!("no metadata entry for tenant {tenant_id} at schema {schema_uuid}"),
                entry: format!("({tenant_id}, {schema_uuid})"),
            })
        }
    }
}

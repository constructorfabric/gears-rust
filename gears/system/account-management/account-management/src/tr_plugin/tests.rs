//! Unit tests for the `tr_plugin` gear.
//!
//! Each test builds a fresh in-memory `FakePort` (a pure-Rust
//! `TenantHierarchyReadPort` backed by two `HashMap`s) and exercises
//! `PluginImpl` through the `TenantResolverPluginClient` trait.
//!
//! The two registry stubs (`TestRegistry { fail: false/true }`) cover
//! every call-site the plugin makes against `TypesRegistryClient`:
//! `get_type_schema_by_uuid` (single) and `get_type_schemas_by_uuid`
//! (batch). All other trait methods panic — if the plugin ever calls
//! them unexpectedly the test fails loudly.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    dead_code
)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetDescendantsOptions, GetTenantsOptions, IsAncestorOptions,
    TenantId, TenantResolverError, TenantResolverPluginClient, TenantStatus as SdkStatus,
};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::models::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery,
};
use types_registry_sdk::testing::make_test_type_schema;
use uuid::Uuid;

use super::PluginImpl;
use crate::domain::error::DomainError;
use crate::domain::tenant::hierarchy_read_port::{
    BarrierMode as PortBarrierMode, StatusFilter, TenantHierarchyReadPort,
};
use crate::domain::tenant::model::{TenantModel, TenantStatus};

// ── Status constants (domain enum aliases, replacing the i16 SMALLINTs) ──

const PROVISIONING: TenantStatus = TenantStatus::Provisioning;
const ACTIVE: TenantStatus = TenantStatus::Active;
const SUSPENDED: TenantStatus = TenantStatus::Suspended;
const DELETED: TenantStatus = TenantStatus::Deleted;

// ── Registry stubs ────────────────────────────────────────────────────────

const TEST_TYPE_ID: &str = "gts.cf.core.test.tenant.v1~";

/// Dual-mode stub: when `fail=false` it returns a fixed `GtsTypeSchema` for
/// any UUID; when `fail=true` every lookup returns `CanonicalError::NotFound`.
struct TestRegistry {
    fail: bool,
}

impl TestRegistry {
    fn ok() -> Arc<dyn TypesRegistryClient> {
        Arc::new(Self { fail: false })
    }

    fn fail() -> Arc<dyn TypesRegistryClient> {
        Arc::new(Self { fail: true })
    }
}

#[async_trait]
impl TypesRegistryClient for TestRegistry {
    async fn get_type_schema_by_uuid(
        &self,
        _uuid: Uuid,
    ) -> std::result::Result<GtsTypeSchema, CanonicalError> {
        if self.fail {
            Err(types_registry_sdk::testing::not_found("test"))
        } else {
            Ok(make_test_type_schema(TEST_TYPE_ID))
        }
    }

    async fn get_type_schemas_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, std::result::Result<GtsTypeSchema, CanonicalError>> {
        uuids
            .into_iter()
            .map(|u| {
                let res = if self.fail {
                    Err(types_registry_sdk::testing::not_found("test"))
                } else {
                    Ok(make_test_type_schema(TEST_TYPE_ID))
                };
                (u, res)
            })
            .collect()
    }

    async fn register(
        &self,
        _: Vec<serde_json::Value>,
    ) -> std::result::Result<Vec<RegisterResult>, CanonicalError> {
        unimplemented!("tr_plugin does not call register")
    }

    async fn register_type_schemas(
        &self,
        _: Vec<serde_json::Value>,
    ) -> std::result::Result<Vec<RegisterResult>, CanonicalError> {
        unimplemented!("tr_plugin does not call register_type_schemas")
    }

    async fn get_type_schema(&self, _: &str) -> std::result::Result<GtsTypeSchema, CanonicalError> {
        unimplemented!("tr_plugin does not call get_type_schema by string id")
    }

    async fn get_type_schemas(
        &self,
        _: Vec<String>,
    ) -> HashMap<String, std::result::Result<GtsTypeSchema, CanonicalError>> {
        unimplemented!("tr_plugin does not call get_type_schemas by string ids")
    }

    async fn list_type_schemas(
        &self,
        _: TypeSchemaQuery,
    ) -> std::result::Result<Vec<GtsTypeSchema>, CanonicalError> {
        unimplemented!("tr_plugin does not call list_type_schemas")
    }

    async fn register_instances(
        &self,
        _: Vec<serde_json::Value>,
    ) -> std::result::Result<Vec<RegisterResult>, CanonicalError> {
        unimplemented!("tr_plugin does not call register_instances")
    }

    async fn get_instance(&self, _: &str) -> std::result::Result<GtsInstance, CanonicalError> {
        unimplemented!("tr_plugin does not call get_instance")
    }

    async fn get_instance_by_uuid(
        &self,
        _: Uuid,
    ) -> std::result::Result<GtsInstance, CanonicalError> {
        unimplemented!("tr_plugin does not call get_instance_by_uuid")
    }

    async fn get_instances(
        &self,
        _: Vec<String>,
    ) -> HashMap<String, std::result::Result<GtsInstance, CanonicalError>> {
        unimplemented!("tr_plugin does not call get_instances")
    }

    async fn get_instances_by_uuid(
        &self,
        _: Vec<Uuid>,
    ) -> HashMap<Uuid, std::result::Result<GtsInstance, CanonicalError>> {
        unimplemented!("tr_plugin does not call get_instances_by_uuid")
    }

    async fn list_instances(
        &self,
        _: InstanceQuery,
    ) -> std::result::Result<Vec<GtsInstance>, CanonicalError> {
        unimplemented!("tr_plugin does not call list_instances")
    }
}

// ── In-memory FakePort ────────────────────────────────────────────────────

/// In-memory `TenantHierarchyReadPort` for unit-testing `PluginImpl`
/// without touching `SeaORM`. Rows live in two `HashMap`s wrapped in
/// `Mutex` so seed helpers can mutate after construction.
///
/// The fake mirrors the production adapter's semantics:
/// - `get` filters out provisioning rows.
/// - `get_root` returns up to 2 non-provisioning rows ordered by id ASC.
/// - `get_bulk` deduplicates ids, applies the `StatusFilter`, and excludes
///   provisioning.
/// - `get_ancestors` / `get_descendants` exclude the self-row at the
///   "DB" level (here: a `HashMap` predicate), with optional barrier.
/// - `is_ancestor` is a plain `(ancestor, descendant)` key probe with
///   optional barrier.
///
/// Seeders are sync `&self` methods (no `&mut self` so Arc-wrapping
/// remains trivial). They take `TenantStatus` enum and `u32` depth
/// directly — the SMALLINT/`i16`/`i32` conversions of the old `SeaORM`
/// seeders are unnecessary here.
struct FakePort {
    rows: Mutex<HashMap<Uuid, TenantModel>>,
    /// Key: `(ancestor, descendant)`. Value: `barrier` (0 = not barrier,
    /// 1 = barrier-marked). `descendant_status` is intentionally NOT
    /// modeled here — the adapter never reads it from the closure
    /// table on the read paths exercised by these tests; AM's writer
    /// stamps it for downstream uses we don't simulate.
    closure: Mutex<HashMap<(Uuid, Uuid), i16>>,
}

impl FakePort {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(HashMap::new()),
            closure: Mutex::new(HashMap::new()),
        })
    }

    /// Seed one tenant row. `depth` is the root-relative depth
    /// (0 for root, 1 for direct child, etc.) — matches AM's
    /// `tenants.depth` semantics.
    fn insert_tenant(&self, id: Uuid, parent_id: Option<Uuid>, status: TenantStatus, depth: u32) {
        let now = OffsetDateTime::now_utc();
        let deleted_at = matches!(status, TenantStatus::Deleted).then_some(now);
        let row = TenantModel {
            id,
            parent_id,
            name: format!("test-{id}"),
            status,
            self_managed: false,
            tenant_type_uuid: Uuid::nil(),
            depth,
            created_at: now,
            updated_at: now,
            deleted_at,
        };
        self.rows.lock().unwrap().insert(id, row);
    }

    /// Seed one closure row. `barrier` is the i16 stored in
    /// `tenant_closure.barrier` (0 = clear, 1 = `self_managed`
    /// boundary).
    fn insert_closure(&self, ancestor: Uuid, descendant: Uuid, barrier: i16) {
        self.closure
            .lock()
            .unwrap()
            .insert((ancestor, descendant), barrier);
    }
}

#[async_trait]
impl TenantHierarchyReadPort for FakePort {
    async fn get(&self, id: Uuid) -> Result<Option<TenantModel>, DomainError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(&id)
            .filter(|r| !matches!(r.status, TenantStatus::Provisioning))
            .cloned())
    }

    async fn get_root(&self) -> Result<Vec<TenantModel>, DomainError> {
        let mut found: Vec<TenantModel> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.parent_id.is_none() && !matches!(r.status, TenantStatus::Provisioning))
            .cloned()
            .collect();
        found.sort_by_key(|r| r.id);
        found.truncate(2);
        Ok(found)
    }

    async fn get_bulk(
        &self,
        ids: &[Uuid],
        filter: &StatusFilter,
    ) -> Result<Vec<TenantModel>, DomainError> {
        let rows_guard = self.rows.lock().unwrap();
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut out = Vec::new();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let Some(r) = rows_guard.get(id) else {
                continue;
            };
            if matches!(r.status, TenantStatus::Provisioning) {
                continue;
            }
            if let StatusFilter::VisibleIn(allowed) = filter
                && !allowed.contains(&r.status)
            {
                continue;
            }
            out.push(r.clone());
        }
        Ok(out)
    }

    async fn get_ancestors(
        &self,
        descendant_id: Uuid,
        barrier_mode: PortBarrierMode,
    ) -> Result<Vec<Uuid>, DomainError> {
        Ok(self
            .closure
            .lock()
            .unwrap()
            .iter()
            .filter(|((a, d), b)| {
                *d == descendant_id
                    && *a != descendant_id
                    && (matches!(barrier_mode, PortBarrierMode::Ignore) || **b == 0)
            })
            .map(|((a, _), _)| *a)
            .collect())
    }

    async fn get_descendants(
        &self,
        ancestor_id: Uuid,
        barrier_mode: PortBarrierMode,
    ) -> Result<Vec<Uuid>, DomainError> {
        Ok(self
            .closure
            .lock()
            .unwrap()
            .iter()
            .filter(|((a, d), b)| {
                *a == ancestor_id
                    && *d != ancestor_id
                    && (matches!(barrier_mode, PortBarrierMode::Ignore) || **b == 0)
            })
            .map(|((_, d), _)| *d)
            .collect())
    }

    async fn is_ancestor(
        &self,
        ancestor_id: Uuid,
        descendant_id: Uuid,
        barrier_mode: PortBarrierMode,
    ) -> Result<bool, DomainError> {
        let map = self.closure.lock().unwrap();
        Ok(map
            .get(&(ancestor_id, descendant_id))
            .is_some_and(|b| matches!(barrier_mode, PortBarrierMode::Ignore) || *b == 0))
    }
}

// ── Harness helpers ───────────────────────────────────────────────────────

/// Build a fresh in-memory `FakePort`. Sync — no async DB setup needed.
fn setup() -> Arc<FakePort> {
    FakePort::new()
}

/// Build a `PluginImpl` over `port` with the given registry mode.
fn make_plugin(port: Arc<FakePort>, fail_registry: bool) -> PluginImpl {
    let registry = if fail_registry {
        TestRegistry::fail()
    } else {
        TestRegistry::ok()
    };
    let port_dyn: Arc<dyn TenantHierarchyReadPort> = port;
    PluginImpl::new(port_dyn, registry)
}

/// Anonymous `SecurityContext` — the plugin ignores it per DESIGN §4.2.
fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

// ── Seed helpers (sync, FakePort-backed) ──────────────────────────────────

/// Seed a single-root tenant with its self-row.
///
/// Provisioning tenants have no closure rows by AM's contract; pass
/// `ACTIVE` / `SUSPENDED` / `DELETED` only.
fn seed_root(port: &FakePort, status: TenantStatus) -> Uuid {
    assert!(
        !matches!(status, TenantStatus::Provisioning),
        "seed_root: provisioning roots have no closure rows; use port.insert_tenant directly"
    );
    let root = Uuid::new_v4();
    port.insert_tenant(root, None, status, 0);
    port.insert_closure(root, root, 0);
    root
}

/// Seed a two-level tree (root → child) with all required closure rows.
/// Returns `(root, child)`.
fn seed_two_level(
    port: &FakePort,
    root_status: TenantStatus,
    child_status: TenantStatus,
) -> (Uuid, Uuid) {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    port.insert_tenant(root, None, root_status, 0);
    port.insert_tenant(child, Some(root), child_status, 1);
    port.insert_closure(root, root, 0);
    port.insert_closure(child, child, 0);
    port.insert_closure(root, child, 0);
    (root, child)
}

// ── Tests: get_tenant ─────────────────────────────────────────────────────

#[tokio::test]
async fn get_tenant_returns_info() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), false);
    let info = plugin.get_tenant(&ctx(), TenantId(root)).await.unwrap();

    assert_eq!(info.id.0, root);
    assert_eq!(info.status, SdkStatus::Active);
    assert!(info.parent_id.is_none());
    assert!(
        info.tenant_type.is_some(),
        "registry must hydrate tenant_type"
    );
}

#[tokio::test]
async fn get_tenant_not_found() {
    let port = setup();
    let missing = Uuid::new_v4();

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_tenant(&ctx(), TenantId(missing))
        .await
        .unwrap_err();

    assert!(
        matches!(err, TenantResolverError::TenantNotFound { .. }),
        "expected TenantNotFound, got {err:?}"
    );
}

// ── Tests: provisioning invisibility ─────────────────────────────────────

#[tokio::test]
async fn provisioning_hidden_from_get_tenant() {
    let port = setup();
    let id = Uuid::new_v4();
    // Provisioning tenants have no closure rows (descendant_status CHECK constraint
    // allows only 1/2/3). The tenant row exists; the status predicate hides it.
    port.insert_tenant(id, None, PROVISIONING, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin.get_tenant(&ctx(), TenantId(id)).await.unwrap_err();
    assert!(matches!(err, TenantResolverError::TenantNotFound { .. }));
}

#[tokio::test]
async fn provisioning_only_root_yields_internal_error() {
    let port = setup();
    // Only row in the DB is a provisioning tenant with parent_id=None.
    // No closure rows (AM invariant). get_root_tenant must return Internal
    // because the provisioning-visibility predicate hides it.
    port.insert_tenant(Uuid::new_v4(), None, PROVISIONING, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin.get_root_tenant(&ctx()).await.unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "expected Internal when no non-provisioning root; got {err:?}"
    );
}

#[tokio::test]
async fn provisioning_hidden_from_is_ancestor_as_ancestor() {
    let port = setup();
    let anc = Uuid::new_v4(); // provisioning
    let desc = Uuid::new_v4(); // active
    // Provisioning tenants have no closure rows by AM invariant.
    port.insert_tenant(anc, None, PROVISIONING, 0);
    port.insert_tenant(desc, Some(anc), ACTIVE, 1);
    port.insert_closure(desc, desc, 0);
    // (anc, desc) closure row: descendant_status=ACTIVE is valid, but we omit
    // it to match AM's invariant that provisioning tenants have no closure rows.

    let plugin = make_plugin(Arc::clone(&port), false);
    // The existence check reads tenants with status != PROVISIONING;
    // anc is filtered → TenantNotFound.
    let err = plugin
        .is_ancestor(
            &ctx(),
            TenantId(anc),
            TenantId(desc),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::TenantNotFound { .. }));
}

#[tokio::test]
async fn provisioning_hidden_from_is_ancestor_as_descendant() {
    let port = setup();
    let anc = Uuid::new_v4(); // active root
    let desc = Uuid::new_v4(); // provisioning child
    port.insert_tenant(anc, None, ACTIVE, 0);
    // desc is provisioning: no closure rows (AM invariant).
    port.insert_tenant(desc, Some(anc), PROVISIONING, 1);
    port.insert_closure(anc, anc, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    // Existence check reads tenants with status != PROVISIONING; desc filtered → TenantNotFound.
    let err = plugin
        .is_ancestor(
            &ctx(),
            TenantId(anc),
            TenantId(desc),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::TenantNotFound { .. }));
}

#[tokio::test]
async fn provisioning_hidden_from_get_tenants() {
    let port = setup();
    let active_id = Uuid::new_v4();
    let prov_id = Uuid::new_v4();
    port.insert_tenant(active_id, None, ACTIVE, 0);
    // prov_id is a provisioning child (non-root to avoid the unique-root constraint).
    // No closure rows for provisioning tenants.
    port.insert_tenant(prov_id, Some(active_id), PROVISIONING, 1);
    port.insert_closure(active_id, active_id, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let result = plugin
        .get_tenants(
            &ctx(),
            &[TenantId(active_id), TenantId(prov_id)],
            &GetTenantsOptions { status: vec![] },
        )
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id.0, active_id);
}

#[tokio::test]
async fn provisioning_parent_yields_empty_ancestors() {
    // Hierarchy: provisioning_root → active_child.
    // Per AM's invariant a provisioning tenant carries NO closure
    // rows (in either direction), so `tenant_closure` only contains
    // the child's self-row. `get_ancestors(child)` therefore reads
    // an empty closure set (after self-exclusion) and emits an
    // empty ancestor list — the provisioning-invisibility property
    // is delivered structurally by AM's writer, not by a downstream
    // filter.
    let port = setup();
    let prov_root = Uuid::new_v4();
    let child = Uuid::new_v4();
    port.insert_tenant(prov_root, None, PROVISIONING, 0);
    port.insert_tenant(child, Some(prov_root), ACTIVE, 1);
    // Only the child's self-row — no closure rows reference
    // `prov_root` because it is provisioning.
    port.insert_closure(child, child, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_ancestors(
            &ctx(),
            TenantId(child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.tenant.id.0, child);
    assert!(
        resp.ancestors.is_empty(),
        "provisioning parent must not appear in ancestors"
    );
}

#[tokio::test]
async fn corrupt_closure_to_provisioning_ancestor_yields_internal() {
    // Defense-in-depth: AM's writer is contractually required to
    // remove every closure row referencing a tenant that becomes
    // provisioning, but if the on-disk state ever diverges from
    // that invariant — bug, partial migration, manual surgery — the
    // plugin MUST surface `Internal` rather than silently truncate
    // the ancestor chain. This test deliberately seeds corrupt data
    // (a closure row from a `provisioning` ancestor to an `active`
    // descendant) and asserts the fail-closed surface.
    let port = setup();
    let prov_root = Uuid::new_v4();
    let child = Uuid::new_v4();
    port.insert_tenant(prov_root, None, PROVISIONING, 0);
    port.insert_tenant(child, Some(prov_root), ACTIVE, 1);
    port.insert_closure(child, child, 0);
    // Corrupt: closure row whose ancestor is provisioning. AM's writer
    // would normally never produce it — that is exactly the corruption
    // the plugin needs to fail closed on.
    port.insert_closure(prov_root, child, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_ancestors(
            &ctx(),
            TenantId(child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "corrupt closure → provisioning ancestor must surface as Internal; got {err:?}"
    );
}

#[tokio::test]
async fn provisioning_start_hidden_from_get_descendants() {
    let port = setup();
    let prov = Uuid::new_v4();
    // Provisioning tenant — no closure rows (AM invariant).
    port.insert_tenant(prov, None, PROVISIONING, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_descendants(
            &ctx(),
            TenantId(prov),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::TenantNotFound { .. }));
}

#[tokio::test]
async fn corrupt_closure_to_provisioning_descendant_yields_internal() {
    // Symmetric companion to
    // `corrupt_closure_to_provisioning_ancestor_yields_internal`:
    // here the corrupt row is on the descendant side. AM's writer
    // contractually removes every closure row referencing a tenant
    // that becomes provisioning; if on-disk state diverges, the
    // bulk-hydrate vs closure-id-set check inside `get_descendants`
    // MUST surface `Internal` rather than silently truncate the
    // emitted subtree.
    let port = setup();
    let root = Uuid::new_v4();
    let prov_child = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(prov_child, Some(root), PROVISIONING, 1);
    port.insert_closure(root, root, 0);
    // Corrupt: closure row whose descendant is provisioning. AM's writer
    // would normally never produce it — the referenced `tenants` row
    // carries `PROVISIONING`, exactly the hydration mismatch the
    // fail-close defends against.
    port.insert_closure(root, prov_child, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "corrupt closure → provisioning descendant must surface as Internal; got {err:?}"
    );
}

// NOTE on the `visited.insert` revisit branch: structurally
// unreachable in valid `tenants` data because every row carries
// exactly one `parent_id`, so each node appears in exactly one
// `children_by_parent[parent]` entry and the walk pushes any given
// node at most once. The check exists as a paranoid guard against
// a future refactor that might break the single-parent invariant
// (e.g., a multi-parent tenant model). It is therefore covered by
// inspection rather than by an end-to-end test — every corruption
// shape we *can* construct from valid `(tenants, tenant_closure)`
// rows surfaces through the bulk-hydrate consistency check or the
// post-walk completeness check below, both of which ARE tested.

#[tokio::test]
async fn unreachable_closure_descendant_yields_internal() {
    // Hierarchy corruption where closure asserts X is a descendant
    // of pivot but X's parent_id chain never reaches pivot. The
    // post-walk completeness check must catch this: closure
    // hydrate succeeds (X resolves to a visible tenants row), but
    // the `parent_id` walk never visits X. With `max_depth = None`
    // the bound is unconstrained, so every closure descendant
    // MUST be visited. Setup: pivot=root, child reachable normally
    // (root → child), orphan in closure but its parent_id points
    // to a separate active tenant outside the subtree.
    let port = setup();
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    let orphan = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(child, Some(root), ACTIVE, 1);
    // `outsider` is a separate ACTIVE tenant not in the test
    // subtree (we make outsider a child of root too). Its only
    // role is to be `orphan.parent_id` so the walk from `root`
    // reaches `outsider` AND `child` legitimately.
    // The corruption is the closure row asserting `(root, orphan)`
    // when no such parent_id chain exists from the orphan back to
    // root through any descendant of root.
    port.insert_tenant(outsider, Some(root), ACTIVE, 1);
    // `orphan.parent_id = orphan itself` makes the parent_id walk from
    // root never reach orphan — self-parent creates an unreachable node.
    port.insert_tenant(orphan, Some(orphan), ACTIVE, 99);
    port.insert_closure(root, root, 0);
    port.insert_closure(child, child, 0);
    port.insert_closure(outsider, outsider, 0);
    port.insert_closure(orphan, orphan, 0);
    port.insert_closure(root, child, 0);
    port.insert_closure(root, outsider, 0);
    // Corrupt: closure asserts orphan is a descendant of root,
    // but orphan.parent_id is itself (no chain to root).
    port.insert_closure(root, orphan, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "closure descendant unreachable via parent_id walk must surface as Internal; got {err:?}"
    );
}

#[tokio::test]
async fn unreachable_in_bound_descendant_with_max_depth_yields_internal() {
    // Companion to `unreachable_closure_descendant_yields_internal`
    // covering the bounded path: an unreachable closure descendant
    // that falls WITHIN the requested `max_depth` envelope MUST
    // still surface `Internal`. A descendant outside the depth
    // envelope (legitimate trim) does NOT surface — that case is
    // covered separately by `get_descendants_max_depth_limits_traversal`.
    //
    // Setup: root → child (depth 1, normal), plus orphan at
    // depth 1 in closure with parent_id = self (unreachable).
    // Walk with `max_depth = Some(1)` should still flag orphan
    // as in-bound-but-unreachable.
    let port = setup();
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let orphan = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(child, Some(root), ACTIVE, 1);
    // orphan: depth=1 (in-bound for max_depth=1), parent_id=self.
    port.insert_tenant(orphan, Some(orphan), ACTIVE, 1);
    port.insert_closure(root, root, 0);
    port.insert_closure(child, child, 0);
    port.insert_closure(orphan, orphan, 0);
    port.insert_closure(root, child, 0);
    // Corrupt: closure says orphan is descendant of root.
    port.insert_closure(root, orphan, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: Some(1),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "in-bound unreachable descendant under max_depth must surface as Internal; got {err:?}"
    );
}

// ── Tests: get_root_tenant ────────────────────────────────────────────────

#[tokio::test]
async fn get_root_tenant_finds_single_root() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), false);
    let info = plugin.get_root_tenant(&ctx()).await.unwrap();
    assert_eq!(info.id.0, root);
}

#[tokio::test]
async fn get_root_tenant_no_root_yields_internal_error() {
    let port = setup();
    // Empty port — no tenant rows at all.
    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin.get_root_tenant(&ctx()).await.unwrap_err();
    assert!(matches!(err, TenantResolverError::Internal(_)));
}

// NOTE: the "multiple roots → Internal" branch of get_root_tenant is
// now exercisable with the FakePort (no DB unique-index constraint).
// The new test `get_root_tenant_multiple_roots_yields_internal_error`
// below covers this path directly.

#[tokio::test]
async fn get_root_tenant_multiple_roots_yields_internal_error() {
    let port = setup();
    let r1 = Uuid::new_v4();
    let r2 = Uuid::new_v4();
    port.insert_tenant(r1, None, ACTIVE, 0);
    port.insert_tenant(r2, None, ACTIVE, 0);
    port.insert_closure(r1, r1, 0);
    port.insert_closure(r2, r2, 0);
    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin.get_root_tenant(&ctx()).await.unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "multiple roots must surface as Internal; got {err:?}"
    );
}

// ── Tests: get_tenants ────────────────────────────────────────────────────

#[tokio::test]
async fn get_tenants_deduplicates_input_ids() {
    let port = setup();
    let (root, _child) = seed_two_level(&port, ACTIVE, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), false);
    // Pass the root ID three times.
    let result = plugin
        .get_tenants(
            &ctx(),
            &[TenantId(root), TenantId(root), TenantId(root)],
            &GetTenantsOptions { status: vec![] },
        )
        .await
        .unwrap();

    assert_eq!(result.len(), 1, "duplicate IDs must be deduplicated");
    assert_eq!(result[0].id.0, root);
}

#[tokio::test]
async fn get_tenants_status_filter() {
    let port = setup();
    let (root, child) = seed_two_level(&port, ACTIVE, SUSPENDED);

    let plugin = make_plugin(Arc::clone(&port), false);
    let active_only = plugin
        .get_tenants(
            &ctx(),
            &[TenantId(root), TenantId(child)],
            &GetTenantsOptions {
                status: vec![SdkStatus::Active],
            },
        )
        .await
        .unwrap();

    assert_eq!(active_only.len(), 1);
    assert_eq!(active_only[0].id.0, root);
}

#[tokio::test]
async fn get_tenants_empty_status_equals_all_visible() {
    // The `tenants_status_in_condition` projection treats an empty
    // `status` slice as "all SDK-visible statuses" by short-circuiting
    // to the bare provisioning-exclusion predicate. This test pins
    // that equivalence behaviorally: passing `vec![]` and passing
    // `vec![Active, Suspended, Deleted]` must yield the same row set
    // for the same `(ids, port)` pair.
    let port = setup();
    let root = Uuid::new_v4();
    let sus = Uuid::new_v4();
    let del = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(sus, Some(root), SUSPENDED, 1);
    port.insert_tenant(del, Some(root), DELETED, 1);
    port.insert_closure(root, root, 0);
    port.insert_closure(sus, sus, 0);
    port.insert_closure(del, del, 0);
    port.insert_closure(root, sus, 0);
    port.insert_closure(root, del, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ids = [TenantId(root), TenantId(sus), TenantId(del)];

    let empty_filter = plugin
        .get_tenants(&ctx(), &ids, &GetTenantsOptions { status: vec![] })
        .await
        .unwrap();
    let explicit_filter = plugin
        .get_tenants(
            &ctx(),
            &ids,
            &GetTenantsOptions {
                status: vec![SdkStatus::Active, SdkStatus::Suspended, SdkStatus::Deleted],
            },
        )
        .await
        .unwrap();

    let mut empty_ids: Vec<Uuid> = empty_filter.iter().map(|t| t.id.0).collect();
    let mut explicit_ids: Vec<Uuid> = explicit_filter.iter().map(|t| t.id.0).collect();
    empty_ids.sort_unstable();
    explicit_ids.sort_unstable();
    assert_eq!(
        empty_ids, explicit_ids,
        "empty status slice must equal the explicit all-visible-statuses set"
    );
    assert_eq!(empty_ids.len(), 3);
}

#[tokio::test]
async fn get_tenants_empty_ids_returns_empty() {
    let port = setup();
    let plugin = make_plugin(Arc::clone(&port), false);
    let result = plugin
        .get_tenants(&ctx(), &[], &GetTenantsOptions { status: vec![] })
        .await
        .unwrap();
    assert!(result.is_empty());
}

// ── Tests: is_ancestor ────────────────────────────────────────────────────

#[tokio::test]
async fn is_ancestor_direct_parent_returns_true() {
    let port = setup();
    let (root, child) = seed_two_level(&port, ACTIVE, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(child),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();
    assert!(ok);
}

#[tokio::test]
async fn is_ancestor_self_returns_false() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(root),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();
    assert!(!ok, "self is not an ancestor of self");
}

#[tokio::test]
async fn is_ancestor_unrelated_siblings_returns_false() {
    // Both endpoints exist and are non-provisioning (so the visibility
    // probe passes), but no `(ancestor, descendant)` row exists in
    // `tenant_closure`. Exercises the `count == 0` terminal branch of
    // `is_ancestor` — distinct from `is_ancestor_self_returns_false`,
    // which short-circuits before the closure probe.
    let port = setup();
    let root = Uuid::new_v4();
    let sib_a = Uuid::new_v4();
    let sib_b = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(sib_a, Some(root), ACTIVE, 1);
    port.insert_tenant(sib_b, Some(root), ACTIVE, 1);
    port.insert_closure(root, root, 0);
    port.insert_closure(sib_a, sib_a, 0);
    port.insert_closure(sib_b, sib_b, 0);
    port.insert_closure(root, sib_a, 0);
    port.insert_closure(root, sib_b, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(sib_a),
            TenantId(sib_b),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();
    assert!(!ok, "siblings are not ancestors of each other");
}

#[tokio::test]
async fn is_ancestor_missing_endpoint_yields_not_found() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);
    let missing = Uuid::new_v4();

    let plugin = make_plugin(Arc::clone(&port), false);
    let err = plugin
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(missing),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::TenantNotFound { .. }));
}

// ── Tests: barrier semantics ──────────────────────────────────────────────
//
// Hierarchy: A (root) → B → C, where the A→B edge carries barrier=1
// (simulating a self_managed tenant boundary as AM's writer would set it).
//
// The plugin only reads `tenant_closure.barrier`; it never inspects
// `tenants.self_managed` directly. Tests therefore seed the correct
// barrier values in the closure rows and leave `self_managed=false` on
// the tenant rows without loss of fidelity.
//
// Closure rows:
//   (A, A, barrier=0)  — self-row
//   (B, B, barrier=0)  — self-row
//   (C, C, barrier=0)  — self-row
//   (A, B, barrier=1)  — barrier set by AM writer at tenant-B creation
//   (B, C, barrier=0)  — within B's subtree, no additional barrier
//   (A, C, barrier=1)  — A→C inherits the barrier from the A→B edge

fn seed_barrier_tree(port: &FakePort) -> (Uuid, Uuid, Uuid) {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    port.insert_tenant(a, None, ACTIVE, 0);
    port.insert_tenant(b, Some(a), ACTIVE, 1);
    port.insert_tenant(c, Some(b), ACTIVE, 2);
    // Self-rows
    port.insert_closure(a, a, 0);
    port.insert_closure(b, b, 0);
    port.insert_closure(c, c, 0);
    // Cross-boundary rows
    port.insert_closure(a, b, 1);
    port.insert_closure(b, c, 0);
    port.insert_closure(a, c, 1);
    (a, b, c)
}

#[tokio::test]
async fn barrier_respect_is_ancestor_blocked() {
    let port = setup();
    let (a, _b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(a),
            TenantId(c),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .unwrap();
    assert!(!ok, "Respect should block cross-barrier ancestry");
}

#[tokio::test]
async fn barrier_ignore_is_ancestor_crosses() {
    let port = setup();
    let (a, _b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(a),
            TenantId(c),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();
    assert!(ok, "Ignore should allow cross-barrier ancestry");
}

#[tokio::test]
async fn barrier_respect_get_ancestors_clamps_at_barrier() {
    let port = setup();
    let (a, b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_ancestors(
            &ctx(),
            TenantId(c),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .unwrap();

    let ancestor_ids: Vec<Uuid> = resp.ancestors.iter().map(|t| t.id.0).collect();
    assert!(
        ancestor_ids.contains(&b),
        "B (within-barrier ancestor) must appear"
    );
    assert!(
        !ancestor_ids.contains(&a),
        "A (cross-barrier ancestor) must NOT appear under Respect"
    );
}

#[tokio::test]
async fn barrier_ignore_get_ancestors_crosses_barrier() {
    let port = setup();
    let (a, b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_ancestors(
            &ctx(),
            TenantId(c),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();

    let ancestor_ids: Vec<Uuid> = resp.ancestors.iter().map(|t| t.id.0).collect();
    assert!(ancestor_ids.contains(&a), "A must appear under Ignore");
    assert!(ancestor_ids.contains(&b), "B must appear under Ignore");
}

#[tokio::test]
async fn barrier_respect_get_descendants_stops_at_boundary() {
    let port = setup();
    let (a, b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_descendants(
            &ctx(),
            TenantId(a),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Respect,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap();

    let desc_ids: Vec<Uuid> = resp.descendants.iter().map(|t| t.id.0).collect();
    assert!(
        !desc_ids.contains(&b),
        "B (cross-barrier) must NOT appear under Respect"
    );
    assert!(
        !desc_ids.contains(&c),
        "C (cross-barrier) must NOT appear under Respect"
    );
    assert!(
        resp.descendants.is_empty(),
        "A has no non-barrier descendants"
    );
}

#[tokio::test]
async fn barrier_ignore_get_descendants_crosses_boundary() {
    let port = setup();
    let (a, b, c) = seed_barrier_tree(&port);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_descendants(
            &ctx(),
            TenantId(a),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap();

    let desc_ids: Vec<Uuid> = resp.descendants.iter().map(|t| t.id.0).collect();
    assert!(desc_ids.contains(&b), "B must appear under Ignore");
    assert!(desc_ids.contains(&c), "C must appear under Ignore");
}

// ── Tests: status filter semantics ───────────────────────────────────────

#[tokio::test]
async fn status_filter_does_not_prune_branches() {
    // Root (Active) → Mid (Suspended) → Leaf (Active)
    // Filtering by [Active] must still return Leaf even though Mid is
    // Suspended — the filter is an emission predicate, not a branch prune.
    let port = setup();
    let root = Uuid::new_v4();
    let mid = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(mid, Some(root), SUSPENDED, 1);
    port.insert_tenant(leaf, Some(mid), ACTIVE, 2);
    // Self-rows
    port.insert_closure(root, root, 0);
    port.insert_closure(mid, mid, 0);
    port.insert_closure(leaf, leaf, 0);
    // Ancestor rows (barrier=0 everywhere — no self_managed tenant)
    port.insert_closure(root, mid, 0);
    port.insert_closure(root, leaf, 0);
    port.insert_closure(mid, leaf, 0);

    let plugin = make_plugin(Arc::clone(&port), false);
    let resp = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![SdkStatus::Active],
                max_depth: None,
            },
        )
        .await
        .unwrap();

    let desc_ids: Vec<Uuid> = resp.descendants.iter().map(|t| t.id.0).collect();
    assert!(
        desc_ids.contains(&leaf),
        "Leaf (Active) must be emitted even though Mid (Suspended) is on the path"
    );
    assert!(
        !desc_ids.contains(&mid),
        "Mid (Suspended) must not be emitted when filter=[Active]"
    );
}

// ── Tests: get_descendants max_depth ─────────────────────────────────────

#[tokio::test]
async fn get_descendants_max_depth_limits_traversal() {
    // ROOT → CHILD → GRANDCHILD → GREAT
    let port = setup();
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let grandchild = Uuid::new_v4();
    let great = Uuid::new_v4();
    port.insert_tenant(root, None, ACTIVE, 0);
    port.insert_tenant(child, Some(root), ACTIVE, 1);
    port.insert_tenant(grandchild, Some(child), ACTIVE, 2);
    port.insert_tenant(great, Some(grandchild), ACTIVE, 3);
    // Self-rows
    for id in [root, child, grandchild, great] {
        port.insert_closure(id, id, 0);
    }
    // Ancestor rows
    port.insert_closure(root, child, 0);
    port.insert_closure(root, grandchild, 0);
    port.insert_closure(root, great, 0);
    port.insert_closure(child, grandchild, 0);
    port.insert_closure(child, great, 0);
    port.insert_closure(grandchild, great, 0);

    let plugin = make_plugin(Arc::clone(&port), false);

    // max_depth=1 → only CHILD
    let r1 = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(r1.descendants.len(), 1);
    assert_eq!(r1.descendants[0].id.0, child);

    // max_depth=2 → CHILD + GRANDCHILD
    let r2 = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: Some(2),
            },
        )
        .await
        .unwrap();
    let ids2: Vec<Uuid> = r2.descendants.iter().map(|t| t.id.0).collect();
    assert!(ids2.contains(&child));
    assert!(ids2.contains(&grandchild));
    assert!(!ids2.contains(&great));

    // max_depth=None → all three
    let r_all = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(r_all.descendants.len(), 3);
}

// ── Tests: TypesRegistry failure → Internal ───────────────────────────────

#[tokio::test]
async fn registry_fail_get_tenant_yields_internal() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true);
    let err = plugin.get_tenant(&ctx(), TenantId(root)).await.unwrap_err();
    assert!(
        matches!(err, TenantResolverError::Internal(_)),
        "registry failure must surface as Internal; got {err:?}"
    );
}

#[tokio::test]
async fn registry_fail_get_root_tenant_yields_internal() {
    let port = setup();
    seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true);
    let err = plugin.get_root_tenant(&ctx()).await.unwrap_err();
    assert!(matches!(err, TenantResolverError::Internal(_)));
}

#[tokio::test]
async fn registry_fail_get_tenants_yields_internal() {
    let port = setup();
    let root = seed_root(&port, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true);
    let err = plugin
        .get_tenants(
            &ctx(),
            &[TenantId(root)],
            &GetTenantsOptions { status: vec![] },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::Internal(_)));
}

#[tokio::test]
async fn registry_fail_get_ancestors_yields_internal() {
    let port = setup();
    let (_root, child) = seed_two_level(&port, ACTIVE, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true);
    let err = plugin
        .get_ancestors(
            &ctx(),
            TenantId(child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::Internal(_)));
}

#[tokio::test]
async fn registry_fail_get_descendants_yields_internal() {
    let port = setup();
    let (root, _child) = seed_two_level(&port, ACTIVE, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true);
    let err = plugin
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Ignore,
                status: vec![],
                max_depth: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, TenantResolverError::Internal(_)));
}

#[tokio::test]
async fn is_ancestor_not_affected_by_registry_failure() {
    // `is_ancestor` probes `tenants` for visibility and `tenant_closure` for
    // the edge — it never calls TypesRegistryClient. A failing registry must
    // not affect the result.
    let port = setup();
    let (root, child) = seed_two_level(&port, ACTIVE, ACTIVE);

    let plugin = make_plugin(Arc::clone(&port), true); // registry always fails
    let ok = plugin
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(child),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();
    assert!(ok, "is_ancestor must not depend on TypesRegistry");
}

// ── Tests: ClientHub wiring contract ─────────────────────────────────────

/// Pins the in-process registration shape `Gear::init` uses on the
/// `tr_plugin.enabled = true` branch (see `gear.rs` near
/// "tenant-resolver plugin registered (in-process, AM-co-located)"):
///
/// 1. The plugin is built as `Arc<PluginImpl>` and erased to
///    `Arc<dyn TenantResolverPluginClient>` so the gateway can resolve
///    it through the SDK trait.
/// 2. Its `ClientHub` key is `ClientScope::gts_id(&instance_id)` where
///    `instance_id` is derived from `TenantResolverPluginSpecV1` and
///    the AM-builtin segment string
///    `"cf.builtin.account_management_tenant_resolver.plugin.v1"`.
/// 3. The registered plugin answers a real `get_tenant` round-trip
///    against the seeded port after resolving through the hub.
///
/// The 40+ tests above pin the plugin's behaviour in isolation; this
/// test pins the gateway-discovery seam separately. A rename of the
/// instance segment (would silently flip selection in the TR gateway),
/// a swap of the trait identity (would make scoped lookups miss), or a
/// regression in `ClientHub::register_scoped` / `get_scoped` would
/// surface here instead of in a downstream E2E run.
#[tokio::test]
async fn client_hub_round_trip_resolves_registered_plugin() {
    use tenant_resolver_sdk::TenantResolverPluginSpecV1;
    use toolkit::ClientHub;
    use toolkit::client_hub::ClientScope;

    let port = setup();
    let root = seed_root(&port, ACTIVE);

    // Same instance-id derivation as `Gear::init`; mirror the literal
    // so a rename of the AM-builtin segment trips this test instead of
    // silently flipping TR-gateway selection.
    let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
        "cf.builtin.account_management_tenant_resolver.plugin.v1",
    );

    let plugin: Arc<dyn TenantResolverPluginClient> =
        Arc::new(make_plugin(Arc::clone(&port), false));

    let hub = ClientHub::new();
    hub.register_scoped::<dyn TenantResolverPluginClient>(
        ClientScope::gts_id(&instance_id),
        Arc::clone(&plugin),
    );

    let resolved = hub
        .get_scoped::<dyn TenantResolverPluginClient>(&ClientScope::gts_id(&instance_id))
        .expect("registered plugin must be resolvable under its gts-scoped key");

    let info = resolved
        .get_tenant(&ctx(), TenantId(root))
        .await
        .expect("resolved plugin must answer get_tenant against the seeded root");

    assert_eq!(info.id.0, root);
    assert_eq!(info.status, SdkStatus::Active);
    assert!(info.parent_id.is_none(), "seeded tenant is the root");
}

/// Negative twin: a fresh `ClientHub` with no registration must miss
/// the same scoped lookup, surfacing `ScopedNotFound`. This pins that
/// the `enabled = false` branch of `Gear::init` -- which intentionally
/// skips both the types-registry advertise and the `register_scoped`
/// call -- leaves the hub clean from the TR gateway's perspective, so
/// AM does not accidentally win selection when an operator opts out.
#[tokio::test]
async fn client_hub_lookup_misses_when_plugin_not_registered() {
    use tenant_resolver_sdk::TenantResolverPluginSpecV1;
    use toolkit::ClientHub;
    use toolkit::client_hub::{ClientHubError, ClientScope};

    let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
        "cf.builtin.account_management_tenant_resolver.plugin.v1",
    );

    let hub = ClientHub::new();
    // `Arc<dyn TenantResolverPluginClient>` does not implement `Debug`,
    // so `expect_err` is unavailable -- match on the `Result` directly.
    match hub.get_scoped::<dyn TenantResolverPluginClient>(&ClientScope::gts_id(&instance_id)) {
        Ok(_) => panic!("empty hub must not produce a resolved client"),
        Err(ClientHubError::ScopedNotFound { .. }) => {}
        Err(other) => panic!("expected ScopedNotFound, got {other:?}"),
    }
}

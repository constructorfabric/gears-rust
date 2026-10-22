//! Fixtures shared by every unit-test module of the crate: the resource
//! types the tests use, a caller builder, a fake tenant tree with the tenant
//! resolver's barrier semantics, and a migrated in-memory database.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use sea_orm_migration::MigratorTrait;
use tenant_resolver_sdk::{BarrierMode, TenantId};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::ports::{HierarchyPort, PortError};
use crate::infra::storage::Migrator;

/// A resource type every test registry knows.
pub const WIDGET: &str = "gts.cf.core.test.widget.v1~";
/// A second resource type, for selection tests.
pub const GADGET: &str = "gts.cf.core.test.gadget.v1~";
/// A wildcard pattern matching both [`WIDGET`] and [`GADGET`].
pub const TEST_PATTERN: &str = "gts.cf.core.test.*";
/// A well-formed resource type no test registry knows.
pub const UNKNOWN_TYPE: &str = "gts.cf.core.test.unknown.v1~";

/// A caller `subject` in `tenant`.
pub fn ctx(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("security context")
}

/// A hand-written tenant tree implementing the hierarchy port, with fault
/// injection and a count of reachability calls.
pub struct FakeTree {
    /// Each tenant's parent and whether it is self-managed (a barrier).
    nodes: HashMap<Uuid, (Option<Uuid>, bool)>,
    /// When set, every call fails with this error.
    pub fault: Mutex<Option<PortError>>,
    /// Reachability calls answered so far.
    pub reach_calls: AtomicUsize,
}

impl FakeTree {
    /// A tree of `(tenant, parent, self_managed)` nodes.
    pub fn new(nodes: &[(Uuid, Option<Uuid>, bool)]) -> Self {
        Self {
            nodes: nodes
                .iter()
                .map(|&(tenant, parent, managed)| (tenant, (parent, managed)))
                .collect(),
            fault: Mutex::new(None),
            reach_calls: AtomicUsize::new(0),
        }
    }

    /// Makes every later call fail with `fault`.
    pub fn fail_with(&self, fault: PortError) {
        *self.fault.lock() = Some(fault);
    }

    fn node(&self, tenant: Uuid) -> Result<(Option<Uuid>, bool), PortError> {
        if let Some(fault) = self.fault.lock().clone() {
            return Err(fault);
        }
        self.nodes
            .get(&tenant)
            .copied()
            .ok_or_else(|| PortError::NotFound(tenant.to_string()))
    }
}

#[async_trait]
impl HierarchyPort for FakeTree {
    async fn ancestors(
        &self,
        _ctx: &SecurityContext,
        tenant: TenantId,
    ) -> Result<Vec<Uuid>, PortError> {
        let mut chain = Vec::new();
        let mut next = Some(tenant.0);
        while let Some(current) = next {
            let (parent, self_managed) = self.node(current)?;
            chain.push(current);
            next = if self_managed { None } else { parent };
        }
        Ok(chain)
    }

    async fn is_reachable(
        &self,
        _ctx: &SecurityContext,
        from_tenant: TenantId,
        target_tenant: TenantId,
        barrier: BarrierMode,
    ) -> Result<bool, PortError> {
        self.reach_calls.fetch_add(1, Ordering::SeqCst);
        let (from_tenant, mut current) = (from_tenant.0, target_tenant.0);
        self.node(from_tenant)?;
        loop {
            if current == from_tenant {
                return Ok(true);
            }
            let (parent, self_managed) = self.node(current)?;
            if self_managed && barrier == BarrierMode::Respect {
                return Ok(false);
            }
            match parent {
                Some(parent) => current = parent,
                None => return Ok(false),
            }
        }
    }
}

/// An in-memory `SQLite` database with every migration of the gear applied.
pub async fn migrated_db() -> Db {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| e.to_string())
        .expect("migrations apply");
    db
}

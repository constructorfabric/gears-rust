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

use crate::domain::ports::{HierarchyPort, PortError, Subtree};
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
    /// When set, every call from the given count of calls on fails with the
    /// error: a dependency that fails part-way through an operation.
    pub fault_from: Mutex<Option<(usize, PortError)>>,
    /// Calls of any kind answered or refused so far.
    pub calls: AtomicUsize,
    /// Reachability calls answered so far.
    pub reach_calls: AtomicUsize,
    /// Subtree reads answered so far.
    pub subtree_calls: AtomicUsize,
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
            fault_from: Mutex::new(None),
            calls: AtomicUsize::new(0),
            reach_calls: AtomicUsize::new(0),
            subtree_calls: AtomicUsize::new(0),
        }
    }

    /// Makes every later call fail with `fault`.
    pub fn fail_with(&self, fault: PortError) {
        *self.fault.lock() = Some(fault);
    }

    /// Makes every call from the `nth` one on (counting from 0, over the
    /// tree's whole life) fail with `fault`.
    pub fn fail_from(&self, nth: usize, fault: PortError) {
        *self.fault_from.lock() = Some((nth, fault));
    }

    /// Counts one call, refusing it when [`Self::fail_from`] says so.
    fn call(&self) -> Result<(), PortError> {
        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        match &*self.fault_from.lock() {
            Some((from, fault)) if nth >= *from => Err(fault.clone()),
            _ => Ok(()),
        }
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

    /// Whether `from` is `target` or an ancestor of it under `barrier`.
    fn reaches(&self, from: Uuid, target: Uuid, barrier: BarrierMode) -> Result<bool, PortError> {
        self.node(from)?;
        let mut current = target;
        loop {
            if current == from {
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

#[async_trait]
impl HierarchyPort for FakeTree {
    async fn ancestors(
        &self,
        _ctx: &SecurityContext,
        tenant: TenantId,
    ) -> Result<Vec<Uuid>, PortError> {
        self.call()?;
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
        self.call()?;
        self.reach_calls.fetch_add(1, Ordering::SeqCst);
        self.reaches(from_tenant.0, target_tenant.0, barrier)
    }

    async fn subtree(&self, _ctx: &SecurityContext, root: TenantId) -> Result<Subtree, PortError> {
        self.call()?;
        self.subtree_calls.fetch_add(1, Ordering::SeqCst);
        self.node(root.0)?;
        let mut descendants = Vec::new();
        for (&tenant, &(parent, self_managed)) in &self.nodes {
            if let Some(parent) = parent
                && tenant != root.0
                && self.reaches(root.0, tenant, BarrierMode::Ignore)?
            {
                descendants.push((tenant, parent, self_managed));
            }
        }
        Ok(Subtree::new(root.0, descendants))
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

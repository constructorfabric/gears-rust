//! In-memory fixtures of the decision-path tests: a tenant tree honouring
//! barriers, a binding source and builders for versions with real Rego.
//!
//! ```text
//! ROOT ─┬─ PARENT ─┬─ CHILD
//!       │          └─ ISLAND*   (* = self-managed, a barrier)
//!       └─ SIBLING
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use admission_control_sdk::EngineRequest;
use async_trait::async_trait;
use parking_lot::Mutex;
use tenant_resolver_sdk::BarrierMode;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{CompileCache, DecisionMetrics, DecisionService};
use crate::domain::model::{
    Assignment, AssignmentId, BundleId, BundleVersion, Document, DocumentId, VersionId,
    VersionState,
};
use crate::domain::ports::{ActiveBinding, BindingSource, HierarchyPort, PortError};

pub const ROOT: Uuid = Uuid::from_u128(0xA0);
pub const PARENT: Uuid = Uuid::from_u128(0xA1);
pub const CHILD: Uuid = Uuid::from_u128(0xA2);
pub const SIBLING: Uuid = Uuid::from_u128(0xA3);
pub const ISLAND: Uuid = Uuid::from_u128(0xA4);
pub const WIDGET: &str = "gts.cf.core.example.widget.v1~";
pub const GADGET: &str = "gts.cf.core.example.gadget.v1~";

pub const DENY_ALL: &str = "package p\ndeny := true";
pub const DENY_NONE: &str = "package p\ndeny := false";
pub const DENY_DELETE: &str = "package p\ndeny := true if input.action == \"delete\"";
pub const NOT_BOOLEAN: &str = "package p\ndeny := \"yes\"";

/// Hand-written tenant tree with fault injection.
pub struct FakeHierarchy {
    tree: HashMap<Uuid, (Option<Uuid>, bool)>,
    pub fault: Mutex<Option<PortError>>,
    pub reach_calls: AtomicUsize,
}

impl FakeHierarchy {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tree: HashMap::from([
                (ROOT, (None, false)),
                (PARENT, (Some(ROOT), false)),
                (CHILD, (Some(PARENT), false)),
                (SIBLING, (Some(ROOT), false)),
                (ISLAND, (Some(PARENT), true)),
            ]),
            fault: Mutex::new(None),
            reach_calls: AtomicUsize::new(0),
        })
    }

    fn check(&self, tenant: Uuid) -> Result<(Option<Uuid>, bool), PortError> {
        if let Some(fault) = self.fault.lock().clone() {
            return Err(fault);
        }
        self.tree
            .get(&tenant)
            .copied()
            .ok_or_else(|| PortError::NotFound(tenant.to_string()))
    }
}

#[async_trait]
impl HierarchyPort for FakeHierarchy {
    async fn ancestors(
        &self,
        _ctx: &SecurityContext,
        tenant: Uuid,
    ) -> Result<Vec<Uuid>, PortError> {
        let mut chain = Vec::new();
        let mut next = Some(tenant);
        while let Some(current) = next {
            let (parent, self_managed) = self.check(current)?;
            chain.push(current);
            next = if self_managed { None } else { parent };
        }
        Ok(chain)
    }

    async fn is_reachable(
        &self,
        _ctx: &SecurityContext,
        from_tenant: Uuid,
        target_tenant: Uuid,
        barrier: BarrierMode,
    ) -> Result<bool, PortError> {
        self.reach_calls.fetch_add(1, Ordering::SeqCst);
        let mut current = target_tenant;
        loop {
            if current == from_tenant {
                return Ok(true);
            }
            let (parent, self_managed) = self.check(current)?;
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

/// Binding source over a fixed list, recording the chains it was asked for.
pub struct FakeBindings {
    pub bindings: Vec<ActiveBinding>,
    pub asked: Mutex<Vec<Vec<Uuid>>>,
}

#[async_trait]
impl BindingSource for FakeBindings {
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError> {
        self.asked.lock().push(tenants.to_vec());
        Ok(self
            .bindings
            .iter()
            .filter(|b| tenants.contains(&b.assignment.tenant_id))
            .cloned()
            .collect())
    }
}

/// Counts compile-cache lookups.
#[derive(Default)]
pub struct CountingMetrics {
    pub hits: AtomicUsize,
    pub misses: AtomicUsize,
}

impl DecisionMetrics for CountingMetrics {
    fn compile_cache(&self, hit: bool) {
        let counter = if hit { &self.hits } else { &self.misses };
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

/// A document over `types` and `actions` with Rego `source`.
pub fn document(name: &str, source: &str, types: &[&str], actions: &[&str]) -> Document {
    Document {
        id: DocumentId(Uuid::new_v4()),
        name: name.to_owned(),
        content: source.to_owned(),
        resource_types: types.iter().map(|t| (*t).to_owned()).collect(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
    }
}

/// An active version of bundle `bundle` assigned to `tenant`.
pub fn binding(
    bundle: u128,
    tenant: Uuid,
    enforce: bool,
    documents: Vec<Document>,
) -> ActiveBinding {
    let now = OffsetDateTime::now_utc();
    let bundle_id = BundleId(Uuid::from_u128(bundle));
    ActiveBinding {
        assignment: Assignment {
            id: AssignmentId(Uuid::new_v4()),
            bundle_id,
            tenant_id: tenant,
            owner_tenant_id: ROOT,
            enforce,
            created_at: now,
            updated_at: now,
        },
        version: Arc::new(BundleVersion {
            id: VersionId(Uuid::from_u128(bundle + 1_000)),
            bundle_id,
            owner_tenant_id: ROOT,
            ordinal: 1,
            state: VersionState::Active,
            created_at: now,
            activated_at: Some(now),
            activated_by: None,
            documents,
        }),
    }
}

/// A binding with one document over [`WIDGET`], every action.
pub fn widget_binding(bundle: u128, tenant: Uuid, enforce: bool, source: &str) -> ActiveBinding {
    binding(
        bundle,
        tenant,
        enforce,
        vec![document(&format!("doc{bundle}"), source, &[WIDGET], &[])],
    )
}

/// The service under test with its fakes.
pub struct Stack {
    pub service: Arc<DecisionService>,
    pub hierarchy: Arc<FakeHierarchy>,
    pub bindings: Arc<FakeBindings>,
    pub cache: Arc<CompileCache>,
    pub metrics: Arc<CountingMetrics>,
}

impl Stack {
    pub fn new(bindings: Vec<ActiveBinding>) -> Self {
        let hierarchy = FakeHierarchy::new();
        let bindings = Arc::new(FakeBindings {
            bindings,
            asked: Mutex::new(Vec::new()),
        });
        let cache = Arc::new(CompileCache::new(16));
        let metrics = Arc::new(CountingMetrics::default());
        let service = Arc::new(DecisionService::new(
            Arc::clone(&hierarchy) as Arc<dyn HierarchyPort>,
            Arc::clone(&bindings) as Arc<dyn BindingSource>,
            Arc::clone(&cache),
            Duration::from_secs(5),
            Arc::clone(&metrics) as Arc<dyn DecisionMetrics>,
        ));
        Self {
            service,
            hierarchy,
            bindings,
            cache,
            metrics,
        }
    }
}

/// A caller in `tenant`.
pub fn ctx_in(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5B))
        .subject_tenant_id(tenant)
        .build()
        .expect("security context")
}

/// A request for `action` on a `type_id` resource in `tenant`.
pub fn request_for(action: &str, type_id: &str, tenant: Uuid) -> EngineRequest {
    EngineRequest {
        correlation_id: Uuid::new_v4(),
        enforcing_gear: "test-gear".to_owned(),
        action: action.to_owned(),
        resource_type: type_id.to_owned(),
        resource_id: None,
        resource_tenant_id: tenant,
        properties: serde_json::Map::new(),
    }
}

/// A request for `action` on a [`WIDGET`] in `tenant`.
pub fn request(action: &str, tenant: Uuid) -> EngineRequest {
    request_for(action, WIDGET, tenant)
}

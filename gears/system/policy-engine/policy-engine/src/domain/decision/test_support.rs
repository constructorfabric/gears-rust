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
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{BuiltinPolicySet, CompileCache, DecisionMetrics, DecisionService};
use crate::domain::model::{
    Assignment, AssignmentId, BundleId, BundleVersion, Document, DocumentId, VersionId,
    VersionState,
};
use crate::domain::ports::{ActiveBinding, BindingSource, HierarchyPort, PortError};
use crate::test_support::{FakeTree, ctx};

pub const ROOT: Uuid = Uuid::from_u128(0xA0);
pub const PARENT: Uuid = Uuid::from_u128(0xA1);
pub const CHILD: Uuid = Uuid::from_u128(0xA2);
pub const SIBLING: Uuid = Uuid::from_u128(0xA3);
pub const ISLAND: Uuid = Uuid::from_u128(0xA4);
pub use crate::test_support::{GADGET, TEST_PATTERN, WIDGET};

pub const DENY_ALL: &str = "package p\ndeny := true";
pub const DENY_NONE: &str = "package p\ndeny := false";
pub const DENY_DELETE: &str = "package p\ndeny := true if input.action == \"delete\"";
pub const NOT_BOOLEAN: &str = "package p\ndeny := \"yes\"";
pub const EXPLICIT_NULL: &str = "package p\ndeny := null";

/// An assignment with its bundle's active version and that version's
/// content, as the store holds them.
#[derive(Debug, Clone)]
pub struct Stored {
    pub assignment: Assignment,
    pub version: Arc<BundleVersion>,
}

impl Stored {
    /// The same assignment, made by `owner`.
    pub fn owned_by(mut self, owner: Uuid) -> Self {
        self.assignment.owner_tenant_id = owner;
        self
    }
}

/// Binding source over a fixed list, recording the chains and the version
/// content it was asked for, with fault injection. Selects by target tenant
/// only, so the service's own owner check is what the tests observe.
pub struct FakeBindings {
    pub stored: Vec<Stored>,
    pub asked: Mutex<Vec<Vec<Uuid>>>,
    pub content_reads: Mutex<Vec<Vec<VersionId>>>,
    pub fault: Mutex<Option<PortError>>,
}

impl FakeBindings {
    pub fn new(stored: Vec<Stored>) -> Self {
        Self {
            stored,
            asked: Mutex::new(Vec::new()),
            content_reads: Mutex::new(Vec::new()),
            fault: Mutex::new(None),
        }
    }
}

#[async_trait]
impl BindingSource for FakeBindings {
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError> {
        self.asked.lock().push(tenants.to_vec());
        if let Some(fault) = self.fault.lock().clone() {
            return Err(fault);
        }
        Ok(self
            .stored
            .iter()
            .filter(|b| tenants.contains(&b.assignment.tenant_id))
            .map(|b| ActiveBinding {
                assignment: b.assignment.clone(),
                version_id: b.version.id,
            })
            .collect())
    }

    async fn version_documents(
        &self,
        versions: &[VersionId],
    ) -> Result<HashMap<VersionId, Vec<Document>>, PortError> {
        self.content_reads.lock().push(versions.to_vec());
        if let Some(fault) = self.fault.lock().clone() {
            return Err(fault);
        }
        Ok(self
            .stored
            .iter()
            .filter(|b| versions.contains(&b.version.id))
            .map(|b| (b.version.id, b.version.documents.clone()))
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

/// Rego no test can finish within a short evaluation timeout, written
/// without denylisted builtins so it passes the compile-time screen: 10^10
/// iterations over a literal 100-element array.
pub fn unbounded() -> String {
    let items: Vec<String> = (0..100).map(|n| n.to_string()).collect();
    format!(
        "package p\nxs := [{}]\ndeny := count([1 | some a in xs; some b in xs; some c in xs; some d in xs; some e in xs]) > 0",
        items.join(", ")
    )
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

/// An active version of bundle `bundle` assigned to `tenant` by `tenant`
/// itself.
pub fn binding(bundle: u128, tenant: Uuid, enforce: bool, documents: Vec<Document>) -> Stored {
    let now = OffsetDateTime::now_utc();
    let bundle_id = BundleId(Uuid::from_u128(bundle));
    Stored {
        assignment: Assignment {
            id: AssignmentId(Uuid::new_v4()),
            bundle_id,
            tenant_id: tenant,
            owner_tenant_id: tenant,
            enforce,
            created_at: now,
            created_by: Uuid::nil(),
            updated_at: now,
            updated_by: Uuid::nil(),
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
pub fn widget_binding(bundle: u128, tenant: Uuid, enforce: bool, source: &str) -> Stored {
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
    pub hierarchy: Arc<FakeTree>,
    pub bindings: Arc<FakeBindings>,
    pub cache: Arc<CompileCache>,
    pub metrics: Arc<CountingMetrics>,
}

impl Stack {
    pub fn new(bindings: Vec<Stored>) -> Self {
        Self::with_builtins(&[], bindings)
    }

    /// A stack with `builtins` (documents named by their built-in ids).
    pub fn with_builtins(builtins: &[Document], bindings: Vec<Stored>) -> Self {
        Self::with_evaluation_timeout(builtins, bindings, Duration::from_secs(5))
    }

    /// A stack whose requests get `evaluation_timeout` of evaluation time.
    pub fn with_evaluation_timeout(
        builtins: &[Document],
        bindings: Vec<Stored>,
        evaluation_timeout: Duration,
    ) -> Self {
        let hierarchy = Arc::new(FakeTree::new(&[
            (ROOT, None, false),
            (PARENT, Some(ROOT), false),
            (CHILD, Some(PARENT), false),
            (SIBLING, Some(ROOT), false),
            (ISLAND, Some(PARENT), true),
        ]));
        let bindings = Arc::new(FakeBindings::new(bindings));
        let cache = Arc::new(CompileCache::new(16));
        let metrics = Arc::new(CountingMetrics::default());
        let service = Arc::new(DecisionService::new(
            BuiltinPolicySet::compile(builtins).unwrap(),
            Arc::clone(&hierarchy) as Arc<dyn HierarchyPort>,
            Arc::clone(&bindings) as Arc<dyn BindingSource>,
            Arc::clone(&cache),
            evaluation_timeout,
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
    ctx(Uuid::from_u128(0x5B), tenant)
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

//! Unit tests for [`TenantResolverDir`].

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use credstore_sdk::TenantId as DomainTenantId;
use tenant_resolver_sdk::TenantResolverClient;
use tenant_resolver_sdk::error::TenantResolverError;
use tenant_resolver_sdk::models::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantRef,
    TenantStatus,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::resolver::TenantDirectory;
use crate::infra::tenant_resolver::TenantResolverDir;

fn make_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .expect("test ctx")
}

fn tenant_ref(id: Uuid) -> TenantRef {
    TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: None,
        self_managed: false,
    }
}

// ── Fake client ───────────────────────────────────────────────────────────────

struct FakeTenantResolverClient {
    child: Uuid,
    parent: Uuid,
    root: Uuid,
    calls: AtomicU32,
    last_barrier: Mutex<Option<BarrierMode>>,
    is_ancestor_last_barrier: Mutex<Option<BarrierMode>>,
}

impl FakeTenantResolverClient {
    fn new(child: Uuid, parent: Uuid, root: Uuid) -> Self {
        Self {
            child,
            parent,
            root,
            calls: AtomicU32::new(0),
            last_barrier: Mutex::new(None),
            is_ancestor_last_barrier: Mutex::new(None),
        }
    }

    fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_barrier_mode(&self) -> Option<BarrierMode> {
        *self.last_barrier.lock().expect("lock")
    }
}

#[async_trait]
impl TenantResolverClient for FakeTenantResolverClient {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<tenant_resolver_sdk::models::TenantInfo, TenantResolverError> {
        unimplemented!()
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<tenant_resolver_sdk::models::TenantInfo, TenantResolverError> {
        unimplemented!()
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<tenant_resolver_sdk::models::TenantInfo>, TenantResolverError> {
        unimplemented!()
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        *self.last_barrier.lock().expect("lock") = Some(options.barrier_mode);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if id.0 == self.child {
            Ok(GetAncestorsResponse {
                tenant: tenant_ref(self.child),
                ancestors: vec![tenant_ref(self.parent), tenant_ref(self.root)],
            })
        } else {
            Err(TenantResolverError::TenantNotFound { tenant_id: id })
        }
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unimplemented!()
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        *self.is_ancestor_last_barrier.lock().expect("lock") = Some(options.barrier_mode);
        // `root` is an ancestor of `child` only across an isolation barrier,
        // so it answers true for `BarrierMode::Ignore` alone.
        if descendant_id.0 == self.child {
            return Ok(ancestor_id.0 == self.parent
                || (ancestor_id.0 == self.root && options.barrier_mode == BarrierMode::Ignore));
        }
        if descendant_id.0 == self.parent {
            return Err(TenantResolverError::Internal("boom".to_owned()));
        }
        Err(TenantResolverError::TenantNotFound {
            tenant_id: descendant_id,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chain_includes_self_then_ancestors() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let root = Uuid::new_v4();

    let client: Arc<dyn TenantResolverClient> =
        Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));
    let ctx = make_ctx();

    let chain = dir
        .ancestor_chain(&ctx, DomainTenantId(child))
        .await
        .expect("ancestor_chain");

    assert_eq!(chain, vec![child, parent, root]);
}

#[tokio::test]
async fn every_call_reads_tenant_resolver() {
    let child = Uuid::new_v4();
    let fake = Arc::new(FakeTenantResolverClient::new(
        child,
        Uuid::new_v4(),
        Uuid::new_v4(),
    ));
    let client: Arc<dyn TenantResolverClient> = Arc::clone(&fake) as _;
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));
    let ctx = make_ctx();

    for _ in 0..2 {
        dir.ancestor_chain(&ctx, DomainTenantId(child))
            .await
            .expect("ancestor chain");
    }

    assert_eq!(fake.call_count(), 2, "the adapter keeps no cache");
}

// `shared` secrets inherit through self-managed (isolation-barrier)
// boundaries by design, so the ancestor chain must be requested with barriers
// ignored — read access is the PDP's decision, not the chain's.
#[tokio::test]
async fn ancestor_chain_requests_barrier_ignoring_mode() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let root = Uuid::new_v4();

    let fake = Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let client: Arc<dyn TenantResolverClient> = Arc::clone(&fake) as _;
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));

    dir.ancestor_chain(&make_ctx(), DomainTenantId(child))
        .await
        .expect("ancestor_chain");

    assert_eq!(
        fake.last_barrier_mode(),
        Some(BarrierMode::Ignore),
        "walk-up must request the full, barrier-ignoring ancestor chain"
    );
}

// A row behind an isolation barrier still holds its reference under the
// creator's hierarchy, so the downward check asks with barriers ignored.
#[tokio::test]
async fn is_ancestor_requests_barrier_ignoring_mode() {
    let (child, parent, root) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let fake = Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let client: Arc<dyn TenantResolverClient> = Arc::clone(&fake) as _;
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));

    let across_barrier = dir
        .is_ancestor(&make_ctx(), DomainTenantId(root), DomainTenantId(child))
        .await
        .expect("is_ancestor");

    assert!(across_barrier, "the barrier-only ancestor is found");
    assert_eq!(
        *fake.is_ancestor_last_barrier.lock().expect("lock"),
        Some(BarrierMode::Ignore)
    );
}

#[tokio::test]
async fn is_ancestor_of_an_unknown_tenant_is_false() {
    let (child, parent, root) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let client: Arc<dyn TenantResolverClient> =
        Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));

    let is = dir
        .is_ancestor(
            &make_ctx(),
            DomainTenantId(root),
            DomainTenantId(Uuid::new_v4()),
        )
        .await
        .expect("TenantNotFound is not an error");

    assert!(!is);
}

#[tokio::test]
async fn is_ancestor_other_failures_are_service_unavailable() {
    let (child, parent, root) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let client: Arc<dyn TenantResolverClient> =
        Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics));

    let err = dir
        .is_ancestor(&make_ctx(), DomainTenantId(root), DomainTenantId(parent))
        .await
        .expect_err("internal error");

    assert!(matches!(
        err,
        crate::domain::error::DomainError::ServiceUnavailable { .. }
    ));
}

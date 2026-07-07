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
}

impl FakeTenantResolverClient {
    fn new(child: Uuid, parent: Uuid, root: Uuid) -> Self {
        Self {
            child,
            parent,
            root,
            calls: AtomicU32::new(0),
            last_barrier: Mutex::new(None),
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
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!()
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
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics), 60);
    let ctx = make_ctx();

    let chain = dir
        .ancestor_chain(&ctx, DomainTenantId(child))
        .await
        .expect("ancestor_chain");

    assert_eq!(chain, vec![child, parent, root]);
}

#[tokio::test]
async fn cache_hit_avoids_second_call() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let root = Uuid::new_v4();

    let fake = Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let client: Arc<dyn TenantResolverClient> = Arc::clone(&fake) as _;
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics), 60);
    let ctx = make_ctx();

    dir.ancestor_chain(&ctx, DomainTenantId(child))
        .await
        .expect("first call");
    dir.ancestor_chain(&ctx, DomainTenantId(child))
        .await
        .expect("second call");

    assert_eq!(
        fake.call_count(),
        1,
        "client should be called only once within TTL"
    );
}

// Secrets must not be inherited across a self-managed tenant boundary, so the
// ancestor chain must be requested in a barrier-respecting mode.
#[tokio::test]
async fn ancestor_chain_requests_barrier_respecting_mode() {
    let child = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let root = Uuid::new_v4();

    let fake = Arc::new(FakeTenantResolverClient::new(child, parent, root));
    let client: Arc<dyn TenantResolverClient> = Arc::clone(&fake) as _;
    let dir = TenantResolverDir::new(client, Arc::new(NoopMetrics), 60);

    dir.ancestor_chain(&make_ctx(), DomainTenantId(child))
        .await
        .expect("ancestor_chain");

    assert_eq!(
        fake.last_barrier_mode(),
        Some(BarrierMode::Respect),
        "walk-up must request a barrier-respecting ancestor chain"
    );
}

//! Unit tests for [`ResolverLiveTenantFilter`] — the registry anchor that
//! decides which ledger-derived tenants a background sweep may still touch.
//! A wrong answer here is expensive in both directions: too generous and the
//! sweep keeps writing rows for tenants that no longer exist (the 29 GB
//! `ledger_reconciliation_run` defect); too strict and a live tenant silently
//! stops being reconciled.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unimplemented,
    reason = "test doubles: unused trait methods are unreachable, assertions unwrap"
)]

use std::sync::Mutex;

use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    IsAncestorOptions, TenantInfo, TenantResolverError,
};

use super::*;

/// A registry double: knows a fixed `id -> status` map and records the size of
/// every batch it was asked about (so chunking is observable).
struct FakeResolver {
    known: Vec<(Uuid, TenantStatus)>,
    batches: Mutex<Vec<usize>>,
    fail: bool,
}

impl FakeResolver {
    fn new(known: Vec<(Uuid, TenantStatus)>) -> Self {
        Self {
            known,
            batches: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            known: Vec::new(),
            batches: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn batches(&self) -> Vec<usize> {
        self.batches.lock().expect("batches lock").clone()
    }
}

#[async_trait]
impl TenantResolverClient for FakeResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not exercised by the live-tenant filter")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unimplemented!("not exercised by the live-tenant filter")
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        if self.fail {
            return Err(TenantResolverError::NoPluginAvailable);
        }
        self.batches.lock().expect("batches lock").push(ids.len());
        let asked: HashSet<Uuid> = ids.iter().map(|id| id.0).collect();
        Ok(self
            .known
            .iter()
            .filter(|(id, status)| {
                asked.contains(id) && (options.status.is_empty() || options.status.contains(status))
            })
            .map(|(id, status)| TenantInfo {
                id: TenantId(*id),
                name: format!("tenant-{id}"),
                status: *status,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            })
            .collect())
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unimplemented!("not exercised by the live-tenant filter")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unimplemented!("not exercised by the live-tenant filter")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!("not exercised by the live-tenant filter")
    }
}

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[tokio::test]
async fn keeps_active_and_suspended_drops_deleted_and_unknown() {
    let active = tenant(1);
    let suspended = tenant(2);
    let deleted = tenant(3);
    // Never registered — the hard-deleted / never-existed case (28% of the
    // stage1 rows), which the registry answers by simply omitting the id.
    let absent = tenant(4);

    let fake = Arc::new(FakeResolver::new(vec![
        (active, TenantStatus::Active),
        (suspended, TenantStatus::Suspended),
        (deleted, TenantStatus::Deleted),
    ]));
    let filter = ResolverLiveTenantFilter::new(Arc::clone(&fake) as Arc<dyn TenantResolverClient>);

    let live = filter
        .live_tenants(&[active, suspended, deleted, absent])
        .await
        .expect("registry read");

    assert_eq!(
        live,
        HashSet::from([active, suspended]),
        "a suspended tenant still owns live money; a deleted / absent one does not"
    );
}

#[tokio::test]
async fn splits_large_candidate_sets_into_bounded_batches() {
    // One and a half chunks: the registry must be asked twice, and the union
    // of both answers returned. Unchunked, the id list would grow one bind
    // parameter per tenant straight into Postgres' 65,535-parameter ceiling.
    let candidates: Vec<Uuid> = (0..LOOKUP_CHUNK + LOOKUP_CHUNK / 2)
        .map(|n| tenant(n as u128 + 1))
        .collect();
    let known = candidates
        .iter()
        .map(|id| (*id, TenantStatus::Active))
        .collect();

    let fake = Arc::new(FakeResolver::new(known));
    let filter = ResolverLiveTenantFilter::new(Arc::clone(&fake) as Arc<dyn TenantResolverClient>);

    let live = filter
        .live_tenants(&candidates)
        .await
        .expect("registry read");

    assert_eq!(live.len(), candidates.len());
    assert_eq!(fake.batches(), vec![LOOKUP_CHUNK, LOOKUP_CHUNK / 2]);
}

#[tokio::test]
async fn empty_candidate_set_never_touches_the_registry() {
    let fake = Arc::new(FakeResolver::new(Vec::new()));
    let filter = ResolverLiveTenantFilter::new(Arc::clone(&fake) as Arc<dyn TenantResolverClient>);

    assert!(filter.live_tenants(&[]).await.expect("no-op").is_empty());
    assert!(fake.batches().is_empty(), "no round trip for an empty set");
}

#[tokio::test]
async fn registry_failure_surfaces_as_an_error_not_an_empty_set() {
    // Critical: an empty `Ok` set means "every candidate is dead". A transient
    // registry fault must NOT be able to mint that answer, or one bad tick
    // would stop reconciling the whole fleet (and, worse, mark it purgeable).
    let fake = Arc::new(FakeResolver::failing());
    let filter = ResolverLiveTenantFilter::new(fake as Arc<dyn TenantResolverClient>);

    assert!(filter.live_tenants(&[tenant(1)]).await.is_err());
}

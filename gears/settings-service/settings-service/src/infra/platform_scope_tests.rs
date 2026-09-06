// Created: 2026-09-06 by Constructor Tech
//! Tests for the hub-backed platform scope.
//!
//! What is pinned: the lookup is deferred to first use and fails as
//! *unavailable* rather than guessing, and a learned id is not asked for twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::HubPlatformScope;
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;

/// A resolver that knows the root and counts how often it is asked.
struct CountingResolver {
    root: Uuid,
    asked: AtomicUsize,
}

#[async_trait]
impl TenantResolverClient for CountingResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("not exercised")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(TenantInfo {
            id: TenantId(self.root),
            name: "root".to_owned(),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        })
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unreachable!("not exercised")
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unreachable!("not exercised")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unreachable!("not exercised")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unreachable!("not exercised")
    }
}

#[tokio::test]
async fn an_unwired_resolver_is_unavailable_not_a_guess() {
    // The resolver is a consumed client wired after init; until it is there,
    // a platform-scoped mutation must be refused as unavailable rather than
    // filed under an invented scope.
    let scope = HubPlatformScope::new(Arc::new(ClientHub::new()));
    match scope.root_tenant().await {
        Err(DomainError::Unavailable { detail }) => {
            assert!(detail.contains("tenant resolver"), "got `{detail}`");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn the_root_tenant_is_asked_for_once() {
    // The root tenant is install-time and undeletable, so the first answer is
    // the only answer; a second lookup would only spend a call.
    let root = Uuid::new_v4();
    let resolver = Arc::new(CountingResolver {
        root,
        asked: AtomicUsize::new(0),
    });
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(resolver.clone());
    let scope = HubPlatformScope::new(hub);

    assert_eq!(scope.root_tenant().await.expect("first"), root);
    assert_eq!(scope.root_tenant().await.expect("second"), root);
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_lookup_is_retried_on_the_next_call() {
    // Nothing is cached on failure: a resolver that was not yet wired at the
    // first mutation must not leave the gear permanently unable to audit.
    let hub = Arc::new(ClientHub::new());
    let scope = HubPlatformScope::new(Arc::clone(&hub));
    assert!(scope.root_tenant().await.is_err());

    let root = Uuid::new_v4();
    hub.register::<dyn TenantResolverClient>(Arc::new(CountingResolver {
        root,
        asked: AtomicUsize::new(0),
    }));
    assert_eq!(scope.root_tenant().await.expect("wired now"), root);
}

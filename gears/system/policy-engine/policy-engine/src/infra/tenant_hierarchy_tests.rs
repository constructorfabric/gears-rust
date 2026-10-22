//! The tenant-resolver adapter's bounds, error mapping and answer checks: the
//! decision path's fail-closed behaviour depends on each.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo,
    TenantResolverClient, TenantResolverError, TenantStatus,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::TenantResolverHierarchy;
use crate::domain::ports::{HierarchyPort, PortError};
use crate::test_support::ctx;

const ROOT: Uuid = Uuid::from_u128(1);
const PARENT: Uuid = Uuid::from_u128(2);
const CHILD: Uuid = Uuid::from_u128(3);

/// How the fake resolver answers `get_ancestors` and `is_ancestor`.
enum Answer {
    /// Never answers.
    Hang,
    /// Fails with the error this builds.
    Fail(fn() -> TenantResolverError),
    /// Answers for `tenant` with `ancestors` (nearest first) and `reachable`.
    Tree {
        tenant: Uuid,
        ancestors: Vec<Uuid>,
        reachable: bool,
    },
}

struct Resolver(Answer);

fn info(id: Uuid) -> TenantInfo {
    TenantInfo {
        id: TenantId(id),
        name: "t".to_owned(),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: None,
        self_managed: false,
    }
}

impl Resolver {
    async fn answer(&self) -> Result<(Uuid, Vec<Uuid>, bool), TenantResolverError> {
        match &self.0 {
            Answer::Hang => std::future::pending().await,
            Answer::Fail(err) => Err(err()),
            Answer::Tree {
                tenant,
                ancestors,
                reachable,
            } => Ok((*tenant, ancestors.clone(), *reachable)),
        }
    }
}

#[async_trait]
impl TenantResolverClient for Resolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("the adapter reads ancestry and reachability only")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("the adapter reads ancestry and reachability only")
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unreachable!("the adapter reads ancestry and reachability only")
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        assert_eq!(options.barrier_mode, BarrierMode::Respect);
        let (tenant, ancestors, _) = self.answer().await?;
        Ok(GetAncestorsResponse {
            tenant: info(tenant).into(),
            ancestors: ancestors.into_iter().map(|a| info(a).into()).collect(),
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unreachable!("the adapter reads ancestry and reachability only")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Ok(self.answer().await?.2)
    }
}

fn adapter(answer: Answer) -> TenantResolverHierarchy {
    TenantResolverHierarchy::new(Arc::new(Resolver(answer)), Duration::from_millis(20))
}

fn caller() -> SecurityContext {
    ctx(Uuid::from_u128(0x5B), PARENT)
}

#[tokio::test]
async fn the_chain_is_the_tenant_then_its_ancestors_nearest_first() {
    let hierarchy = adapter(Answer::Tree {
        tenant: CHILD,
        ancestors: vec![PARENT, ROOT],
        reachable: true,
    });
    assert_eq!(
        hierarchy.ancestors(&caller(), TenantId(CHILD)).await,
        Ok(vec![CHILD, PARENT, ROOT])
    );
}

#[tokio::test]
async fn an_answer_about_another_tenant_is_unavailable_not_trusted() {
    let hierarchy = adapter(Answer::Tree {
        tenant: PARENT,
        ancestors: vec![ROOT],
        reachable: true,
    });
    assert!(matches!(
        hierarchy.ancestors(&caller(), TenantId(CHILD)).await,
        Err(PortError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_slow_resolver_is_a_timeout() {
    let hierarchy = adapter(Answer::Hang);
    assert_eq!(
        hierarchy.ancestors(&caller(), TenantId(CHILD)).await,
        Err(PortError::Timeout)
    );
    assert_eq!(
        hierarchy
            .is_reachable(
                &caller(),
                TenantId(PARENT),
                TenantId(CHILD),
                BarrierMode::Respect
            )
            .await,
        Err(PortError::Timeout)
    );
}

#[tokio::test]
async fn an_unknown_tenant_is_not_found_and_any_other_error_unavailable() {
    let unknown = adapter(Answer::Fail(|| TenantResolverError::TenantNotFound {
        tenant_id: TenantId(CHILD),
    }));
    assert!(matches!(
        unknown.ancestors(&caller(), TenantId(CHILD)).await,
        Err(PortError::NotFound(_))
    ));
    let others: [fn() -> TenantResolverError; 3] = [
        || TenantResolverError::ServiceUnavailable("down".to_owned()),
        || TenantResolverError::Internal("bug".to_owned()),
        || TenantResolverError::NoPluginAvailable,
    ];
    for err in others {
        let failing = adapter(Answer::Fail(err));
        assert!(matches!(
            failing
                .is_reachable(
                    &caller(),
                    TenantId(PARENT),
                    TenantId(CHILD),
                    BarrierMode::Respect
                )
                .await,
            Err(PortError::Unavailable(_))
        ));
    }
}

#[tokio::test]
async fn a_tenant_reaches_itself_without_asking() {
    // A hanging resolver would time out if it were asked.
    let hierarchy = adapter(Answer::Hang);
    assert_eq!(
        hierarchy
            .is_reachable(
                &caller(),
                TenantId(CHILD),
                TenantId(CHILD),
                BarrierMode::Respect
            )
            .await,
        Ok(true)
    );
}

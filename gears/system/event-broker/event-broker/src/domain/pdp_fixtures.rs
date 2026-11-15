//! What the tests that run real policy share: the example tenant tree, a
//! `TenantResolverClient` over it for tr-authz, the real tr-authz and
//! static-authz services wrapped as `AuthZResolverApi`, and the callers.
//!
//! The tree is the issue's example: root `0x123` -> `0x234` -> {`0xC`,
//! self-managed `0xB` -> `0xB1`}, and `0x999`, a second child of `0x123`.
//! `P` (subject `0x456`) is the platform publisher in `0x123`; `U` (subject
//! `0x890`) sits in `0x234`.

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse};
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions,
    GetDescendantsResponse, GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef,
    TenantResolverClient, TenantResolverError, TenantStatus,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

pub(super) const P: Uuid = Uuid::from_u128(0x456);
pub(super) const U: Uuid = Uuid::from_u128(0x890);

pub(super) const ROOT: Uuid = Uuid::from_u128(0x123);
pub(super) const T234: Uuid = Uuid::from_u128(0x234);
pub(super) const C: Uuid = Uuid::from_u128(0xC);
pub(super) const B: Uuid = Uuid::from_u128(0xB);
pub(super) const B1: Uuid = Uuid::from_u128(0xB1);
pub(super) const T999: Uuid = Uuid::from_u128(0x999);

/// The example tree: each tenant, its parent (none for the root) and whether
/// it is self-managed. `PolicyAuthZ` and `InMemoryTenantTree` are both built
/// from it, so the two PDPs walk the same tree.
pub(super) const TREE: [(Uuid, Option<Uuid>, bool); 6] = [
    (ROOT, None, false),
    (T234, Some(ROOT), false),
    (C, Some(T234), false),
    (B, Some(T234), true),
    (B1, Some(B), false),
    (T999, Some(ROOT), false),
];

/// An in-memory `TenantResolverClient` over [`TREE`], following the barrier
/// semantics of the SDK docs for all six methods.
///
/// tr-authz reaches `is_ancestor` through its `is_in_subtree` and
/// `get_descendants` through its `resolve_subtree`; it never calls
/// `get_ancestors`. `the_tree_double_follows_the_resolver_barrier_rules`
/// pins all three against the SDK docs.
#[derive(Debug)]
pub(super) struct InMemoryTenantTree;

impl InMemoryTenantTree {
    fn info(id: TenantId) -> Result<TenantInfo, TenantResolverError> {
        TREE.iter()
            .find(|(tenant, _, _)| *tenant == id.0)
            .map(|&(tenant, parent, self_managed)| TenantInfo {
                id: TenantId(tenant),
                name: format!("T-{tenant}"),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: parent.map(TenantId),
                self_managed,
            })
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })
    }

    fn reference(info: &TenantInfo) -> TenantRef {
        TenantRef {
            id: info.id,
            status: info.status,
            tenant_type: info.tenant_type.clone(),
            parent_id: info.parent_id,
            self_managed: info.self_managed,
        }
    }

    /// The children of `id` that a traversal under `barrier_mode` and the
    /// status filter enters, in tree order.
    fn children(
        id: TenantId,
        barrier_mode: BarrierMode,
        status: &[TenantStatus],
    ) -> Vec<TenantInfo> {
        TREE.iter()
            .filter(|(_, parent, _)| *parent == Some(id.0))
            .filter_map(|&(child, _, _)| Self::info(TenantId(child)).ok())
            .filter(|child| barrier_mode == BarrierMode::Ignore || !child.self_managed)
            .filter(|child| status.is_empty() || status.contains(&child.status))
            .collect()
    }

    fn collect_descendants(
        id: TenantId,
        barrier_mode: BarrierMode,
        status: &[TenantStatus],
        depth: u32,
        max_depth: Option<u32>,
        into: &mut Vec<TenantRef>,
    ) {
        if max_depth.is_some_and(|max| depth > max) {
            return;
        }
        for child in Self::children(id, barrier_mode, status) {
            into.push(Self::reference(&child));
            Self::collect_descendants(child.id, barrier_mode, status, depth + 1, max_depth, into);
        }
    }
}

#[async_trait]
impl TenantResolverClient for InMemoryTenantTree {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Self::info(id)
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Self::info(TenantId(ROOT))
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        let mut found: Vec<TenantInfo> = Vec::new();
        for id in ids {
            if let Ok(info) = Self::info(*id)
                && !found.iter().any(|seen| seen.id == info.id)
                && (options.status.is_empty() || options.status.contains(&info.status))
            {
                found.push(info);
            }
        }
        Ok(found)
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        let tenant = Self::info(id)?;
        let mut ancestors = Vec::new();
        // A barrier hides its own parent chain; an ancestor barrier is listed
        // and ends the chain.
        let mut next = if options.barrier_mode == BarrierMode::Respect && tenant.self_managed {
            None
        } else {
            tenant.parent_id
        };
        while let Some(parent_id) = next {
            let parent = Self::info(parent_id)?;
            ancestors.push(Self::reference(&parent));
            next = if options.barrier_mode == BarrierMode::Respect && parent.self_managed {
                None
            } else {
                parent.parent_id
            };
        }
        Ok(GetAncestorsResponse {
            tenant: Self::reference(&tenant),
            ancestors,
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        let tenant = Self::info(id)?;
        let mut descendants = Vec::new();
        Self::collect_descendants(
            id,
            options.barrier_mode,
            &options.status,
            1,
            options.max_depth,
            &mut descendants,
        );
        Ok(GetDescendantsResponse {
            tenant: Self::reference(&tenant),
            descendants,
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Self::info(ancestor_id)?;
        let descendant = Self::info(descendant_id)?;
        if ancestor_id == descendant_id {
            return Ok(false);
        }
        if options.barrier_mode == BarrierMode::Respect && descendant.self_managed {
            return Ok(false);
        }
        let mut next = descendant.parent_id;
        while let Some(parent_id) = next {
            if parent_id == ancestor_id {
                return Ok(true);
            }
            let parent = Self::info(parent_id)?;
            if options.barrier_mode == BarrierMode::Respect && parent.self_managed {
                return Ok(false);
            }
            next = parent.parent_id;
        }
        Ok(false)
    }
}

/// The real tr-authz service over the example tree.
pub(super) struct TrAuthz(pub(super) tr_authz_plugin::domain::Service);

#[async_trait]
impl AuthZResolverApi for TrAuthz {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(self.0.evaluate(&request).await)
    }
}

/// The real static-authz service.
pub(super) struct StaticAuthz(pub(super) static_authz_plugin::domain::Service);

#[async_trait]
impl AuthZResolverApi for StaticAuthz {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(self.0.evaluate(&request))
    }
}

pub(super) fn caller(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("valid security context")
}

/// The SDK docs' barrier rules (`TenantResolverClient`, `is_ancestor`,
/// `get_descendants`, `get_ancestors`) over the example tree, for both
/// barrier modes. Descendants are compared sorted by id: the SDK does not
/// fix their order.
#[tokio::test]
async fn the_tree_double_follows_the_resolver_barrier_rules() {
    let ctx = SecurityContext::anonymous();
    let tree = InMemoryTenantTree;
    // The expected references, written from `TREE`: every tenant is active
    // and untyped.
    let reference = |id: Uuid, parent: Option<Uuid>, self_managed: bool| TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(TenantId),
        self_managed,
    };

    let mut ancestry = Vec::new();
    for (ancestor, descendant, barrier_mode) in [
        (T234, C, BarrierMode::Respect),
        (T234, B, BarrierMode::Respect),
        (T234, B1, BarrierMode::Respect),
        (T234, B1, BarrierMode::Ignore),
        (T234, T234, BarrierMode::Respect),
        (T234, T234, BarrierMode::Ignore),
        (T234, T999, BarrierMode::Ignore),
        (ROOT, C, BarrierMode::Respect),
    ] {
        let answer = tree
            .is_ancestor(
                &ctx,
                TenantId(ancestor),
                TenantId(descendant),
                &IsAncestorOptions { barrier_mode },
            )
            .await
            .expect("both tenants are in the tree");
        ancestry.push((ancestor, descendant, barrier_mode, answer));
    }
    assert_eq!(
        ancestry,
        vec![
            (T234, C, BarrierMode::Respect, true),
            (T234, B, BarrierMode::Respect, false),
            (T234, B1, BarrierMode::Respect, false),
            (T234, B1, BarrierMode::Ignore, true),
            (T234, T234, BarrierMode::Respect, false),
            (T234, T234, BarrierMode::Ignore, false),
            (T234, T999, BarrierMode::Ignore, false),
            (ROOT, C, BarrierMode::Respect, true),
        ],
        "is_ancestor"
    );

    let mut descendants = Vec::new();
    for barrier_mode in [BarrierMode::Respect, BarrierMode::Ignore] {
        let mut response = tree
            .get_descendants(
                &ctx,
                TenantId(T234),
                &GetDescendantsOptions {
                    barrier_mode,
                    ..GetDescendantsOptions::default()
                },
            )
            .await
            .expect("0x234 is in the tree");
        response.descendants.sort_by_key(|tenant| tenant.id.0);
        descendants.push((barrier_mode, response));
    }
    assert_eq!(
        descendants,
        vec![
            (
                BarrierMode::Respect,
                GetDescendantsResponse {
                    tenant: reference(T234, Some(ROOT), false),
                    descendants: vec![reference(C, Some(T234), false)],
                },
            ),
            (
                BarrierMode::Ignore,
                GetDescendantsResponse {
                    tenant: reference(T234, Some(ROOT), false),
                    descendants: vec![
                        reference(B, Some(T234), true),
                        reference(C, Some(T234), false),
                        reference(B1, Some(B), false)
                    ],
                },
            ),
        ],
        "get_descendants of 0x234"
    );

    let mut ancestors = Vec::new();
    for (tenant, barrier_mode) in [
        (B1, BarrierMode::Respect),
        (B, BarrierMode::Respect),
        (B1, BarrierMode::Ignore),
    ] {
        let response = tree
            .get_ancestors(
                &ctx,
                TenantId(tenant),
                &GetAncestorsOptions { barrier_mode },
            )
            .await
            .expect("the tenant is in the tree");
        ancestors.push((barrier_mode, response));
    }
    assert_eq!(
        ancestors,
        vec![
            (
                BarrierMode::Respect,
                GetAncestorsResponse {
                    tenant: reference(B1, Some(B), false),
                    ancestors: vec![reference(B, Some(T234), true)],
                },
            ),
            (
                BarrierMode::Respect,
                GetAncestorsResponse {
                    tenant: reference(B, Some(T234), true),
                    ancestors: vec![],
                },
            ),
            (
                BarrierMode::Ignore,
                GetAncestorsResponse {
                    tenant: reference(B1, Some(B), false),
                    ancestors: vec![
                        reference(B, Some(T234), true),
                        reference(T234, Some(ROOT), false),
                        reference(ROOT, None, false)
                    ],
                },
            ),
        ],
        "get_ancestors"
    );
}

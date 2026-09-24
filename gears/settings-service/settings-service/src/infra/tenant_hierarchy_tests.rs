// Created: 2026-09-17 by Virtuozzo International GmbH
//! Tests for the hub-backed tenant hierarchy.
//!
//! The resolver answers in its own shape and the walk needs another: parents
//! come back nearest-first and the chain wants root-first, descendants come
//! back as a flat set and the impact walk wants them level by level. What is
//! pinned here is that reshaping, the one question answered without a call at
//! all, and that an unwired resolver is unavailability rather than an
//! invented tree.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::HubTenantHierarchy;
use crate::domain::error::DomainError;
use crate::domain::resolution::TenantHierarchy;

fn tenant_ref(id: Uuid, parent: Option<Uuid>) -> TenantRef {
    TenantRef {
        id: TenantId(id),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(TenantId),
        self_managed: false,
    }
}

/// How the fake answers: from its tree, or not at all.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Knows {
    #[default]
    The,
    Nothing,
}

/// A resolver that answers from a fixed tree and counts what it was asked.
#[derive(Default)]
struct FakeResolver {
    /// Nearest parent first, as the resolver answers.
    ancestors: Vec<TenantRef>,
    descendants: Vec<TenantRef>,
    self_managed: bool,
    is_ancestor: bool,
    knows: Knows,
    asked: AtomicUsize,
    /// The `max_depth` the last descendants request carried; `None` when it
    /// asked for the whole tree.
    depth_asked: std::sync::Mutex<Option<u32>>,
}

fn not_found() -> TenantResolverError {
    TenantResolverError::TenantNotFound {
        tenant_id: TenantId(Uuid::nil()),
    }
}

#[async_trait]
impl TenantResolverClient for FakeResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.knows == Knows::Nothing {
            return Err(not_found());
        }
        Ok(TenantInfo {
            id,
            name: "a tenant".to_owned(),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: self.self_managed,
        })
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("not exercised")
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
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        assert!(
            matches!(
                options.barrier_mode,
                tenant_resolver_sdk::BarrierMode::Ignore
            ),
            "runtime resolution walks through a barrier: a standalone tenant \
             still inherits the platform's defaults"
        );
        if self.knows == Knows::Nothing {
            return Err(not_found());
        }
        Ok(GetAncestorsResponse {
            tenant: tenant_ref(id.0, None),
            ancestors: self.ancestors.clone(),
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        assert!(
            matches!(
                options.barrier_mode,
                tenant_resolver_sdk::BarrierMode::Respect
            ),
            "administration stops at a barrier"
        );
        *self.depth_asked.lock().expect("lock") = options.max_depth;
        if self.knows == Knows::Nothing {
            return Err(not_found());
        }
        Ok(GetDescendantsResponse {
            tenant: tenant_ref(id.0, None),
            descendants: self.descendants.clone(),
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        assert!(
            matches!(
                options.barrier_mode,
                tenant_resolver_sdk::BarrierMode::Respect
            ),
            "administration from above cannot reach past a barrier"
        );
        if self.knows == Knows::Nothing {
            return Err(not_found());
        }
        Ok(self.is_ancestor)
    }
}

fn over(resolver: FakeResolver) -> (HubTenantHierarchy, Arc<FakeResolver>) {
    let resolver = Arc::new(resolver);
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::clone(&resolver) as Arc<dyn TenantResolverClient>);
    (HubTenantHierarchy::new(hub), resolver)
}

#[tokio::test]
async fn the_chain_is_turned_root_first_and_ends_at_the_tenant_itself() {
    // The resolver answers nearest parent first; effective-value resolution
    // walks the other way, applying the root's value before anything nearer.
    // The requested tenant is not in the resolver's answer and has to be the
    // last element, or its own override would never be considered.
    let root = Uuid::new_v4();
    let middle = Uuid::new_v4();
    let leaf = Uuid::new_v4();
    let (hierarchy, _) = over(FakeResolver {
        ancestors: vec![tenant_ref(middle, Some(root)), tenant_ref(root, None)],
        ..FakeResolver::default()
    });

    assert_eq!(
        hierarchy.chain(leaf).await.expect("a chain"),
        vec![root, middle, leaf]
    );
}

#[tokio::test]
async fn the_chain_of_the_root_is_the_root_alone() {
    let root = Uuid::new_v4();
    let (hierarchy, _) = over(FakeResolver::default());
    assert_eq!(hierarchy.chain(root).await.expect("a chain"), vec![root]);
}

#[tokio::test]
async fn a_tenant_is_within_its_own_subtree_without_asking_anyone() {
    // The commonest question of the write path, and the resolver has nothing
    // to add to it.
    let tenant = Uuid::new_v4();
    let (hierarchy, resolver) = over(FakeResolver::default());

    assert!(
        hierarchy
            .is_within_subtree(tenant, tenant)
            .await
            .expect("an answer")
    );
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_different_tenant_is_referred_to_the_resolver() {
    let (hierarchy, resolver) = over(FakeResolver {
        is_ancestor: true,
        ..FakeResolver::default()
    });
    assert!(
        hierarchy
            .is_within_subtree(Uuid::new_v4(), Uuid::new_v4())
            .await
            .expect("an answer")
    );
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 1);

    let (hierarchy, _) = over(FakeResolver::default());
    assert!(
        !hierarchy
            .is_within_subtree(Uuid::new_v4(), Uuid::new_v4())
            .await
            .expect("an answer")
    );
}

#[tokio::test]
async fn a_barrier_is_the_resolvers_self_managed_flag() {
    let (hierarchy, _) = over(FakeResolver {
        self_managed: true,
        ..FakeResolver::default()
    });
    assert!(
        hierarchy
            .is_standalone(Uuid::new_v4())
            .await
            .expect("an answer")
    );

    let (hierarchy, _) = over(FakeResolver::default());
    assert!(
        !hierarchy
            .is_standalone(Uuid::new_v4())
            .await
            .expect("an answer")
    );
}

#[tokio::test]
async fn the_descendants_are_the_set_the_resolver_answers_in_breadth_first_order() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let (hierarchy, _) = over(FakeResolver {
        descendants: vec![tenant_ref(a, Some(parent)), tenant_ref(b, Some(a))],
        ..FakeResolver::default()
    });

    let (order, truncated) = hierarchy
        .descendants_bfs(parent, 100)
        .await
        .expect("descendants");
    assert_eq!(order, vec![a, b]);
    assert!(!truncated, "well within the budget and the ceiling");
}

#[tokio::test]
async fn the_breadth_first_walk_is_rebuilt_from_the_parent_links() {
    // The impact report shows the nearest affected scopes first, so the flat
    // set the resolver answers has to be re-ordered level by level. Depth-first
    // order would put one deep branch ahead of every direct child.
    let root = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let a_child = Uuid::new_v4();
    let b_child = Uuid::new_v4();
    // Deliberately answered deepest-branch-first, which is what the walk must
    // not reproduce.
    let (hierarchy, _) = over(FakeResolver {
        descendants: vec![
            tenant_ref(a_child, Some(a)),
            tenant_ref(a, Some(root)),
            tenant_ref(b_child, Some(b)),
            tenant_ref(b, Some(root)),
        ],
        ..FakeResolver::default()
    });

    let (order, truncated) = hierarchy
        .descendants_bfs(root, 100)
        .await
        .expect("a walk order");
    assert!(!truncated);
    assert_eq!(
        &order[..2],
        &[a, b],
        "the direct children come first: {order:?}"
    );
    assert_eq!(order.len(), 4);
    assert!(order[2..].contains(&a_child) && order[2..].contains(&b_child));
}

#[tokio::test]
async fn the_walk_stops_at_its_budget_and_says_so() {
    let root = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let (hierarchy, _) = over(FakeResolver {
        descendants: vec![tenant_ref(a, Some(root)), tenant_ref(b, Some(root))],
        ..FakeResolver::default()
    });

    let (order, truncated) = hierarchy
        .descendants_bfs(root, 1)
        .await
        .expect("a walk order");
    assert_eq!(order, vec![a]);
    assert!(truncated, "the report reads as `at least this many`");
}

#[tokio::test]
async fn the_resolver_is_asked_for_a_bounded_depth_and_the_ceiling_marks_truncation() {
    // The SDK bounds depth, not count: the request carries the ceiling, and a
    // node found at the ceiling may have children below it, so the walk says
    // it was cut.
    let root = Uuid::new_v4();
    let mut chain = Vec::new();
    let mut parent = root;
    for _ in 0..super::SUBTREE_DEPTH_CEILING {
        let next = Uuid::new_v4();
        chain.push(tenant_ref(next, Some(parent)));
        parent = next;
    }
    let (hierarchy, resolver) = over(FakeResolver {
        descendants: chain,
        ..FakeResolver::default()
    });

    let (order, truncated) = hierarchy
        .descendants_bfs(root, 10_000)
        .await
        .expect("a walk order");
    assert_eq!(
        *resolver.depth_asked.lock().expect("lock"),
        Some(super::SUBTREE_DEPTH_CEILING),
        "the request itself is bounded"
    );
    assert_eq!(order.len(), super::SUBTREE_DEPTH_CEILING as usize);
    assert!(truncated, "a node at the ceiling may hide children");

    // A tree shallower than the ceiling is whole, and says so.
    let (hierarchy, _) = over(FakeResolver {
        descendants: vec![tenant_ref(Uuid::new_v4(), Some(root))],
        ..FakeResolver::default()
    });
    let (_, truncated) = hierarchy
        .descendants_bfs(root, 10_000)
        .await
        .expect("a walk order");
    assert!(!truncated);
}

#[tokio::test]
async fn a_tenant_the_resolver_does_not_know_is_absent_not_unavailable() {
    // The two are read differently upstream: absence is the caller's problem,
    // unavailability is the platform's.
    let (hierarchy, _) = over(FakeResolver {
        knows: Knows::Nothing,
        ..FakeResolver::default()
    });
    assert!(matches!(
        hierarchy.chain(Uuid::new_v4()).await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn an_unwired_resolver_is_unavailable_on_every_question() {
    // The resolver is wired after init, so until it is there the hierarchy has
    // nothing to answer from — and must say so rather than assume a flat tree,
    // which would make every tenant its own root.
    let hierarchy = HubTenantHierarchy::new(Arc::new(ClientHub::new()));
    let tenant = Uuid::new_v4();
    let refusals = [
        hierarchy.chain(tenant).await.err(),
        hierarchy
            .is_within_subtree(tenant, Uuid::new_v4())
            .await
            .err(),
        hierarchy.is_standalone(tenant).await.err(),
        hierarchy.descendants_bfs(tenant, 10).await.err(),
    ];
    for refusal in refusals {
        match refusal.expect("an unwired resolver refuses") {
            DomainError::Unavailable { detail } => {
                assert!(detail.contains("tenant resolver"), "got `{detail}`");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_duplicate_or_cyclic_parent_link_is_walked_once() {
    // Malformed hierarchy data — a child named under two parents, a pair
    // naming each other — must not fill the order with repeats up to the
    // budget: the walk emits distinct tenants, and a cycle is not truncation.
    let root = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let (hierarchy, _) = over(FakeResolver {
        descendants: vec![
            tenant_ref(a, Some(root)),
            tenant_ref(b, Some(a)),
            tenant_ref(a, Some(b)),
            tenant_ref(b, Some(root)),
        ],
        ..FakeResolver::default()
    });

    let (order, truncated) = hierarchy
        .descendants_bfs(root, 100)
        .await
        .expect("descendants");
    assert_eq!(order, vec![a, b], "each tenant once, nearest level first");
    assert!(
        !truncated,
        "two distinct tenants are well within the budget"
    );
}

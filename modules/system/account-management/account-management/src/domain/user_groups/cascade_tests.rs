//! Unit tests for [`super::cascade::build_cascade_cleanup_hook`].
//!
//! Each test wires the cascade hook against a [`FakeCascadeRgClient`]
//! that returns canned responses for `list_groups` and
//! `delete_group_cascade`. Pins:
//!
//! * Empty tenant (no groups) → success no-op.
//! * Single group → one cascade call → success.
//! * Two listed (sibling-root) groups → each cascade-called exactly
//!   once (RG handles subtree ordering internally; AM is single-pass).
//! * Group `NotFound` during cascade → treated as already-deleted
//!   (a parent's cascade may have eaten the descendant already).
//! * RG unavailable during list → `Retryable`.
//! * RG unavailable during cascade → `Retryable`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use modkit_odata::page::PageInfo;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, GroupHierarchy, ResourceGroup, ResourceGroupClient,
    ResourceGroupError, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
    UpdateGroupRequest, UpdateTypeRequest,
};
use uuid::Uuid;

use super::USER_GROUP_TYPE_CODE;
use super::cascade::build_cascade_cleanup_hook;
use crate::domain::tenant::hooks::HookError;

// ---- fake client ---------------------------------------------------

type ListGroupsFn = Box<dyn Fn() -> Result<Page<ResourceGroup>, ResourceGroupError> + Send + Sync>;
type DeleteCascadeFn = Box<dyn Fn(Uuid) -> Result<(), ResourceGroupError> + Send + Sync>;

struct FakeCascadeRgClient {
    list_groups_fn: ListGroupsFn,
    delete_cascade_fn: DeleteCascadeFn,
    cascade_calls: AtomicUsize,
}

impl FakeCascadeRgClient {
    fn empty() -> Self {
        Self {
            list_groups_fn: Box::new(|| Ok(Page::empty(100))),
            delete_cascade_fn: Box::new(|_| Ok(())),
            cascade_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ResourceGroupClient for FakeCascadeRgClient {
    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError> {
        (self.list_groups_fn)()
    }

    async fn delete_group_cascade(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ResourceGroupError> {
        self.cascade_calls.fetch_add(1, Ordering::SeqCst);
        (self.delete_cascade_fn)(id)
    }

    // -- Unreachable in the cascade path --

    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError> {
        unreachable!("cascade hook no longer drains memberships -- RG cascade handles them")
    }
    async fn remove_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<(), ResourceGroupError> {
        unreachable!("cascade hook no longer removes memberships -- RG cascade handles them")
    }
    async fn delete_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), ResourceGroupError> {
        unreachable!("cascade hook only calls delete_group_cascade, not delete_group")
    }
    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        _request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!()
    }
    async fn get_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!()
    }
    async fn list_types(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, ResourceGroupError> {
        unreachable!()
    }
    async fn update_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
        _request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        unreachable!()
    }
    async fn delete_type(
        &self,
        _ctx: &SecurityContext,
        _code: &str,
    ) -> Result<(), ResourceGroupError> {
        unreachable!()
    }
    async fn create_group(
        &self,
        _ctx: &SecurityContext,
        _request: CreateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn update_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError> {
        unreachable!()
    }
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        unreachable!()
    }
    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        unreachable!()
    }
    async fn add_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<ResourceGroupMembership, ResourceGroupError> {
        unreachable!()
    }
}

// ---- helpers -------------------------------------------------------

const TENANT_ID: Uuid = Uuid::from_u128(0xAAAA_0001);

fn make_group(id: u128) -> ResourceGroup {
    ResourceGroup {
        id: Uuid::from_u128(id),
        code: USER_GROUP_TYPE_CODE.to_owned(),
        name: format!("group-{id}"),
        hierarchy: GroupHierarchy {
            parent_id: None,
            tenant_id: TENANT_ID,
        },
        metadata: None,
    }
}

fn groups_page(groups: Vec<ResourceGroup>) -> Page<ResourceGroup> {
    Page::new(
        groups,
        PageInfo {
            next_cursor: None,
            prev_cursor: None,
            limit: 100,
        },
    )
}

// ---- tests ---------------------------------------------------------

#[tokio::test]
async fn empty_tenant_returns_ok() {
    let client: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::new(FakeCascadeRgClient::empty());
    let hook = build_cascade_cleanup_hook(client);
    let result = hook(TENANT_ID).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn single_group_deletes_via_cascade() {
    let g = make_group(1);
    let fake = FakeCascadeRgClient {
        list_groups_fn: {
            let g = g.clone();
            Box::new(move || Ok(groups_page(vec![g.clone()])))
        },
        delete_cascade_fn: Box::new(|_| Ok(())),
        cascade_calls: AtomicUsize::new(0),
    };
    let client: Arc<FakeCascadeRgClient> = Arc::new(fake);
    let client_dyn: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::clone(&client) as _;
    let hook = build_cascade_cleanup_hook(client_dyn);
    let result = hook(TENANT_ID).await;
    assert!(result.is_ok());
    assert_eq!(
        client.cascade_calls.load(Ordering::SeqCst),
        1,
        "exactly one delete_group_cascade call per listed group"
    );
}

#[tokio::test]
async fn two_listed_groups_each_cascade_called_exactly_once() {
    // RG's `delete_group_cascade` is itself recursive on the RG side
    // (force=true tears down the subtree atomically). AM is therefore
    // single-pass: each group returned by `list_groups` is dispatched
    // to cascade exactly once. There is no leaf-first retry loop --
    // the old AM-side multi-pass algorithm has moved to RG.
    //
    // Both fixtures land as sibling roots (`make_group` builds rows
    // with `parent_id: None`); production `fetch_tenant_groups`
    // filters to `parent_id IS NULL` so this matches the real shape.
    //
    // Asserting only `cascade_calls == 2` is too weak -- it would pass
    // for any 2-call sequence including `[g1, g1]`. Record the exact
    // group_ids passed so a regression that called the same id twice
    // surfaces.
    let g1 = make_group(1);
    let g2 = make_group(2);
    let g1_id = g1.id;
    let g2_id = g2.id;

    let called: Arc<parking_lot::Mutex<Vec<Uuid>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let called_clone = Arc::clone(&called);

    let fake = FakeCascadeRgClient {
        list_groups_fn: {
            let g1 = g1.clone();
            let g2 = g2.clone();
            Box::new(move || Ok(groups_page(vec![g1.clone(), g2.clone()])))
        },
        delete_cascade_fn: Box::new(move |id| {
            called_clone.lock().push(id);
            Ok(())
        }),
        cascade_calls: AtomicUsize::new(0),
    };

    let client: Arc<FakeCascadeRgClient> = Arc::new(fake);
    let client_dyn: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::clone(&client) as _;
    let hook = build_cascade_cleanup_hook(client_dyn);
    let result = hook(TENANT_ID).await;
    assert!(result.is_ok());

    // Two listed groups → exactly two cascade calls. Anything more
    // would indicate a regression to a multi-pass retry loop.
    assert_eq!(
        client.cascade_calls.load(Ordering::SeqCst),
        2,
        "expected one cascade call per listed group (no retries)"
    );

    // The set of group_ids cascade-called must be exactly {g1, g2}.
    // Locks the "both groups received a call" invariant, blocking a
    // regression where the same id is passed twice.
    let called_set: std::collections::HashSet<Uuid> = called.lock().iter().copied().collect();
    assert_eq!(
        called_set,
        std::collections::HashSet::from([g1_id, g2_id]),
        "every listed group must receive its own cascade call"
    );
}

#[tokio::test]
async fn group_not_found_during_cascade_treated_as_already_deleted() {
    let g = make_group(1);
    let fake = FakeCascadeRgClient {
        list_groups_fn: {
            let g = g.clone();
            Box::new(move || Ok(groups_page(vec![g.clone()])))
        },
        delete_cascade_fn: Box::new(|id| Err(ResourceGroupError::not_found(id.to_string()))),
        cascade_calls: AtomicUsize::new(0),
    };
    let client: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::new(fake);
    let hook = build_cascade_cleanup_hook(client);
    let result = hook(TENANT_ID).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn list_groups_unavailable_returns_retryable() {
    let fake = FakeCascadeRgClient {
        list_groups_fn: Box::new(|| {
            Err(ResourceGroupError::service_unavailable(
                "connection refused",
            ))
        }),
        delete_cascade_fn: Box::new(|_| Ok(())),
        cascade_calls: AtomicUsize::new(0),
    };
    let client: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::new(fake);
    let hook = build_cascade_cleanup_hook(client);
    let result = hook(TENANT_ID).await;
    assert!(matches!(result, Err(HookError::Retryable { .. })));
}

#[tokio::test]
async fn cascade_error_returns_retryable() {
    let g = make_group(1);
    let fake = FakeCascadeRgClient {
        list_groups_fn: {
            let g = g.clone();
            Box::new(move || Ok(groups_page(vec![g.clone()])))
        },
        delete_cascade_fn: Box::new(|_| Err(ResourceGroupError::internal())),
        cascade_calls: AtomicUsize::new(0),
    };
    let client: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::new(fake);
    let hook = build_cascade_cleanup_hook(client);
    let result = hook(TENANT_ID).await;
    assert!(matches!(result, Err(HookError::Retryable { .. })));
}

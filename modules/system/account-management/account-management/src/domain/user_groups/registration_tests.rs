//! Unit tests for [`super::registration::register_user_group_types`].
//!
//! The post-Path-D flow registers **two** RG types in fixed order:
//! [`USER_MEMBERSHIP_TYPE`] (the AM-user member handle) first, then
//! [`USER_GROUP_TYPE_CODE`] (the user-group container). The fake
//! ([`FakeRgClient`]) is therefore **per-code** -- callers script the
//! behaviour of each `get_type` / `create_type` independently. Tests
//! that only care about one of the two types leave the other in its
//! default "absent -> register cleanly" state.
//!
//! Pins:
//!
//! * Both types registered when both absent.
//! * Member-handle missing / divergent surfaces BEFORE the container
//!   call (caller never proceeds past a failing step-1).
//! * Container divergent / missing-membership-type surfaces with the
//!   tightened `classify_existing` equivalence check.
//! * `TypeAlreadyExists` race is re-read per-type via `get_type` and
//!   classified against the same spec (no silent swallow).
//! * Transport errors from `get_type` / `create_type` collapse to
//!   `ServiceUnavailable` on the first failing step.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup, ResourceGroupClient, ResourceGroupError,
    ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest,
    UpdateTypeRequest,
};
use uuid::Uuid;

use super::registration::{RegistrationError, RegistrationOutcome, register_user_group_types};
use super::{USER_GROUP_TYPE_CODE, USER_MEMBERSHIP_TYPE};

// ---- fake client ---------------------------------------------------

enum GetTypeBehaviour {
    Return(ResourceGroupType),
    NotFound,
    Error(ResourceGroupError),
    /// Sequence: every call advances; out-of-range calls reuse the
    /// last entry. Used by the `TypeAlreadyExists` race-path tests
    /// where the first `get_type` returns `NotFound` (we believe
    /// the type is absent) and a second call after a peer's race
    /// returns whatever the peer wrote (equivalent or divergent).
    Sequence(Vec<GetTypeBehaviour>, AtomicUsize),
}

#[derive(Clone)]
enum CreateTypeBehaviour {
    Ok,
    AlreadyExists,
    Error(ResourceGroupError),
}

struct TypeState {
    get_type: GetTypeBehaviour,
    create_type: CreateTypeBehaviour,
}

impl TypeState {
    /// Default: type absent, `create_type` succeeds. Used as the
    /// no-op fixture for the step the test is not exercising.
    fn absent_then_create() -> Self {
        Self {
            get_type: GetTypeBehaviour::NotFound,
            create_type: CreateTypeBehaviour::Ok,
        }
    }

    fn already_present(row: ResourceGroupType) -> Self {
        Self {
            get_type: GetTypeBehaviour::Return(row),
            create_type: CreateTypeBehaviour::Ok,
        }
    }

    fn race_with_reread(peer_row: ResourceGroupType) -> Self {
        Self {
            get_type: GetTypeBehaviour::Sequence(
                vec![
                    GetTypeBehaviour::NotFound,
                    GetTypeBehaviour::Return(peer_row),
                ],
                AtomicUsize::new(0),
            ),
            create_type: CreateTypeBehaviour::AlreadyExists,
        }
    }

    fn get_type_unavailable() -> Self {
        Self {
            get_type: GetTypeBehaviour::Error(ResourceGroupError::ServiceUnavailable {
                message: "connection refused".to_owned(),
            }),
            create_type: CreateTypeBehaviour::Ok,
        }
    }

    fn create_type_unavailable() -> Self {
        Self {
            get_type: GetTypeBehaviour::NotFound,
            create_type: CreateTypeBehaviour::Error(ResourceGroupError::ServiceUnavailable {
                message: "connection refused".to_owned(),
            }),
        }
    }
}

struct FakeRgClient {
    states: HashMap<&'static str, TypeState>,
    /// Order in which `create_type` was observed -- lets tests assert
    /// that the member handle was registered before the container.
    create_order: Mutex<Vec<String>>,
}

impl FakeRgClient {
    /// Default fixture: both types absent, both register cleanly.
    fn defaults() -> Self {
        let mut states: HashMap<&'static str, TypeState> = HashMap::new();
        states.insert(USER_MEMBERSHIP_TYPE, TypeState::absent_then_create());
        states.insert(USER_GROUP_TYPE_CODE, TypeState::absent_then_create());
        Self {
            states,
            create_order: Mutex::new(Vec::new()),
        }
    }

    /// Override the state of one type while leaving the other at the
    /// default "absent then create" fixture.
    fn with(mut self, code: &'static str, state: TypeState) -> Self {
        self.states.insert(code, state);
        self
    }

    /// Row matching the spec the registration algorithm submits for
    /// the user-group container: `can_be_root=true`, self-parent,
    /// member-handle in `allowed_membership_types`.
    fn equivalent_container_row() -> ResourceGroupType {
        ResourceGroupType {
            code: USER_GROUP_TYPE_CODE.to_owned(),
            can_be_root: true,
            allowed_parent_types: vec![USER_GROUP_TYPE_CODE.to_owned()],
            allowed_membership_types: vec![USER_MEMBERSHIP_TYPE.to_owned()],
            metadata_schema: None,
        }
    }

    /// Row matching the spec for the user member handle.
    fn equivalent_member_row() -> ResourceGroupType {
        ResourceGroupType {
            code: USER_MEMBERSHIP_TYPE.to_owned(),
            can_be_root: true,
            allowed_parent_types: Vec::new(),
            allowed_membership_types: Vec::new(),
            metadata_schema: None,
        }
    }

    fn divergent_container_can_be_root_false() -> ResourceGroupType {
        ResourceGroupType {
            can_be_root: false,
            ..Self::equivalent_container_row()
        }
    }

    fn divergent_container_missing_self_parent() -> ResourceGroupType {
        ResourceGroupType {
            allowed_parent_types: Vec::new(),
            ..Self::equivalent_container_row()
        }
    }

    fn divergent_container_missing_member_type() -> ResourceGroupType {
        ResourceGroupType {
            allowed_membership_types: Vec::new(),
            ..Self::equivalent_container_row()
        }
    }

    /// Container row that satisfies the predicate AND additionally
    /// admits an extra parent / membership type that AM did not
    /// request. Pins the looseness contract: RG is allowed to seed
    /// broader policies AM does not control, so extras are accepted.
    fn loose_container_row(extra_parents: &[&str], extra_members: &[&str]) -> ResourceGroupType {
        let mut row = Self::equivalent_container_row();
        for p in extra_parents {
            row.allowed_parent_types.push((*p).to_owned());
        }
        for m in extra_members {
            row.allowed_membership_types.push((*m).to_owned());
        }
        row
    }
}

#[async_trait]
impl ResourceGroupClient for FakeRgClient {
    async fn get_type(
        &self,
        _ctx: &SecurityContext,
        code: &str,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        fn dispatch(
            b: &GetTypeBehaviour,
            code: &str,
        ) -> Result<ResourceGroupType, ResourceGroupError> {
            match b {
                GetTypeBehaviour::Return(t) => Ok(t.clone()),
                GetTypeBehaviour::Error(e) => Err(e.clone()),
                // `NotFound` is the genuine "no row" result; nested
                // `Sequence` is a misuse (caller should flatten) but
                // collapsing to `NotFound` keeps the fake observable
                // rather than panicking inside a test fixture.
                GetTypeBehaviour::NotFound | GetTypeBehaviour::Sequence(_, _) => {
                    Err(ResourceGroupError::not_found(code))
                }
            }
        }
        let state = self
            .states
            .get(code)
            .ok_or_else(|| ResourceGroupError::not_found(code))?;
        match &state.get_type {
            GetTypeBehaviour::Sequence(steps, idx) => {
                let i = idx.fetch_add(1, Ordering::SeqCst);
                let step = steps
                    .get(i)
                    .or_else(|| steps.last())
                    .ok_or_else(|| ResourceGroupError::not_found(code))?;
                dispatch(step, code)
            }
            other => dispatch(other, code),
        }
    }

    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError> {
        // Best-effort observability: out-of-band record the order of
        // `create_type` calls. Lock-poison failure aborts the test
        // with a clear cause rather than silently corrupting state.
        self.create_order
            .lock()
            .expect("create_order lock not poisoned")
            .push(request.code.clone());
        let state = self
            .states
            .get(request.code.as_str())
            .ok_or_else(|| ResourceGroupError::not_found(&request.code))?;
        match &state.create_type {
            CreateTypeBehaviour::Ok => Ok(ResourceGroupType {
                code: request.code,
                can_be_root: request.can_be_root,
                allowed_parent_types: request.allowed_parent_types,
                allowed_membership_types: request.allowed_membership_types,
                metadata_schema: request.metadata_schema,
            }),
            CreateTypeBehaviour::AlreadyExists => {
                Err(ResourceGroupError::type_already_exists(&request.code))
            }
            CreateTypeBehaviour::Error(e) => Err(e.clone()),
        }
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
    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError> {
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
    async fn delete_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), ResourceGroupError> {
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
    async fn remove_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<(), ResourceGroupError> {
        unreachable!()
    }
    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError> {
        unreachable!()
    }
}

// ---- helpers -------------------------------------------------------

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

fn into_client(c: FakeRgClient) -> Arc<dyn ResourceGroupClient + Send + Sync> {
    Arc::new(c)
}

// ---- happy paths ---------------------------------------------------

#[tokio::test]
async fn both_types_absent_registers_both_in_member_then_container_order() {
    let arc: Arc<FakeRgClient> = Arc::new(FakeRgClient::defaults());
    let dyn_arc: Arc<dyn ResourceGroupClient + Send + Sync> = arc.clone();
    let result = register_user_group_types(&dyn_arc, &ctx())
        .await
        .expect("both registrations succeed");
    assert_eq!(result.member, RegistrationOutcome::RegisteredNew);
    assert_eq!(result.container, RegistrationOutcome::RegisteredNew);

    // The member handle MUST land first: the container's
    // `allowed_membership_types` references it, so RG's
    // `resolve_ids(allowed_membership_types)` step would fail
    // closed if the order ever inverted. Pin the ordering invariant.
    let order = arc.create_order.lock().expect("lock").clone();
    assert_eq!(
        order,
        vec![
            USER_MEMBERSHIP_TYPE.to_owned(),
            USER_GROUP_TYPE_CODE.to_owned(),
        ],
        "member handle MUST be registered before container"
    );
}

#[tokio::test]
async fn both_types_already_present_returns_already_present_pair() {
    let client = FakeRgClient::defaults()
        .with(
            USER_MEMBERSHIP_TYPE,
            TypeState::already_present(FakeRgClient::equivalent_member_row()),
        )
        .with(
            USER_GROUP_TYPE_CODE,
            TypeState::already_present(FakeRgClient::equivalent_container_row()),
        );
    let result = register_user_group_types(&into_client(client), &ctx())
        .await
        .expect("equivalent traits accepted");
    assert_eq!(result.member, RegistrationOutcome::AlreadyPresent);
    assert_eq!(result.container, RegistrationOutcome::AlreadyPresent);
}

#[tokio::test]
async fn container_loose_extras_accepted() {
    // Looseness contract: AM accepts existing RG schemas that admit
    // EXTRA parents / memberships beyond what AM requires. RG is
    // allowed to seed broader policies AM does not control. Mirrors
    // the previous single-type "extras accepted" test, now exercised
    // against the post-Path-D two-type spec.
    let row = FakeRgClient::loose_container_row(
        &["gts.cf.core.rg.type.v1~extra.type.v1~"],
        &["gts.cf.core.rg.type.v1~extra.member.v1~"],
    );
    let client =
        FakeRgClient::defaults().with(USER_GROUP_TYPE_CODE, TypeState::already_present(row));
    let result = register_user_group_types(&into_client(client), &ctx())
        .await
        .expect("extra parents / members accepted");
    assert_eq!(result.container, RegistrationOutcome::AlreadyPresent);
}

// ---- divergent / missing on the container -------------------------

#[tokio::test]
async fn container_can_be_root_false_diverges() {
    let client = FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::already_present(FakeRgClient::divergent_container_can_be_root_false()),
    );
    match register_user_group_types(&into_client(client), &ctx()).await {
        Err(RegistrationError::DivergentSchema(msg)) => {
            assert!(
                msg.contains("can_be_root"),
                "msg should name the diverging trait: {msg}"
            );
        }
        other => panic!("expected DivergentSchema, got {other:?}"),
    }
}

#[tokio::test]
async fn container_missing_self_parent_diverges() {
    let client = FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::already_present(FakeRgClient::divergent_container_missing_self_parent()),
    );
    match register_user_group_types(&into_client(client), &ctx()).await {
        Err(RegistrationError::DivergentSchema(msg)) => {
            assert!(
                msg.contains("allowed_parent_type"),
                "msg should name the missing parent trait: {msg}"
            );
        }
        other => panic!("expected DivergentSchema, got {other:?}"),
    }
}

#[tokio::test]
async fn container_missing_member_type_diverges() {
    // The post-Path-D check tightens the predicate: a container that
    // is registered without `USER_MEMBERSHIP_TYPE` in its
    // `allowed_membership_types` is **incompatible** because
    // `add_membership(group, USER_MEMBERSHIP_TYPE, user)` would fail
    // closed in production. Pin the divergence.
    let client = FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::already_present(FakeRgClient::divergent_container_missing_member_type()),
    );
    match register_user_group_types(&into_client(client), &ctx()).await {
        Err(RegistrationError::DivergentSchema(msg)) => {
            assert!(
                msg.contains("allowed_membership_type"),
                "msg should name the missing membership trait: {msg}"
            );
        }
        other => panic!("expected DivergentSchema, got {other:?}"),
    }
}

// ---- divergent on the member handle -------------------------------

#[tokio::test]
async fn member_handle_can_be_root_false_diverges_and_container_not_attempted() {
    let divergent_member = ResourceGroupType {
        can_be_root: false,
        ..FakeRgClient::equivalent_member_row()
    };
    let arc: Arc<FakeRgClient> = Arc::new(FakeRgClient::defaults().with(
        USER_MEMBERSHIP_TYPE,
        TypeState::already_present(divergent_member),
    ));
    let dyn_arc: Arc<dyn ResourceGroupClient + Send + Sync> = arc.clone();
    match register_user_group_types(&dyn_arc, &ctx()).await {
        Err(RegistrationError::DivergentSchema(_)) => {}
        other => panic!("expected DivergentSchema on member handle, got {other:?}"),
    }
    // Container's `create_type` MUST NOT run when step-1 fails.
    let order = arc.create_order.lock().expect("lock").clone();
    assert!(
        !order.iter().any(|c| c == USER_GROUP_TYPE_CODE),
        "container create_type MUST NOT be attempted after member-handle divergence; order={order:?}"
    );
}

// ---- race re-read --------------------------------------------------

#[tokio::test]
async fn container_race_with_equivalent_peer_traits_returns_already_present() {
    let client = FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::race_with_reread(FakeRgClient::equivalent_container_row()),
    );
    let result = register_user_group_types(&into_client(client), &ctx())
        .await
        .expect("race-winner with equivalent traits accepted");
    assert_eq!(result.container, RegistrationOutcome::AlreadyPresent);
}

#[tokio::test]
async fn container_race_with_divergent_peer_traits_surfaces_divergent() {
    let client = FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::race_with_reread(FakeRgClient::divergent_container_missing_member_type()),
    );
    match register_user_group_types(&into_client(client), &ctx()).await {
        Err(RegistrationError::DivergentSchema(_)) => {}
        other => panic!("expected DivergentSchema on divergent race-winner, got {other:?}"),
    }
}

// ---- transport errors ---------------------------------------------

#[tokio::test]
async fn member_handle_get_type_transport_error_returns_service_unavailable() {
    let arc: Arc<FakeRgClient> = Arc::new(
        FakeRgClient::defaults().with(USER_MEMBERSHIP_TYPE, TypeState::get_type_unavailable()),
    );
    let dyn_arc: Arc<dyn ResourceGroupClient + Send + Sync> = arc.clone();
    match register_user_group_types(&dyn_arc, &ctx()).await {
        Err(RegistrationError::ServiceUnavailable(_)) => {}
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
    // Container MUST NOT be touched when step-1 transport fails.
    let order = arc.create_order.lock().expect("lock").clone();
    assert!(
        !order.iter().any(|c| c == USER_GROUP_TYPE_CODE),
        "container create_type MUST NOT be attempted after member-handle transport failure; order={order:?}"
    );
}

#[tokio::test]
async fn container_create_type_transport_error_returns_service_unavailable() {
    let client =
        FakeRgClient::defaults().with(USER_GROUP_TYPE_CODE, TypeState::create_type_unavailable());
    match register_user_group_types(&into_client(client), &ctx()).await {
        Err(RegistrationError::ServiceUnavailable(_)) => {}
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

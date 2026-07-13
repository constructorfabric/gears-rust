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
//! * `AlreadyExists` race is re-read per-type via `get_type` and
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
use toolkit_gts::gts_id;

use async_trait::async_trait;
use resource_group_sdk::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup, ResourceGroupClient,
    ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest,
    UpdateTypeRequest,
};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::registration::{RegistrationError, RegistrationOutcome, register_user_group_types};
use super::{USER_GROUP_TYPE_CODE, USER_MEMBERSHIP_TYPE};

// The RG trait boundary is `CanonicalError` (ADR 0005). These helpers
// synthesize the canonical errors the real RG ladder emits, so the
// production code's `.map_err(ResourceGroupError::from)` dispatch
// (NotFound vs. transport failure) is exercised exactly as in prod.
#[resource_error(gts_id!("cf.core.resource_group.group.v1~"))]
struct RgErr;

fn rg_not_found(code: &str) -> CanonicalError {
    RgErr::not_found(format!("'{code}' not found"))
        .with_resource(code)
        .create()
}

fn rg_already_exists(code: &str) -> CanonicalError {
    RgErr::already_exists(format!("'{code}' already exists"))
        .with_resource(code)
        .create()
}

// ---- fake client ---------------------------------------------------

enum GetTypeBehaviour {
    Return(ResourceGroupType),
    NotFound,
    Error(CanonicalError),
    /// Sequence: every call advances; out-of-range calls reuse the
    /// last entry. Used by the `AlreadyExists` race-path tests
    /// where the first `get_type` returns `NotFound` (we believe
    /// the type is absent) and a second call after a peer's race
    /// returns whatever the peer wrote (equivalent or divergent).
    Sequence(Vec<GetTypeBehaviour>, AtomicUsize),
}

#[derive(Clone)]
enum CreateTypeBehaviour {
    Ok,
    AlreadyExists,
    Error(CanonicalError),
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
            get_type: GetTypeBehaviour::Error(
                CanonicalError::internal("connection refused").create(),
            ),
            create_type: CreateTypeBehaviour::Ok,
        }
    }

    fn create_type_unavailable() -> Self {
        Self {
            get_type: GetTypeBehaviour::NotFound,
            create_type: CreateTypeBehaviour::Error(
                CanonicalError::internal("connection refused").create(),
            ),
        }
    }
}

struct FakeRgClient {
    states: HashMap<&'static str, TypeState>,
    /// Order in which `create_type` was observed -- lets tests assert
    /// that the member handle was registered before the container.
    create_order: Mutex<Vec<String>>,
    /// Recorded `update_type` calls (code + final spec).
    /// `register_user_group_types` patches the container's self-parent
    /// rule via `update_type` after the initial create, so tests can
    /// assert the patch ran with the expected payload.
    update_calls: Mutex<Vec<(String, UpdateTypeRequest)>>,
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
            update_calls: Mutex::new(Vec::new()),
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

    /// Container row that lacks the self-parent rule. Represents the
    /// "post-CREATE / pre-patch" shape an init crash would leave
    /// behind, OR a fresh row before the follow-up `update_type` runs.
    /// Two-pass registration self-heals this state by calling
    /// `update_type` to install the self-parent rule.
    fn container_row_without_self_parent() -> ResourceGroupType {
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
    ) -> Result<ResourceGroupType, CanonicalError> {
        fn dispatch(b: &GetTypeBehaviour, code: &str) -> Result<ResourceGroupType, CanonicalError> {
            match b {
                GetTypeBehaviour::Return(t) => Ok(t.clone()),
                GetTypeBehaviour::Error(e) => Err(e.clone()),
                // `NotFound` is the genuine "no row" result; nested
                // `Sequence` is a misuse (caller should flatten) but
                // collapsing to `NotFound` keeps the fake observable
                // rather than panicking inside a test fixture.
                GetTypeBehaviour::NotFound | GetTypeBehaviour::Sequence(_, _) => {
                    Err(rg_not_found(code))
                }
            }
        }
        let state = self.states.get(code).ok_or_else(|| rg_not_found(code))?;
        match &state.get_type {
            GetTypeBehaviour::Sequence(steps, idx) => {
                let i = idx.fetch_add(1, Ordering::SeqCst);
                let step = steps
                    .get(i)
                    .or_else(|| steps.last())
                    .ok_or_else(|| rg_not_found(code))?;
                dispatch(step, code)
            }
            other => dispatch(other, code),
        }
    }

    async fn create_type(
        &self,
        _ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
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
            .ok_or_else(|| rg_not_found(&request.code))?;
        match &state.create_type {
            CreateTypeBehaviour::Ok => Ok(ResourceGroupType {
                code: request.code,
                can_be_root: request.can_be_root,
                allowed_parent_types: request.allowed_parent_types,
                allowed_membership_types: request.allowed_membership_types,
                metadata_schema: request.metadata_schema,
            }),
            CreateTypeBehaviour::AlreadyExists => Err(rg_already_exists(&request.code)),
            CreateTypeBehaviour::Error(e) => Err(e.clone()),
        }
    }

    async fn list_types(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, CanonicalError> {
        unreachable!()
    }
    async fn update_type(
        &self,
        _ctx: &SecurityContext,
        code: &str,
        request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError> {
        self.update_calls
            .lock()
            .expect("update_calls lock not poisoned")
            .push((code.to_owned(), request.clone()));
        Ok(ResourceGroupType {
            code: code.to_owned(),
            can_be_root: request.can_be_root,
            allowed_parent_types: request.allowed_parent_types,
            allowed_membership_types: request.allowed_membership_types,
            metadata_schema: request.metadata_schema,
        })
    }
    async fn delete_type(&self, _ctx: &SecurityContext, _code: &str) -> Result<(), CanonicalError> {
        unreachable!()
    }
    async fn create_group(
        &self,
        _ctx: &SecurityContext,
        _request: CreateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        unreachable!()
    }
    async fn update_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        unreachable!()
    }
    async fn delete_group(&self, _ctx: &SecurityContext, _id: Uuid) -> Result<(), CanonicalError> {
        unreachable!()
    }
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        unreachable!()
    }
    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        unreachable!()
    }
    async fn add_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<ResourceGroupMembership, CanonicalError> {
        unreachable!()
    }
    async fn remove_membership(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _resource_type: &str,
        _resource_id: &str,
    ) -> Result<(), CanonicalError> {
        unreachable!()
    }
    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
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

    // The container's CREATE submits an EMPTY `allowed_parent_types`
    // (RG's `resolve_ids` rejects self-references at create time);
    // the self-parent rule is patched as a follow-up `update_type`
    // call once the row is in RG's `gts_type` table.
    let updates = arc.update_calls.lock().expect("lock").clone();
    assert_eq!(
        updates.len(),
        1,
        "exactly one `update_type` call expected for the container's self-parent patch"
    );
    let (code, req) = &updates[0];
    assert_eq!(code, USER_GROUP_TYPE_CODE);
    assert!(req.can_be_root);
    assert_eq!(
        req.allowed_parent_types,
        vec![USER_GROUP_TYPE_CODE.to_owned()],
        "the patch MUST install the self-parent rule"
    );
    assert_eq!(
        req.allowed_membership_types,
        vec![USER_MEMBERSHIP_TYPE.to_owned()],
        "the patch MUST preserve the member-handle in `allowed_membership_types`"
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
        &[gts_id!("cf.core.rg.type.v1~extra.rg.test.type.v1~")],
        &[gts_id!("cf.core.rg.type.v1~extra.rg.test.member.v1~")],
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
async fn container_missing_self_parent_is_patched_via_update_type() {
    // A pre-existing container row that lacks the self-parent rule
    // (e.g. a prior init that completed the CREATE but crashed before
    // the `update_type` patch) MUST be self-healed on the next init.
    // The two-pass algorithm classifies the row as inclusive-equivalent
    // against the empty-parents spec and then calls `update_type` to
    // install the self-parent rule, leaving the container in its
    // canonical shape regardless of where the prior init stopped.
    let arc: Arc<FakeRgClient> = Arc::new(FakeRgClient::defaults().with(
        USER_GROUP_TYPE_CODE,
        TypeState::already_present(FakeRgClient::container_row_without_self_parent()),
    ));
    let dyn_arc: Arc<dyn ResourceGroupClient + Send + Sync> = arc.clone();
    let result = register_user_group_types(&dyn_arc, &ctx())
        .await
        .expect("missing self-parent must be patched, not surfaced as divergent");
    assert_eq!(result.container, RegistrationOutcome::AlreadyPresent);

    let updates = arc.update_calls.lock().expect("lock").clone();
    assert_eq!(updates.len(), 1, "the patch MUST run exactly once");
    let (code, req) = &updates[0];
    assert_eq!(code, USER_GROUP_TYPE_CODE);
    assert_eq!(
        req.allowed_parent_types,
        vec![USER_GROUP_TYPE_CODE.to_owned()],
        "the patch MUST install the self-parent rule on the partially-registered row"
    );
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

#[tokio::test]
async fn container_update_type_transport_error_returns_service_unavailable() {
    // The follow-up `update_type` that patches the self-parent rule is
    // the second hop in the two-pass registration. If RG is reachable
    // for the CREATE but the immediately-following UPDATE trips a
    // transport failure, init MUST fail closed with `ServiceUnavailable`
    // rather than silently leaving the container without its
    // self-parent rule.
    struct FailingUpdateClient {
        delegate: FakeRgClient,
    }

    #[async_trait]
    impl ResourceGroupClient for FailingUpdateClient {
        async fn get_type(
            &self,
            ctx: &SecurityContext,
            code: &str,
        ) -> Result<ResourceGroupType, CanonicalError> {
            self.delegate.get_type(ctx, code).await
        }
        async fn create_type(
            &self,
            ctx: &SecurityContext,
            request: CreateTypeRequest,
        ) -> Result<ResourceGroupType, CanonicalError> {
            self.delegate.create_type(ctx, request).await
        }
        async fn list_types(
            &self,
            ctx: &SecurityContext,
            query: &ODataQuery,
        ) -> Result<Page<ResourceGroupType>, CanonicalError> {
            self.delegate.list_types(ctx, query).await
        }
        async fn update_type(
            &self,
            _ctx: &SecurityContext,
            _code: &str,
            _request: UpdateTypeRequest,
        ) -> Result<ResourceGroupType, CanonicalError> {
            Err(CanonicalError::internal("connection refused").create())
        }
        async fn delete_type(
            &self,
            ctx: &SecurityContext,
            code: &str,
        ) -> Result<(), CanonicalError> {
            self.delegate.delete_type(ctx, code).await
        }
        async fn create_group(
            &self,
            ctx: &SecurityContext,
            request: CreateGroupRequest,
        ) -> Result<ResourceGroup, CanonicalError> {
            self.delegate.create_group(ctx, request).await
        }
        async fn get_group(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<ResourceGroup, CanonicalError> {
            self.delegate.get_group(ctx, id).await
        }
        async fn list_groups(
            &self,
            ctx: &SecurityContext,
            query: &ODataQuery,
        ) -> Result<Page<ResourceGroup>, CanonicalError> {
            self.delegate.list_groups(ctx, query).await
        }
        async fn update_group(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
            request: UpdateGroupRequest,
        ) -> Result<ResourceGroup, CanonicalError> {
            self.delegate.update_group(ctx, id, request).await
        }
        async fn delete_group(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
        ) -> Result<(), CanonicalError> {
            self.delegate.delete_group(ctx, id).await
        }
        async fn get_group_descendants(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
            query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
            self.delegate.get_group_descendants(ctx, id, query).await
        }
        async fn get_group_ancestors(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
            query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
            self.delegate.get_group_ancestors(ctx, id, query).await
        }
        async fn add_membership(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
            ty: &str,
            rid: &str,
        ) -> Result<ResourceGroupMembership, CanonicalError> {
            self.delegate.add_membership(ctx, id, ty, rid).await
        }
        async fn remove_membership(
            &self,
            ctx: &SecurityContext,
            id: Uuid,
            ty: &str,
            rid: &str,
        ) -> Result<(), CanonicalError> {
            self.delegate.remove_membership(ctx, id, ty, rid).await
        }
        async fn list_memberships(
            &self,
            ctx: &SecurityContext,
            query: &ODataQuery,
        ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
            self.delegate.list_memberships(ctx, query).await
        }
    }

    let client: Arc<dyn ResourceGroupClient + Send + Sync> = Arc::new(FailingUpdateClient {
        delegate: FakeRgClient::defaults(),
    });
    match register_user_group_types(&client, &ctx()).await {
        Err(RegistrationError::ServiceUnavailable(msg)) => {
            assert!(
                msg.contains("self-parent"),
                "msg should name the failing patch step: {msg}"
            );
        }
        other => panic!("expected ServiceUnavailable on update_type failure, got {other:?}"),
    }
}

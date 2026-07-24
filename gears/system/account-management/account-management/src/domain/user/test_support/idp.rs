//! Test stub for the [`IdpPluginClient`] contract.
//!
//! Pairs with the [`FakeUserOutcome`] enum that drives the per-call
//! outcome independently for `provision_user`, `deprovision_user`, and
//! `list_users`. Tests configure the desired outcome via the
//! `set_*_outcome` helpers, then exercise [`crate::domain::user::service::UserService`]
//! against the fake to pin the contract behaviour without touching a
//! real provider.
//!
//! State is stored behind `Arc<Mutex<...>>` so the fake is `Clone +
//! Send + Sync` and can be shared across tasks the way
//! `FakeIdpProvisioner` is.

#![allow(
    dead_code,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::expect_used,
    reason = "test-support fake; canonical mutex-locking pattern with helper getters that not every test exercises today"
)]

use std::sync::Mutex;

use account_management_sdk::{
    IdpDeprovisionUserRequest, IdpListUsersRequest, IdpPluginClient, IdpProvisionUserRequest,
    IdpUpdateUserRequest, IdpUser, IdpUserFilterField, IdpUserOperationFailure,
};
use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_odata::{
    ODataOrderBy, Page, PageInfo,
    filter::{FilterNode, FilterOp, ODataValue},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Four-outcome stub for an `IdP` user-operations call.
///
/// Each method (`provision_user`, `deprovision_user`, `list_users`) carries
/// its own configurable outcome stored on [`FakeIdpUserProvisioner`];
/// tests that need different verdicts per method set them
/// independently. `RejectPayload` exists so the
/// `IdpUserOperationFailure::Rejected` -> `Validation` mapping branch is
/// exercised; `Unavailable` and `Unsupported` cover the other two
/// SDK failure variants.
#[domain_model]
#[derive(Clone)]
pub enum FakeUserOutcome {
    /// `deprovision_user` returns `Ok(())`; `provision_user` returns
    /// the configured projection; `list_users` returns the configured
    /// page. Per the collapsed `IdpPluginClient::deprovision_user`
    /// contract a successful call is the same whether the plugin
    /// actually removed the user or saw it already absent — both
    /// surface as `Ok(())` to AM.
    Ok,
    /// Returns `Err(IdpUserOperationFailure::Unavailable)`.
    Unavailable,
    /// Returns `Err(IdpUserOperationFailure::UnsupportedOperation)`.
    Unsupported,
    /// Returns `Err(IdpUserOperationFailure::Rejected)`.
    RejectPayload,
    /// Returns `Err(IdpUserOperationFailure::NotFound)`. Only
    /// meaningful for `update_user` (deprovision folds absent into
    /// `Ok(())`); exposed on the shared enum so update tests can pin
    /// the `NotFound` -> `UserNotFound` (404) mapping.
    NotFound,
}

/// Capture of a `list_users` invocation sufficient for AM service-layer
/// test assertions: tenant scope + the typed `OData` filter and order
/// that were forwarded to the plugin layer. Replaces the older
/// `(tenant_id, Option<Uuid>)` tuple shape now that
/// [`IdpListUsersRequest`] carries `Option<FilterNode<IdpUserFilterField>>`
/// + `Option<ODataOrderBy>` rather than a single-purpose `user_id_filter`.
#[derive(Clone, Debug)]
pub struct RecordedListCall {
    pub tenant_id: Uuid,
    pub filter: Option<FilterNode<IdpUserFilterField>>,
    pub order: Option<ODataOrderBy>,
}

/// In-memory `FakeIdpUserProvisioner` implementing
/// [`IdpPluginClient`]. Per-method outcomes default to
/// [`FakeUserOutcome::Ok`]; tests override them via the
/// `set_*_outcome` helpers below.
///
/// `record_calls` is enabled by default so tests can assert "no `IdP`
/// call issued" cases. Each method append-records a per-call entry
/// (`tenant_id` + the per-method scoped value) to a dedicated `Vec`.
#[domain_model]
pub struct FakeIdpUserProvisioner {
    create_outcome: Mutex<FakeUserOutcome>,
    delete_outcome: Mutex<FakeUserOutcome>,
    list_outcome: Mutex<FakeUserOutcome>,
    update_outcome: Mutex<FakeUserOutcome>,
    create_calls: Mutex<Vec<(Uuid, String)>>,
    delete_calls: Mutex<Vec<(Uuid, Uuid)>>,
    list_calls: Mutex<Vec<RecordedListCall>>,
    update_calls: Mutex<Vec<(Uuid, Uuid)>>,
    /// Per-call snapshot of `req.tenant_context.metadata` recorded
    /// from every `IdP` method (provision / deprovision / list). Lets
    /// service-level tests pin that the AM-loaded
    /// `tenant_idp_metadata` blob is forwarded verbatim on each
    /// call (regression guard against the metadata-load step being
    /// silently dropped).
    create_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    delete_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    list_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    update_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    /// Optional projection returned on the `provision_user` happy path.
    /// Defaults to a synthesized projection with `id = Uuid::new_v4()`.
    create_projection: Mutex<Option<IdpUser>>,
    /// Optional projection returned on the `update_user` happy path.
    /// Defaults to a projection with `id = req.user_id` reflecting the
    /// applied patch fields.
    update_projection: Mutex<Option<IdpUser>>,
    /// Optional page returned on the `list_users` happy path.
    /// Defaults to an empty page with the request's `top` / `skip`.
    list_page_items: Mutex<Vec<IdpUser>>,
}

impl FakeIdpUserProvisioner {
    pub fn new() -> Self {
        Self {
            create_outcome: Mutex::new(FakeUserOutcome::Ok),
            delete_outcome: Mutex::new(FakeUserOutcome::Ok),
            list_outcome: Mutex::new(FakeUserOutcome::Ok),
            update_outcome: Mutex::new(FakeUserOutcome::Ok),
            create_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            list_calls: Mutex::new(Vec::new()),
            update_calls: Mutex::new(Vec::new()),
            create_metadata_snapshots: Mutex::new(Vec::new()),
            delete_metadata_snapshots: Mutex::new(Vec::new()),
            list_metadata_snapshots: Mutex::new(Vec::new()),
            update_metadata_snapshots: Mutex::new(Vec::new()),
            create_projection: Mutex::new(None),
            update_projection: Mutex::new(None),
            list_page_items: Mutex::new(Vec::new()),
        }
    }

    pub fn set_create_outcome(&self, oc: FakeUserOutcome) {
        *self.create_outcome.lock().expect("lock") = oc;
    }

    pub fn set_delete_outcome(&self, oc: FakeUserOutcome) {
        *self.delete_outcome.lock().expect("lock") = oc;
    }

    pub fn set_list_outcome(&self, oc: FakeUserOutcome) {
        *self.list_outcome.lock().expect("lock") = oc;
    }

    pub fn set_update_outcome(&self, oc: FakeUserOutcome) {
        *self.update_outcome.lock().expect("lock") = oc;
    }

    /// Override the projection returned on the `update_user` happy
    /// path. Without this override the fake synthesizes a projection
    /// with `id = req.user_id` reflecting the applied patch fields.
    pub fn set_update_projection(&self, projection: IdpUser) {
        *self.update_projection.lock().expect("lock") = Some(projection);
    }

    /// Override the projection returned on the `provision_user` happy
    /// path. Without this override the fake returns a
    /// `IdpUser` whose `id` is freshly minted on every call.
    pub fn set_create_projection(&self, projection: IdpUser) {
        *self.create_projection.lock().expect("lock") = Some(projection);
    }

    /// Replace the items returned by the `list_users` happy path. The
    /// fake echoes the request's `top` / `skip` on every page; this
    /// helper only governs the `items` vector.
    pub fn set_list_items(&self, items: Vec<IdpUser>) {
        *self.list_page_items.lock().expect("lock") = items;
    }

    pub fn create_call_count(&self) -> usize {
        self.create_calls.lock().expect("lock").len()
    }

    pub fn delete_call_count(&self) -> usize {
        self.delete_calls.lock().expect("lock").len()
    }

    pub fn list_call_count(&self) -> usize {
        self.list_calls.lock().expect("lock").len()
    }

    pub fn update_call_count(&self) -> usize {
        self.update_calls.lock().expect("lock").len()
    }

    pub fn update_calls_snapshot(&self) -> Vec<(Uuid, Uuid)> {
        self.update_calls.lock().expect("lock").clone()
    }

    /// Snapshot of `tenant_context.metadata` recorded on every
    /// `update_user` call, in call order.
    pub fn update_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.update_metadata_snapshots.lock().expect("lock").clone()
    }

    pub fn create_calls_snapshot(&self) -> Vec<(Uuid, String)> {
        self.create_calls.lock().expect("lock").clone()
    }

    pub fn delete_calls_snapshot(&self) -> Vec<(Uuid, Uuid)> {
        self.delete_calls.lock().expect("lock").clone()
    }

    pub fn list_calls_snapshot(&self) -> Vec<RecordedListCall> {
        self.list_calls.lock().expect("lock").clone()
    }

    /// Snapshot of `tenant_context.metadata` recorded on every
    /// `provision_user` call, in call order. See the field doc on
    /// [`FakeIdpUserProvisioner::create_metadata_snapshots`].
    pub fn create_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.create_metadata_snapshots.lock().expect("lock").clone()
    }

    /// Snapshot of `tenant_context.metadata` recorded on every
    /// `deprovision_user` call, in call order.
    pub fn delete_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.delete_metadata_snapshots.lock().expect("lock").clone()
    }

    /// Snapshot of `tenant_context.metadata` recorded on every
    /// `list_users` call, in call order.
    pub fn list_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.list_metadata_snapshots.lock().expect("lock").clone()
    }
}

impl Default for FakeIdpUserProvisioner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IdpPluginClient for FakeIdpUserProvisioner {
    async fn provision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        self.create_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.payload.username.clone()));
        self.create_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.create_outcome.lock().expect("lock").clone();
        match oc {
            FakeUserOutcome::Ok => {
                let projection = self.create_projection.lock().expect("lock").clone();
                Ok(projection.unwrap_or_else(|| {
                    let mut p = IdpUser::new(Uuid::new_v4(), req.payload.username.clone());
                    if let Some(email) = req.payload.email.clone() {
                        p = p.with_email(email);
                    }
                    if let Some(display_name) = req.payload.display_name.clone() {
                        p = p.with_display_name(display_name);
                    }
                    p
                }))
            }
            FakeUserOutcome::Unavailable => Err(IdpUserOperationFailure::Unavailable {
                detail: "fake unavailable".into(),
            }),
            FakeUserOutcome::Unsupported => Err(IdpUserOperationFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            }),
            FakeUserOutcome::RejectPayload => Err(IdpUserOperationFailure::Rejected {
                detail: "fake rejected".into(),
            }),
            FakeUserOutcome::NotFound => Err(IdpUserOperationFailure::NotFound {
                detail: "fake not found".into(),
            }),
        }
    }

    async fn deprovision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        self.delete_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.user_id));
        self.delete_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.delete_outcome.lock().expect("lock").clone();
        match oc {
            FakeUserOutcome::Ok => Ok(()),
            FakeUserOutcome::Unavailable => Err(IdpUserOperationFailure::Unavailable {
                detail: "fake unavailable".into(),
            }),
            FakeUserOutcome::Unsupported => Err(IdpUserOperationFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            }),
            FakeUserOutcome::RejectPayload => Err(IdpUserOperationFailure::Rejected {
                detail: "fake rejected".into(),
            }),
            FakeUserOutcome::NotFound => Err(IdpUserOperationFailure::NotFound {
                detail: "fake not found".into(),
            }),
        }
    }

    async fn update_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpUpdateUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        self.update_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.user_id));
        self.update_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.update_outcome.lock().expect("lock").clone();
        match oc {
            FakeUserOutcome::Ok => {
                let projection = self.update_projection.lock().expect("lock").clone();
                Ok(projection.unwrap_or_else(|| {
                    // Synthesize a projection reflecting the applied
                    // patch so service tests can assert the mutated
                    // fields. Username defaults to a stable placeholder
                    // when the patch does not rename (the fake holds no
                    // pre-update state).
                    let username = req
                        .patch
                        .username
                        .clone()
                        .unwrap_or_else(|| "user".to_owned());
                    let mut p = IdpUser::new(req.user_id, username);
                    if let Some(Some(email)) = req.patch.email.clone() {
                        p = p.with_email(email);
                    }
                    if let Some(Some(display_name)) = req.patch.display_name.clone() {
                        p = p.with_display_name(display_name);
                    }
                    if let Some(Some(first_name)) = req.patch.first_name.clone() {
                        p = p.with_first_name(first_name);
                    }
                    if let Some(Some(last_name)) = req.patch.last_name.clone() {
                        p = p.with_last_name(last_name);
                    }
                    p
                }))
            }
            FakeUserOutcome::Unavailable => Err(IdpUserOperationFailure::Unavailable {
                detail: "fake unavailable".into(),
            }),
            FakeUserOutcome::Unsupported => Err(IdpUserOperationFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            }),
            FakeUserOutcome::RejectPayload => Err(IdpUserOperationFailure::Rejected {
                detail: "fake rejected".into(),
            }),
            FakeUserOutcome::NotFound => Err(IdpUserOperationFailure::NotFound {
                detail: "fake not found".into(),
            }),
        }
    }

    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        self.list_calls
            .lock()
            .expect("lock")
            .push(RecordedListCall {
                tenant_id: req.tenant_context.tenant_id,
                filter: req.filter.clone(),
                order: req.order.clone(),
            });
        self.list_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.list_outcome.lock().expect("lock").clone();
        match oc {
            FakeUserOutcome::Ok => {
                let items = self.list_page_items.lock().expect("lock").clone();
                let filtered: Vec<IdpUser> = match req.filter.as_ref() {
                    Some(f) => items.into_iter().filter(|u| matches_filter(u, f)).collect(),
                    None => items,
                };
                // Emulate a paginating IdP backed by a stable Vec
                // ordering. The opaque cursor is just the decimal
                // offset into the Vec — sufficient to exercise
                // continuation semantics in unit tests without
                // pulling in `toolkit_odata::pagination` (which is the
                // shape real plugins use). Production plugins SHOULD
                // embed a filter hash + sort key per the SDK doc;
                // here the test contract is single-process and
                // single-threaded so the simpler offset cursor
                // suffices.
                let start: usize = req
                    .pagination
                    .cursor()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                let top = usize::try_from(req.pagination.top()).unwrap_or(usize::MAX);
                let end = start.saturating_add(top).min(filtered.len());
                let page_items = filtered[start.min(filtered.len())..end].to_vec();
                let next_cursor = (end < filtered.len()).then(|| end.to_string());
                Ok(Page::new(
                    page_items,
                    PageInfo {
                        next_cursor,
                        prev_cursor: None,
                        limit: u64::from(req.pagination.top()),
                    },
                ))
            }
            FakeUserOutcome::Unavailable => Err(IdpUserOperationFailure::Unavailable {
                detail: "fake unavailable".into(),
            }),
            FakeUserOutcome::Unsupported => Err(IdpUserOperationFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            }),
            FakeUserOutcome::RejectPayload => Err(IdpUserOperationFailure::Rejected {
                detail: "fake rejected".into(),
            }),
            FakeUserOutcome::NotFound => Err(IdpUserOperationFailure::NotFound {
                detail: "fake not found".into(),
            }),
        }
    }
}

/// In-memory evaluator for the typed `OData` filter forwarded on
/// [`IdpListUsersRequest::filter`]. Mirrors the contract documented in
/// the spec §4.4: case-insensitive for `Contains`/`StartsWith`/
/// `EndsWith`, case-sensitive for `Eq`/`Ne`; composite `And`/`Or`/`Not`
/// are honoured; unsupported composite ops short-circuit as match-all
/// (they never reach the fake — the parser rejects them upstream).
/// Order-by is recorded but not applied here; static-idp tests pin the
/// real order semantics for the production path.
fn matches_filter(user: &IdpUser, filter: &FilterNode<IdpUserFilterField>) -> bool {
    match filter {
        FilterNode::Binary { field, op, value } => eval_binary(user, *field, *op, value),
        FilterNode::Composite {
            op: FilterOp::And,
            children,
        } => children.iter().all(|c| matches_filter(user, c)),
        FilterNode::Composite {
            op: FilterOp::Or,
            children,
        } => children.iter().any(|c| matches_filter(user, c)),
        FilterNode::Composite { .. } => unreachable!(
            "the OData parser only emits And/Or as composite ops; everything else \
             surfaces as Binary/InList/Not - reaching this arm signals a bug \
             upstream of the plugin SPI"
        ),
        FilterNode::Not(inner) => !matches_filter(user, inner),
        FilterNode::InList { field, values } => values
            .iter()
            .any(|v| eval_binary(user, *field, FilterOp::Eq, v)),
    }
}

fn eval_binary(
    user: &IdpUser,
    field: IdpUserFilterField,
    op: FilterOp,
    value: &ODataValue,
) -> bool {
    // Project the comparable string from the user row. `None` on an
    // optional field surfaces here as `None` — `Eq` then never matches;
    // `Ne` always matches (an absent value is, by definition, "not
    // equal" to any concrete probe).
    let lhs: Option<String> = match field {
        IdpUserFilterField::Id => Some(user.id.to_string()),
        IdpUserFilterField::Username => Some(user.username.clone()),
        IdpUserFilterField::Email => user.email.clone(),
        IdpUserFilterField::DisplayName => user.display_name.clone(),
        IdpUserFilterField::FirstName => user.first_name.clone(),
        IdpUserFilterField::LastName => user.last_name.clone(),
    };
    let rhs: String = match value {
        ODataValue::String(s) => s.clone(),
        ODataValue::Uuid(u) => u.to_string(),
        other => unreachable!(
            "IdpUserFilterField declares only String and Uuid kinds — the REST parser \
             rejects every other ODataValue at the boundary; got {other:?}"
        ),
    };
    let Some(lhs) = lhs else {
        return matches!(op, FilterOp::Ne);
    };
    let lo = |s: &str| s.to_lowercase();
    match op {
        FilterOp::Eq => lhs == rhs,
        FilterOp::Ne => lhs != rhs,
        FilterOp::Contains => lo(&lhs).contains(&lo(&rhs)),
        FilterOp::StartsWith => lo(&lhs).starts_with(&lo(&rhs)),
        FilterOp::EndsWith => lo(&lhs).ends_with(&lo(&rhs)),
        other => unreachable!(
            "Gt/Ge/Lt/Le/In/And/Or are not legal on the String/Uuid IdpUserFilterField \
             surface — REST parser rejects upstream; got {other:?}"
        ),
    }
}

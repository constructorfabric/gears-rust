//! Test hooks over real persistence with shared port forwarding.
//!

use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};
use types_registry::domain::admission::fingerprint::ScopeHash;
use types_registry::domain::enums::{DependencyKind, EntityKind, OwnershipScope};
use types_registry::domain::family::FamilyKey;
use types_registry::domain::ports::{
    CurrentDocument, CurrentInstanceRow, CurrentInstanceValue, CurrentReadRow, CurrentSchemaCas,
    CurrentSchemaProjection, CurrentTypeSchemaRow, DependencyClosure, DependencyEdgeRow,
    DependencyStore, EdgeSide, EntityEdge, EntityPage, EntityRow, EntityStore,
    EntityWriteOrderStore, InstanceStore, ItemSuccess, ListFilter, NewCurrentInstance,
    NewCurrentTypeSchema, NewEntity, NewInstanceRevision, NewOperation, NewOperationItem,
    NewRevision, OperationItemRow, OperationRow, OperationStore, PageRequest, ReverseImpact,
    Stores, TypeSchemaStore, VersionFamilyRow, VersionFamilyStore,
};
use types_registry::domain::selection::FieldSelection;
use uuid::Uuid;

use super::stores;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PausePoint {
    OperationRead,
    /// Before the commit's first statement claims `entity_write_order`.
    BeforeEntityWriteOrderClaim,
    /// After the claim succeeds.
    AfterEntityWriteOrderClaim,
    /// After the entity/content reads, before the unchanged re-read or CAS.
    CurrentDocuments,
    /// After creation takes the family row but before checking its rules.
    CreateOrGet,
    RevisionEntityRead,
}

#[derive(Default)]
pub struct Hooks {
    pause: Option<Arc<Pause>>,
    claim: Option<ClaimSignals>,
    entity_writes: Option<EntityWrites>,
    refuse_schema_cas_for: Option<i64>,
    refuse_deletion_for: Option<i64>,
    stale_find_items: parking_lot::Mutex<Option<Vec<OperationItemRow>>>,
    fail: std::collections::BTreeSet<FailingCall>,
    transient_mark_running: Option<TransientFailures>,
    pub stall_find_by_id: Option<std::time::Duration>,
    pub stall_mark_system_failed: Option<std::time::Duration>,
    pub stall_mark_running: Option<std::time::Duration>,
    injected_cause: Option<&'static str>,
}

struct Pause {
    at: PausePoint,
    /// Matching call to pause, starting at 1.
    nth: usize,
    seen: std::sync::atomic::AtomicUsize,
    target: Option<parking_lot::Mutex<GateTarget>>,
    reached: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resume: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateTarget {
    Disarmed,
    Armed,
    Holding(Uuid),
}

impl Pause {
    fn attributes(&self, operation_id: Option<Uuid>) -> bool {
        let Some(target) = &self.target else {
            return true;
        };
        let Some(operation_id) = operation_id else {
            return false;
        };
        let mut target = target.lock();
        match *target {
            GateTarget::Disarmed => false,
            GateTarget::Armed => {
                *target = GateTarget::Holding(operation_id);
                true
            }
            GateTarget::Holding(held) => held == operation_id,
        }
    }
}

struct ClaimSignals {
    entered: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    returned: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailingCall {
    MarkCompleted,
    MarkItemSucceeded,
    MarkRunning,
    FindById,
    MarkSystemFailed,
}

#[derive(Default)]
struct TransientFailures {
    remaining: std::sync::atomic::AtomicUsize,
    issued: std::sync::atomic::AtomicUsize,
}

///
/// Refusing rather than only recording is deliberate: a pass that writes and
/// rolls back leaves the same tables behind as one that never wrote, so an
#[derive(Default)]
struct EntityWrites {
    attempts: parking_lot::Mutex<Vec<&'static str>>,
    forbid: bool,
}

impl Hooks {
    async fn at(&self, point: PausePoint) {
        self.hold(point, None).await;
    }

    async fn at_operation(&self, point: PausePoint, operation_id: Uuid) {
        self.hold(point, Some(operation_id)).await;
    }

    async fn hold(&self, point: PausePoint, operation_id: Option<Uuid>) {
        if let Some(claim) = &self.claim {
            let slot = match point {
                PausePoint::BeforeEntityWriteOrderClaim => Some(&claim.entered),
                PausePoint::AfterEntityWriteOrderClaim => Some(&claim.returned),
                _ => None,
            };
            if let Some(slot) = slot
                && let Some(signal) = slot.lock().await.take()
            {
                signal.send(()).ok();
            }
        }

        if let Some(pause) = &self.pause
            && pause.at == point
            && pause.attributes(operation_id)
            && pause.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == pause.nth
        {
            let reached = pause.reached.lock().await.take();
            let resume = pause.resume.lock().await.take();
            if let Some(reached) = reached {
                reached.send(()).ok();
            }
            if let Some(resume) = resume {
                resume.await.expect("the test must always resume the pass");
            }
        }
    }

    fn entity_write(&self, call: &'static str) -> Result<(), ScopeError> {
        let Some(writes) = &self.entity_writes else {
            return Ok(());
        };
        writes.attempts.lock().push(call);
        if writes.forbid {
            return Err(ScopeError::Invalid(
                "this pass must issue no entity-state write",
            ));
        }
        Ok(())
    }

    fn refuses_schema_cas(&self, entity_id: i64) -> bool {
        self.refuse_schema_cas_for == Some(entity_id)
    }

    fn refuses_deletion(&self, entity_id: i64) -> bool {
        self.refuse_deletion_for == Some(entity_id)
    }

    fn fails(&self, call: FailingCall) -> bool {
        self.fail.contains(&call)
    }

    fn cause(&self, own: &'static str) -> &'static str {
        self.injected_cause.unwrap_or(own)
    }

    fn takes_transient_mark_running_failure(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        let Some(failures) = &self.transient_mark_running else {
            return false;
        };
        let taken = failures
            .remaining
            .fetch_update(SeqCst, SeqCst, |left| left.checked_sub(1))
            .is_ok();
        if taken {
            failures.issued.fetch_add(1, SeqCst);
        }
        taken
    }

    fn take_stale_find_items_snapshot(&self) -> Option<Vec<OperationItemRow>> {
        self.stale_find_items.lock().take()
    }
}

#[derive(Clone)]
pub struct SharedPause(Arc<Pause>);

impl SharedPause {
    #[must_use]
    pub fn reached(&self) -> usize {
        self.0.seen.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn arm(&self) {
        let target = self
            .0
            .target
            .as_ref()
            .expect("only a filtered gate attributes arrivals to one operation");
        let mut target = target.lock();
        assert_eq!(
            *target,
            GateTarget::Disarmed,
            "a gate attributes one operation, and this one already has its own",
        );
        *target = GateTarget::Armed;
    }

    #[must_use]
    pub fn held_operation(&self) -> Option<Uuid> {
        match *self.0.target.as_ref()?.lock() {
            GateTarget::Holding(operation_id) => Some(operation_id),
            GateTarget::Armed | GateTarget::Disarmed => None,
        }
    }
}

pub struct TestStores {
    inner: Arc<dyn Stores>,
    hooks: Hooks,
}

#[derive(Default)]
#[must_use]
pub struct TestStoresBuilder {
    hooks: Hooks,
}

impl TestStores {
    pub fn builder() -> TestStoresBuilder {
        TestStoresBuilder::default()
    }

    #[must_use]
    pub fn transient_failures_issued(&self) -> usize {
        self.hooks
            .transient_mark_running
            .as_ref()
            .map_or(0, |failures| {
                failures.issued.load(std::sync::atomic::Ordering::SeqCst)
            })
    }

    #[must_use]
    pub fn failing_running_transiently(times: usize) -> Arc<Self> {
        Self::builder()
            .failing_mark_running_transiently(times)
            .build()
    }

    #[must_use]
    pub fn entity_write_attempts(&self) -> Vec<&'static str> {
        self.hooks
            .entity_writes
            .as_ref()
            .map(|writes| writes.attempts.lock().clone())
            .unwrap_or_default()
    }
}

impl TestStoresBuilder {
    pub fn pausing_at(
        self,
        at: PausePoint,
        nth: usize,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) -> Self {
        self.pausing_with(&SharedPause(Arc::new(Pause {
            at,
            nth,
            seen: std::sync::atomic::AtomicUsize::new(0),
            target: None,
            reached: tokio::sync::Mutex::new(Some(reached)),
            resume: tokio::sync::Mutex::new(Some(resume)),
        })))
    }

    pub fn pausing_with(mut self, gate: &SharedPause) -> Self {
        self.hooks.pause = Some(Arc::clone(&gate.0));
        self
    }

    pub fn signalling_claim(
        mut self,
        entered: tokio::sync::oneshot::Sender<()>,
        returned: tokio::sync::oneshot::Sender<()>,
    ) -> Self {
        self.hooks.claim = Some(ClaimSignals {
            entered: tokio::sync::Mutex::new(Some(entered)),
            returned: tokio::sync::Mutex::new(Some(returned)),
        });
        self
    }

    pub fn recording_entity_writes(mut self, forbid: bool) -> Self {
        self.hooks.entity_writes = Some(EntityWrites {
            attempts: parking_lot::Mutex::new(Vec::new()),
            forbid,
        });
        self
    }

    pub fn refusing_schema_cas(mut self, entity_id: i64) -> Self {
        self.hooks.refuse_schema_cas_for = Some(entity_id);
        self
    }

    pub fn refusing_deletion(mut self, entity_id: i64) -> Self {
        self.hooks.refuse_deletion_for = Some(entity_id);
        self
    }

    pub fn stale_find_items(mut self, snapshot: Vec<OperationItemRow>) -> Self {
        self.hooks.stale_find_items = parking_lot::Mutex::new(Some(snapshot));
        self
    }

    pub fn failing(mut self, call: FailingCall) -> Self {
        self.hooks.fail.insert(call);
        self
    }

    pub fn failing_mark_running_transiently(mut self, times: usize) -> Self {
        self.hooks.transient_mark_running = Some(TransientFailures {
            remaining: std::sync::atomic::AtomicUsize::new(times),
            issued: std::sync::atomic::AtomicUsize::new(0),
        });
        self
    }

    pub fn stalling_find_by_id(mut self, delay: std::time::Duration) -> Self {
        self.hooks.stall_find_by_id = Some(delay);
        self
    }

    pub fn stalling_mark_system_failed(mut self, delay: std::time::Duration) -> Self {
        self.hooks.stall_mark_system_failed = Some(delay);
        self
    }

    pub fn stalling_mark_running(mut self, delay: std::time::Duration) -> Self {
        self.hooks.stall_mark_running = Some(delay);
        self
    }

    pub fn with_injected_cause(mut self, text: &'static str) -> Self {
        self.hooks.injected_cause = Some(text);
        self
    }

    pub fn build(self) -> Arc<TestStores> {
        Arc::new(TestStores {
            inner: stores(),
            hooks: self.hooks,
        })
    }
}

impl TestStores {
    /// Returns decorated ports, a pause notification, and a resume sender.
    #[must_use]
    pub fn pausing(
        at: PausePoint,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        Self::pausing_at_occurrence(at, 1)
    }

    /// Like [`Self::pausing`], but hold the `nth` matching call.
    #[must_use]
    pub fn pausing_at_occurrence(
        at: PausePoint,
        nth: usize,
    ) -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let decorated = Self::builder()
            .pausing_at(at, nth, reached_tx, resume_rx)
            .build();
        (decorated, reached_rx, resume_tx)
    }

    #[must_use]
    pub fn pausing_shared_for_one_operation(
        at: PausePoint,
    ) -> (
        Arc<Self>,
        SharedPause,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let gate = SharedPause(Arc::new(Pause {
            at,
            nth: 1,
            seen: std::sync::atomic::AtomicUsize::new(0),
            target: Some(parking_lot::Mutex::new(GateTarget::Disarmed)),
            reached: tokio::sync::Mutex::new(Some(reached_tx)),
            resume: tokio::sync::Mutex::new(Some(resume_rx)),
        }));
        let decorated = Self::builder().pausing_with(&gate).build();
        (decorated, gate, reached_rx, resume_tx)
    }

    #[must_use]
    pub fn sharing_pause(gate: &SharedPause) -> Arc<Self> {
        Self::builder().pausing_with(gate).build()
    }

    /// Returns decorated ports and notifications for claim entry and success.
    #[must_use]
    pub fn claim_signalling() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
        let decorated = Self::builder()
            .signalling_claim(entered_tx, returned_tx)
            .build();
        (decorated, entered_rx, returned_rx)
    }

    /// Refuse `refuse_for_entity_id`'s current-schema compare-and-swap.
    #[must_use]
    pub fn cas_miss(refuse_for_entity_id: i64) -> Arc<Self> {
        Self::builder()
            .refusing_schema_cas(refuse_for_entity_id)
            .build()
    }

    /// Refuse `refuse_for_entity_id`'s lifecycle transition to `DELETED`.
    #[must_use]
    pub fn deletion_miss(refuse_for_entity_id: i64) -> Arc<Self> {
        Self::builder()
            .refusing_deletion(refuse_for_entity_id)
            .build()
    }

    #[must_use]
    pub fn forbidding_entity_writes() -> Arc<Self> {
        Self::builder().recording_entity_writes(true).build()
    }

    #[must_use]
    pub fn failing_completion() -> Arc<Self> {
        Self::builder().failing(FailingCall::MarkCompleted).build()
    }

    #[must_use]
    pub fn failing_running() -> Arc<Self> {
        Self::builder().failing(FailingCall::MarkRunning).build()
    }

    #[must_use]
    pub fn failing_item_success() -> Arc<Self> {
        Self::builder()
            .failing(FailingCall::MarkItemSucceeded)
            .build()
    }

    #[must_use]
    pub fn failing_operation_read() -> Arc<Self> {
        Self::builder().failing(FailingCall::FindById).build()
    }

    #[must_use]
    pub fn stalling_status_path(delay: std::time::Duration) -> Arc<Self> {
        Self::builder()
            .stalling_find_by_id(delay)
            .stalling_mark_system_failed(delay)
            .build()
    }

    #[must_use]
    pub fn slow_admission_then_stalled_failure_write(
        admit: std::time::Duration,
        write: std::time::Duration,
    ) -> Arc<Self> {
        Self::builder()
            .stalling_mark_running(admit)
            .failing(FailingCall::MarkRunning)
            .stalling_mark_system_failed(write)
            .build()
    }

    #[must_use]
    pub fn failing_running_and_failure_write() -> Arc<Self> {
        Self::builder()
            .failing(FailingCall::MarkRunning)
            .failing(FailingCall::MarkSystemFailed)
            .build()
    }

    #[must_use]
    pub fn failing_item_success_saying(text: &'static str) -> Arc<Self> {
        Self::builder()
            .failing(FailingCall::MarkItemSucceeded)
            .with_injected_cause(text)
            .build()
    }

    #[must_use]
    pub fn with_stale_snapshot(snapshot: Vec<OperationItemRow>) -> Arc<Self> {
        Self::builder().stale_find_items(snapshot).build()
    }
}

// Port implementations.

#[async_trait]
impl EntityWriteOrderStore for TestStores {
    async fn claim_entity_write_order(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        now: OffsetDateTime,
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("claim_entity_write_order")?;
        self.hooks.at(PausePoint::BeforeEntityWriteOrderClaim).await;
        self.inner.claim_entity_write_order(tx, scope, now).await?;
        self.hooks.at(PausePoint::AfterEntityWriteOrderClaim).await;
        Ok(())
    }
}

#[async_trait]
impl VersionFamilyStore for TestStores {
    async fn find_family_by_key(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        family_key: &FamilyKey,
    ) -> Result<Option<VersionFamilyRow>, ScopeError> {
        self.inner.find_family_by_key(tx, scope, family_key).await
    }

    async fn create_or_get(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        family_key: &FamilyKey,
        ownership_scope: OwnershipScope,
        owner_tenant_id: Option<Uuid>,
        now: OffsetDateTime,
    ) -> Result<(VersionFamilyRow, bool), ScopeError> {
        self.hooks.entity_write("create_or_get")?;
        let out = self
            .inner
            .create_or_get(tx, scope, family_key, ownership_scope, owner_tenant_id, now)
            .await?;
        self.hooks.at(PausePoint::CreateOrGet).await;
        Ok(out)
    }
}

#[async_trait]
impl EntityStore for TestStores {
    async fn find_by_gts_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        gts_id: &str,
    ) -> Result<Option<EntityRow>, ScopeError> {
        self.hooks.at(PausePoint::RevisionEntityRead).await;
        self.inner.find_by_gts_id(tx, scope, gts_id).await
    }

    async fn find_by_gts_ids(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        gts_ids: &[String],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        self.inner.find_by_gts_ids(tx, scope, gts_ids).await
    }

    async fn find_by_ids(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        self.inner.find_by_ids(tx, scope, entity_ids).await
    }

    async fn find_by_gts_uuid(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        gts_uuid: Uuid,
    ) -> Result<Option<EntityRow>, ScopeError> {
        self.inner.find_by_gts_uuid(tx, scope, gts_uuid).await
    }

    async fn find_by_gts_uuids(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        gts_uuids: &[Uuid],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        self.inner.find_by_gts_uuids(tx, scope, gts_uuids).await
    }

    async fn list_page(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        filter: &ListFilter,
        request: PageRequest,
    ) -> Result<EntityPage, ScopeError> {
        self.inner.list_page(tx, scope, filter, request).await
    }

    async fn kind_in_family(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        family_id: i64,
    ) -> Result<Option<EntityKind>, ScopeError> {
        self.inner.kind_in_family(tx, scope, family_id).await
    }

    async fn insert_entity(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewEntity,
    ) -> Result<Option<EntityRow>, ScopeError> {
        self.hooks.entity_write("insert_entity")?;
        self.inner.insert_entity(tx, scope, new).await
    }

    async fn compare_and_swap_version(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        expected_resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<Option<i64>, ScopeError> {
        self.hooks.entity_write("compare_and_swap_version")?;
        self.inner
            .compare_and_swap_version(tx, scope, entity_id, expected_resource_version, now)
            .await
    }

    async fn mark_deleted(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        expected_resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<Option<i64>, ScopeError> {
        self.hooks.entity_write("mark_deleted")?;
        if self.hooks.refuses_deletion(entity_id) {
            // Simulate the row moving after the commit read it.
            return Ok(None);
        }
        self.inner
            .mark_deleted(tx, scope, entity_id, expected_resource_version, now)
            .await
    }
}

#[async_trait]
impl TypeSchemaStore for TestStores {
    async fn current_documents(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentDocument>, ScopeError> {
        let out = self.inner.current_documents(tx, scope, entity_ids).await?;
        self.hooks.at(PausePoint::CurrentDocuments).await;
        Ok(out)
    }

    async fn find_current_schema(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentTypeSchemaRow>, ScopeError> {
        self.inner.find_current_schema(tx, scope, entity_id).await
    }

    async fn read_current_schemas(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
        selection: FieldSelection,
    ) -> Result<Vec<CurrentReadRow>, ScopeError> {
        self.inner
            .read_current_schemas(tx, scope, entity_ids, selection)
            .await
    }

    async fn current_schema_projections(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentSchemaProjection>, ScopeError> {
        self.inner
            .current_schema_projections(tx, scope, entity_ids)
            .await
    }

    async fn insert_schema_revision(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewRevision,
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("insert_schema_revision")?;
        self.inner.insert_schema_revision(tx, scope, new).await
    }

    async fn insert_current_schema(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentTypeSchema,
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("insert_current_schema")?;
        self.inner.insert_current_schema(tx, scope, new).await
    }

    async fn update_current_schema(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentTypeSchema,
        expected: CurrentSchemaCas,
    ) -> Result<bool, ScopeError> {
        self.hooks.entity_write("update_current_schema")?;
        if self.hooks.refuses_schema_cas(new.entity_id) {
            // Simulate the projection moving after its token was captured.
            return Ok(false);
        }
        self.inner
            .update_current_schema(tx, scope, new, expected)
            .await
    }
}

#[async_trait]
impl InstanceStore for TestStores {
    async fn current_values(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentInstanceValue>, ScopeError> {
        self.inner.current_values(tx, scope, entity_ids).await
    }

    async fn find_current_instance(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentInstanceRow>, ScopeError> {
        self.inner.find_current_instance(tx, scope, entity_id).await
    }

    async fn read_current_values(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
        selection: FieldSelection,
    ) -> Result<Vec<CurrentReadRow>, ScopeError> {
        self.inner
            .read_current_values(tx, scope, entity_ids, selection)
            .await
    }

    async fn insert_instance_revision(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewInstanceRevision,
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("insert_instance_revision")?;
        self.inner.insert_instance_revision(tx, scope, new).await
    }

    async fn insert_current_instance(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("insert_current_instance")?;
        self.inner.insert_current_instance(tx, scope, new).await
    }

    async fn update_current_instance(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<bool, ScopeError> {
        self.hooks.entity_write("update_current_instance")?;
        self.inner.update_current_instance(tx, scope, new).await
    }
}

#[async_trait]
impl OperationStore for TestStores {
    async fn find_by_idempotency(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        idempotency_scope_hash: &ScopeHash,
        idempotency_key: &str,
    ) -> Result<Option<OperationRow>, ScopeError> {
        self.inner
            .find_by_idempotency(tx, scope, idempotency_scope_hash, idempotency_key)
            .await
    }

    async fn find_by_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<OperationRow>, ScopeError> {
        if self.hooks.fails(FailingCall::FindById) {
            return Err(ScopeError::Invalid(
                "this operation's status read is under failure injection",
            ));
        }
        self.hooks.at_operation(PausePoint::OperationRead, id).await;
        if let Some(delay) = self.hooks.stall_find_by_id {
            tokio::time::sleep(delay).await;
        }
        self.inner.find_by_id(tx, scope, id).await
    }

    async fn insert_operation(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        new: NewOperation,
    ) -> Result<OperationRow, ScopeError> {
        self.inner.insert_operation(tx, scope, new).await
    }

    async fn insert_items(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        parent: &OperationRow,
        items: &[NewOperationItem],
    ) -> Result<(), ScopeError> {
        self.inner.insert_items(tx, scope, parent, items).await
    }

    async fn find_items(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        operation_id: Uuid,
    ) -> Result<Vec<OperationItemRow>, ScopeError> {
        if let Some(snapshot) = self.hooks.take_stale_find_items_snapshot() {
            return Ok(snapshot);
        }
        self.inner.find_items(tx, scope, operation_id).await
    }

    async fn mark_running(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        if let Some(delay) = self.hooks.stall_mark_running {
            tokio::time::sleep(delay).await;
        }
        if self.hooks.fails(FailingCall::MarkRunning) {
            return Err(ScopeError::Invalid(
                "this operation's running move is under failure injection",
            ));
        }
        if self.hooks.takes_transient_mark_running_failure() {
            return Err(ScopeError::Db(sea_orm::DbErr::Custom(
                "this operation's running move is under temporary failure injection".to_owned(),
            )));
        }
        self.inner.mark_running(tx, scope, id, now).await
    }

    async fn mark_completed(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        if self.hooks.fails(FailingCall::MarkCompleted) {
            return Err(ScopeError::Db(sea_orm::DbErr::Query(
                sea_orm::RuntimeErr::Internal(
                    "(code: 5) database is locked: operation completion failure injection"
                        .to_owned(),
                ),
            )));
        }
        self.inner.mark_completed(tx, scope, id, now).await
    }

    async fn mark_system_failed(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        if let Some(delay) = self.hooks.stall_mark_system_failed {
            tokio::time::sleep(delay).await;
        }
        if self.hooks.fails(FailingCall::MarkSystemFailed) {
            return Err(ScopeError::Db(sea_orm::DbErr::Query(
                sea_orm::RuntimeErr::Internal(
                    self.hooks
                        .cause("(code: 5) database is locked: system-failure write injection")
                        .to_owned(),
                ),
            )));
        }
        self.inner.mark_system_failed(tx, scope, id, now).await
    }

    async fn mark_item_succeeded(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        item_id: i64,
        outcome: ItemSuccess,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        if self.hooks.fails(FailingCall::MarkItemSucceeded) {
            return Err(ScopeError::Db(sea_orm::DbErr::Query(
                sea_orm::RuntimeErr::Internal(
                    self.hooks
                        .cause("(code: 5) database is locked: item success failure injection")
                        .to_owned(),
                ),
            )));
        }
        self.inner
            .mark_item_succeeded(tx, scope, item_id, outcome, now)
            .await
    }

    async fn mark_item_unchanged(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        item_id: i64,
        resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        self.inner
            .mark_item_unchanged(tx, scope, item_id, resource_version, now)
            .await
    }

    async fn mark_item_failed(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        item_id: i64,
        error_payload: String,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        self.inner
            .mark_item_failed(tx, scope, item_id, error_payload, now)
            .await
    }

    async fn fail_nonterminal_items(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        operation_id: Uuid,
        error_payload: String,
        now: OffsetDateTime,
    ) -> Result<u64, ScopeError> {
        self.inner
            .fail_nonterminal_items(tx, scope, operation_id, error_payload, now)
            .await
    }
}

#[async_trait]
impl DependencyStore for TestStores {
    async fn has_live_direct_instances(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        type_schema_entity_id: i64,
    ) -> Result<bool, ScopeError> {
        self.inner
            .has_live_direct_instances(tx, scope, type_schema_entity_id)
            .await
    }

    async fn live_direct_dependents(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        bound: usize,
    ) -> Result<usize, ScopeError> {
        self.inner
            .live_direct_dependents(tx, scope, entity_id, bound)
            .await
    }

    async fn edge_page(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
        side: EdgeSide,
        after: Option<&DependencyEdgeRow>,
        limit: usize,
    ) -> Result<Vec<DependencyEdgeRow>, ScopeError> {
        self.inner
            .edge_page(tx, scope, entity_ids, side, after, limit)
            .await
    }

    async fn live_direct_dependent_ids(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        kind: Option<DependencyKind>,
        limit: usize,
    ) -> Result<Vec<i64>, ScopeError> {
        self.inner
            .live_direct_dependent_ids(tx, scope, entity_id, kind, limit)
            .await
    }

    async fn edges_within(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<EntityEdge>, ScopeError> {
        self.inner.edges_within(tx, scope, entity_ids).await
    }

    async fn closure(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        roots: &[String],
    ) -> Result<DependencyClosure, ScopeError> {
        self.inner.closure(tx, scope, roots).await
    }

    async fn reverse_impact(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        roots: &[i64],
        write_set_bound: usize,
    ) -> Result<ReverseImpact, ScopeError> {
        self.inner
            .reverse_impact(tx, scope, roots, write_set_bound)
            .await
    }

    async fn replace_outgoing(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        from_entity_id: i64,
        edges: &[(DependencyKind, i64)],
    ) -> Result<(), ScopeError> {
        self.hooks.entity_write("replace_outgoing")?;
        self.inner
            .replace_outgoing(tx, scope, from_entity_id, edges)
            .await
    }
}

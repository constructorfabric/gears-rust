//! Test hooks over real persistence with shared port forwarding.
//! [`PauseHooks`] pauses calls, [`ClaimHooks`] signals claim entry/return, and
//! [`CasMissHooks`] refuses a CAS. Extend [`PausePoint`] for timing or
//! [`StoreHooks`] for inspection and overrides.
//!
//! `async_trait` may allocate for no-op hooks, though they do not yield.

use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};
use types_registry::domain::admission::fingerprint::ScopeHash;
use types_registry::domain::enums::{DependencyKind, EntityKind, OwnershipScope};
use types_registry::domain::family::FamilyKey;
use types_registry::domain::ports::{
    CurrentDocument, CurrentInstanceRow, CurrentInstanceValue, CurrentSchemaCas,
    CurrentSchemaProjection, CurrentTypeSchemaRow, DependencyClosure, DependencyEdgeRow,
    DependencyStore, EdgeSide, EntityEdge, EntityRow, EntityStore, EntityWriteOrderStore,
    InstanceStore, ItemSuccess, NewCurrentInstance, NewCurrentTypeSchema, NewEntity,
    NewInstanceRevision, NewOperation, NewOperationItem, NewRevision, OperationItemRow,
    OperationRow, OperationStore, ReverseImpact, Stores, TypeSchemaStore, VersionFamilyRow,
    VersionFamilyStore,
};
use uuid::Uuid;

use super::stores;

/// Hook locations in the commit transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PausePoint {
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

/// Port-call hooks with no-op defaults.
#[async_trait]
pub trait StoreHooks: Send + Sync {
    /// Runs at each reached [`PausePoint`]; may hold the transaction open.
    async fn at(&self, _point: PausePoint) {}

    /// Return `true` to simulate a schema CAS miss without a database write.
    fn refuse_schema_cas(&self, _entity_id: i64) -> bool {
        false
    }

    /// Called before each entity-state write or write-order claim; allows by default.
    /// No-write tests reject attempts, since final-state equality also permits rollback.
    fn entity_write(&self, _call: &'static str) -> Result<(), ScopeError> {
        Ok(())
    }

    /// Return `true` to fail the operation's completion write, which is how the
    /// atomicity of publication is put under test.
    fn fail_mark_completed(&self) -> bool {
        false
    }

    /// Return `true` to simulate a deletion losing its race: the entity read
    /// inside the commit saw `ACTIVE` at the expected version, and the write
    /// then matched nothing. Both preconditions live in the statement's
    /// `WHERE`, so this is the only way to reach that arm without a second
    /// writer that ignores the `entity_write_order` claim — and there is none.
    fn refuse_deletion(&self, _entity_id: i64) -> bool {
        false
    }
}

/// Pauses one matching call until the test resumes it.
pub struct PauseHooks {
    at: PausePoint,
    /// Matching call to pause, starting at 1.
    nth: usize,
    seen: std::sync::atomic::AtomicUsize,
    reached: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resume: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[async_trait]
impl StoreHooks for PauseHooks {
    /// Signal and pause only the selected occurrence.
    async fn at(&self, point: PausePoint) {
        if point != self.at {
            return;
        }
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 != self.nth {
            return;
        }
        let reached = self.reached.lock().await.take();
        let resume = self.resume.lock().await.take();
        if let Some(reached) = reached {
            // Ignore a receiver dropped by an aborted test.
            reached.send(()).ok();
        }
        if let Some(resume) = resume {
            resume.await.expect("the test must always resume the pass");
        }
    }
}

/// Signals claim entry and successful return. A lock wait must be verified
/// separately; see `assert_backend_reports_a_blocked_claim` in the backend tests.
pub struct ClaimHooks {
    entered: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    returned: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait]
impl StoreHooks for ClaimHooks {
    async fn at(&self, point: PausePoint) {
        let slot = match point {
            PausePoint::BeforeEntityWriteOrderClaim => &self.entered,
            PausePoint::AfterEntityWriteOrderClaim => &self.returned,
            _ => return,
        };
        if let Some(signal) = slot.lock().await.take() {
            // Ignore a receiver dropped by an aborted test.
            signal.send(()).ok();
        }
    }
}

/// Refuses every schema CAS for one entity, including retries, to test rollback.
pub struct CasMissHooks {
    refuse_for_entity_id: i64,
}

impl StoreHooks for CasMissHooks {
    fn refuse_schema_cas(&self, entity_id: i64) -> bool {
        entity_id == self.refuse_for_entity_id
    }
}

/// Refuses every `mark_deleted` for one entity, so the deletion commit sees the
/// row move between its read and its write.
pub struct DeletionMissHooks {
    refuse_for_entity_id: i64,
}

impl StoreHooks for DeletionMissHooks {
    fn refuse_deletion(&self, entity_id: i64) -> bool {
        entity_id == self.refuse_for_entity_id
    }
}

/// Records every entity-state write attempt, and optionally refuses it.
///
/// Refusing rather than only recording is deliberate: a pass that writes and
/// rolls back leaves the same tables behind as one that never wrote, so an
/// assertion on the tables cannot tell them apart. This one fails the attempt.
#[derive(Default)]
pub struct EntityWriteSpy {
    attempts: std::sync::Mutex<Vec<&'static str>>,
    forbid: bool,
}

impl StoreHooks for EntityWriteSpy {
    fn entity_write(&self, call: &'static str) -> Result<(), ScopeError> {
        self.attempts
            .lock()
            .expect("the spy's record is never poisoned")
            .push(call);
        if self.forbid {
            return Err(ScopeError::Invalid(
                "this pass must issue no entity-state write",
            ));
        }
        Ok(())
    }
}

impl TestStores<EntityWriteSpy> {
    /// Ports that refuse — and record — every entity-state write and the
    /// write-order claim, while serving every read from real storage.
    #[must_use]
    pub fn forbidding_entity_writes() -> Arc<Self> {
        Arc::new(Self {
            inner: stores(),
            hooks: EntityWriteSpy {
                attempts: std::sync::Mutex::new(Vec::new()),
                forbid: true,
            },
        })
    }

    /// Every entity-state write attempted so far, in call order.
    #[must_use]
    pub fn entity_write_attempts(&self) -> Vec<&'static str> {
        self.hooks
            .attempts
            .lock()
            .expect("the spy's record is never poisoned")
            .clone()
    }
}

/// Fails the operation's completion write and nothing else.
pub struct CompletionFailureHooks;

impl StoreHooks for CompletionFailureHooks {
    fn fail_mark_completed(&self) -> bool {
        true
    }
}

impl TestStores<CompletionFailureHooks> {
    /// Ports whose `mark_completed` always fails, so a publication that includes
    /// it must leave every item write behind with it.
    #[must_use]
    pub fn failing_completion() -> Arc<Self> {
        Arc::new(Self {
            inner: stores(),
            hooks: CompletionFailureHooks,
        })
    }
}

/// Real persistence adapter decorated with `H`'s hooks.
pub struct TestStores<H> {
    inner: Arc<dyn Stores>,
    hooks: H,
}

impl TestStores<PauseHooks> {
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
        let decorated = Arc::new(Self {
            inner: stores(),
            hooks: PauseHooks {
                at,
                nth,
                seen: std::sync::atomic::AtomicUsize::new(0),
                reached: tokio::sync::Mutex::new(Some(reached_tx)),
                resume: tokio::sync::Mutex::new(Some(resume_rx)),
            },
        });
        (decorated, reached_rx, resume_tx)
    }
}

impl TestStores<ClaimHooks> {
    /// Returns decorated ports and notifications for claim entry and success.
    #[must_use]
    pub fn claim_signalling() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                inner: stores(),
                hooks: ClaimHooks {
                    entered: tokio::sync::Mutex::new(Some(entered_tx)),
                    returned: tokio::sync::Mutex::new(Some(returned_tx)),
                },
            }),
            entered_rx,
            returned_rx,
        )
    }
}

impl TestStores<CasMissHooks> {
    /// Refuse `refuse_for_entity_id`'s current-schema compare-and-swap.
    #[must_use]
    pub fn cas_miss(refuse_for_entity_id: i64) -> Arc<Self> {
        Arc::new(Self {
            inner: stores(),
            hooks: CasMissHooks {
                refuse_for_entity_id,
            },
        })
    }
}

impl TestStores<DeletionMissHooks> {
    /// Refuse `refuse_for_entity_id`'s lifecycle transition to `DELETED`.
    #[must_use]
    pub fn deletion_miss(refuse_for_entity_id: i64) -> Arc<Self> {
        Arc::new(Self {
            inner: stores(),
            hooks: DeletionMissHooks {
                refuse_for_entity_id,
            },
        })
    }
}

// Port implementations.

#[async_trait]
impl<H: StoreHooks> EntityWriteOrderStore for TestStores<H> {
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
impl<H: StoreHooks> VersionFamilyStore for TestStores<H> {
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
impl<H: StoreHooks> EntityStore for TestStores<H> {
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
        if self.hooks.refuse_deletion(entity_id) {
            // Simulate the row moving after the commit read it.
            return Ok(None);
        }
        self.inner
            .mark_deleted(tx, scope, entity_id, expected_resource_version, now)
            .await
    }
}

#[async_trait]
impl<H: StoreHooks> TypeSchemaStore for TestStores<H> {
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
        if self.hooks.refuse_schema_cas(new.entity_id) {
            // Simulate the projection moving after its token was captured.
            return Ok(false);
        }
        self.inner
            .update_current_schema(tx, scope, new, expected)
            .await
    }
}

#[async_trait]
impl<H: StoreHooks> InstanceStore for TestStores<H> {
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
impl<H: StoreHooks> OperationStore for TestStores<H> {
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
        self.inner.find_items(tx, scope, operation_id).await
    }

    async fn mark_running(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        self.inner.mark_running(tx, scope, id, now).await
    }

    async fn mark_completed(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        if self.hooks.fail_mark_completed() {
            return Err(ScopeError::Invalid(
                "this pass's operation completion is under failure injection",
            ));
        }
        self.inner.mark_completed(tx, scope, id, now).await
    }

    async fn mark_item_succeeded(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        item_id: i64,
        outcome: ItemSuccess,
        now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
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
}

#[async_trait]
impl<H: StoreHooks> DependencyStore for TestStores<H> {
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

//! Dry-run persistence ports over one read snapshot and an [`overlay::Overlay`].
//!
//! Reuse admission checks unchanged. Reads merge lazy snapshot reads with virtual
//! writes; no entity-state statement is issued and the registry is never loaded
//! whole. Bounded graph walks account for replaced edges ([`walk`]).
//!
//! Unsupported ports, including operation mutation and stored-edge paging, return
//! [`ScopeError::Invalid`]. Implementations are grouped by port in [`entities`],
//! [`documents`], [`operations`] and [`dependencies`].

mod dependencies;
mod documents;
mod entities;
mod operations;
mod overlay;
mod walk;

use std::sync::Arc;

use time::OffsetDateTime;
use tokio::sync::{Mutex, MutexGuard};
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};

use crate::domain::enums::LifecycleStatus;
use crate::domain::ports::{EntityRow, Stores};

pub use overlay::ItemOutcomeWrite;
use overlay::{GraphView, Overlay};

/// One dry-run pass's view of the registry: the snapshot underneath, plus what
/// the pass has decided so far.
pub struct AdmissionView {
    base: Arc<dyn Stores>,
    state: Mutex<Overlay>,
}

/// A point the pass can return to — the overlay as it stood before one candidate.
///
/// Opaque on purpose: a caller may keep it or restore it, and may not read or
/// edit what is inside. That is the tentative layer, expressed as the only two
/// operations it has. Restoring it is atomic, because it replaces the overlay
/// whole rather than replaying an undo list that could stop halfway.
#[derive(Debug)]
pub struct CandidateLayer(Overlay);

/// `dyn Stores` is not `Debug`, and what a reader of a dump wants from this type
/// is the overlay anyway.
impl std::fmt::Debug for AdmissionView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionView")
            .field("overlay", &self.state)
            .finish_non_exhaustive()
    }
}

impl AdmissionView {
    #[must_use]
    pub fn new(base: Arc<dyn Stores>) -> Self {
        Self {
            base,
            state: Mutex::new(Overlay::default()),
        }
    }

    /// The lock is `tokio`'s rather than `std`'s because merging a read means
    /// consulting the overlay and then awaiting a base read; a guard that may
    /// not cross an await would have to be dropped and retaken around every one
    /// of them.
    async fn overlay(&self) -> MutexGuard<'_, Overlay> {
        self.state.lock().await
    }

    /// Checkpoint the overlay before a candidate. Its checks see its own writes,
    /// including dependent refreshes. Shared immutable rows make checkpointing
    /// copy pointers rather than documents.
    pub async fn begin_candidate(&self) -> CandidateLayer {
        CandidateLayer(self.overlay().await.clone())
    }

    /// Discard a candidate's tentative layer: the refusal's counterpart to the
    /// commit transaction's rollback. Writes made after `layer` was opened —
    /// including a dependent refresh that ran before the refusal — leave nothing
    /// behind.
    pub async fn discard_candidate(&self, layer: CandidateLayer) {
        *self.overlay().await = layer.0;
    }

    /// Keep a candidate's tentative layer. A no-op by construction: the writes
    /// are already in the overlay, and success is what makes them stay. It exists
    /// so a caller states which of the two happened.
    #[expect(
        clippy::unused_self,
        reason = "the receiver is the point: keeping a layer is a no-op on the view, and taking `&self` is what makes the call read as the counterpart of `discard_candidate`"
    )]
    pub fn keep_candidate(&self, layer: CandidateLayer) {
        drop(layer);
    }

    /// The terminal item write one commit path made, if it reached one.
    pub async fn item_write(&self, item_id: i64) -> Option<ItemOutcomeWrite> {
        self.overlay().await.item(item_id)
    }

    /// How many commit paths claimed the write order. Never issued; see
    /// [`entities`].
    pub async fn write_order_claims(&self) -> usize {
        self.overlay().await.claims()
    }

    /// The overlay's graph opinion, copied without its documents.
    async fn graph(&self) -> GraphView {
        self.overlay().await.graph()
    }

    /// One entity as this pass sees it: the overlay's row if it has one, and the
    /// stored row otherwise.
    async fn merged_entity(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        gts_id: &str,
    ) -> Result<Option<EntityRow>, ScopeError> {
        if let Some(row) = self.overlay().await.entity_by_gts_id(gts_id) {
            return Ok(Some(row.as_ref().clone()));
        }
        self.base.find_by_gts_id(tx, scope, gts_id).await
    }

    async fn merged_entity_by_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<EntityRow>, ScopeError> {
        if let Some(row) = self.overlay().await.entity_by_id(entity_id) {
            return Ok(Some(row.as_ref().clone()));
        }
        if entity_id < 0 {
            return Ok(None);
        }
        Ok(self
            .base
            .find_by_ids(tx, scope, &[entity_id])
            .await?
            .into_iter()
            .next())
    }

    /// The next `resource_version`, or the reason the move does not apply.
    ///
    /// Both preconditions are the ones in the stored statement's `WHERE`: the
    /// entity is active and still at `expected`. `None` is the same answer the
    /// database gives, and for the same two reasons.
    async fn advance_version(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        entity_id: i64,
        expected_resource_version: i64,
        now: OffsetDateTime,
        tombstone: bool,
    ) -> Result<Option<i64>, ScopeError> {
        let Some(mut row) = self.merged_entity_by_id(tx, scope, entity_id).await? else {
            return Ok(None);
        };
        if row.lifecycle_status != LifecycleStatus::Active
            || row.resource_version != expected_resource_version
        {
            return Ok(None);
        }
        let Some(next) = expected_resource_version.checked_add(1) else {
            return Err(unsupported("resource_version cannot advance past i64::MAX"));
        };
        row.resource_version = next;
        row.updated_at = now;
        if tombstone {
            row.lifecycle_status = LifecycleStatus::Deleted;
            row.deleted_at = Some(now);
        }
        self.overlay().await.put_entity(row);
        Ok(Some(next))
    }

    /// The ids whose current state the overlay has **not** replaced, with
    /// virtual ids dropped: exactly what is still worth asking the database.
    async fn stored_only(&self, entity_ids: &[i64], kind: CurrentKind) -> Vec<i64> {
        let overlay = self.overlay().await;
        entity_ids
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .filter(|id| match kind {
                CurrentKind::Schema => overlay.schema(*id).is_none(),
                CurrentKind::Instance => overlay.instance(*id).is_none(),
            })
            .collect()
    }
}

/// Which current-state map a read is merging against.
#[derive(Clone, Copy)]
enum CurrentKind {
    Schema,
    Instance,
}

/// The refusal every port with no virtual meaning returns.
const fn unsupported(what: &'static str) -> ScopeError {
    ScopeError::Invalid(what)
}

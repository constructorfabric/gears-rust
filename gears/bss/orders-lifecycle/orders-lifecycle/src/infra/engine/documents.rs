//! What a slice may contribute inside the engine's transaction, and nothing more.
//!
//! [`ChildDocuments`] is the only handle a slice receives on the locked order. It exposes the
//! child-document writers bound to that order (lines, totals, acceptance, reflections, grants,
//! administrative rows, draft working content) and never the aggregate row, its version chain,
//! audit, registry or outbox: those are the engine's alone (DESIGN §4.1 "No component other
//! than the engine MAY write the aggregate, version, line, resolved-total, transition-audit or
//! idempotency tables"; the engine calls these writers on the slice's behalf, at step 19).
use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::ScopeError;

use crate::infra::events::{EventEnqueueError, EventKind, OrderSummary, TxEvents};
use crate::infra::storage::entity;
use crate::infra::storage::repo::LockedOrder;

/// Child-document writers of one locked order, valid only inside its transition.
pub struct ChildDocuments<'l, 'a> {
    locked: &'l LockedOrder<'a, DbTx<'a>>,
    transition_time: OffsetDateTime,
    audit_id: uuid::Uuid,
    execution_id: uuid::Uuid,
}
impl<'l, 'a> ChildDocuments<'l, 'a> {
    pub(super) fn new(
        locked: &'l LockedOrder<'a, DbTx<'a>>,
        transition_time: OffsetDateTime,
        audit_id: uuid::Uuid,
        execution_id: uuid::Uuid,
    ) -> Self {
        Self {
            locked,
            transition_time,
            audit_id,
            execution_id,
        }
    }
    /// The committed audit entry this transition appends at step 22 (a D-198 grant links it).
    #[must_use]
    pub fn audit_id(&self) -> uuid::Uuid {
        self.audit_id
    }
    /// The registry execution that owns this attempt (a D-198 grant records it).
    #[must_use]
    pub fn execution_id(&self) -> uuid::Uuid {
        self.execution_id
    }
    /// The aggregate's current version and dispatch generation as locked, before this
    /// transition's aggregate write (the engine writes the planned post-state afterwards).
    #[must_use]
    pub fn locked_version_and_generation(&self) -> (i32, i64) {
        let row = self.locked.row();
        (row.current_version, row.fulfillment_control_generation)
    }
    /// The engine's single transition timestamp `t`.
    #[must_use]
    pub fn transition_time(&self) -> OffsetDateTime {
        self.transition_time
    }
    /// The locked order's identity.
    #[must_use]
    pub fn order_id(&self) -> uuid::Uuid {
        self.locked.row().order_id
    }

    /// # Errors
    /// Foreign parent, reused identity, scope or store failure.
    pub async fn add_draft_line(
        &self,
        row: entity::draft_content::Model,
    ) -> Result<entity::draft_content::Model, ScopeError> {
        self.locked.add_draft_line(row, self.transition_time).await
    }
    /// # Errors
    /// Stale expected row, foreign parent, scope or store failure.
    pub async fn replace_draft_content(
        &self,
        expected: &entity::draft_content::Model,
        proposed: entity::draft_content::Model,
    ) -> Result<(), ScopeError> {
        self.locked.replace_draft_content(expected, proposed).await
    }
    /// Remove a line from the draft working set (row 2). The reserved line identity and
    /// immutable history stay; `false` means the line was not a member.
    ///
    /// # Errors
    /// Scope or store failure.
    pub async fn remove_draft_line(&self, line_id: uuid::Uuid) -> Result<bool, ScopeError> {
        self.locked.remove_draft_line(line_id).await
    }
    /// The order's administrative row as read under this transition's aggregate lock (every
    /// administrative writer takes that lock), so a last-write-wins replacement is based on the
    /// locked value, never on a preparation snapshot (04 §3.6, §4.6).
    ///
    /// # Errors
    /// Scope or store failure.
    pub async fn order_admin(&self) -> Result<Option<entity::order_admin::Model>, ScopeError> {
        super::locked_order_admin(self.locked).await
    }
    /// A line's administrative row as read under this transition's aggregate lock.
    ///
    /// # Errors
    /// Scope or store failure.
    pub async fn order_line_admin(
        &self,
        line_id: uuid::Uuid,
    ) -> Result<Option<entity::order_line_admin::Model>, ScopeError> {
        Ok(super::locked_line_admin(self.locked)
            .await?
            .into_iter()
            .find(|row| row.line_id == line_id))
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_order_line(
        &self,
        row: entity::order_line::Model,
    ) -> Result<entity::order_line::Model, ScopeError> {
        self.locked.insert_order_line(row).await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_resolved_total(
        &self,
        row: entity::resolved_total::Model,
    ) -> Result<entity::resolved_total::Model, ScopeError> {
        self.locked.insert_resolved_total(row).await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_order_admin(
        &self,
        row: entity::order_admin::Model,
    ) -> Result<entity::order_admin::Model, ScopeError> {
        self.locked.insert_order_admin(row).await
    }
    /// # Errors
    /// Stale expected row, foreign parent, scope or store failure.
    pub async fn replace_order_admin(
        &self,
        expected: &entity::order_admin::Model,
        proposed: entity::order_admin::Model,
    ) -> Result<(), ScopeError> {
        self.locked.replace_order_admin(expected, proposed).await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_order_line_admin(
        &self,
        row: entity::order_line_admin::Model,
    ) -> Result<entity::order_line_admin::Model, ScopeError> {
        self.locked.insert_order_line_admin(row).await
    }
    /// # Errors
    /// Stale expected row, foreign parent, scope or store failure.
    pub async fn replace_order_line_admin(
        &self,
        expected: &entity::order_line_admin::Model,
        proposed: entity::order_line_admin::Model,
    ) -> Result<(), ScopeError> {
        self.locked
            .replace_order_line_admin(expected, proposed)
            .await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_acceptance(
        &self,
        row: entity::acceptance::Model,
    ) -> Result<entity::acceptance::Model, ScopeError> {
        self.locked.insert_acceptance(row).await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_approval_reflection(
        &self,
        row: entity::approval_reflection::Model,
    ) -> Result<entity::approval_reflection::Model, ScopeError> {
        self.locked.insert_approval_reflection(row).await
    }
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_line_fulfillment(
        &self,
        row: entity::line_fulfillment::Model,
    ) -> Result<entity::line_fulfillment::Model, ScopeError> {
        self.locked.insert_line_fulfillment(row).await
    }
    /// Per-line fulfillment progress (S5 receiver reflections and completion mapping).
    ///
    /// # Errors
    /// Stale expected row, foreign parent, scope or store failure.
    pub async fn replace_line_fulfillment(
        &self,
        expected: &entity::line_fulfillment::Model,
        proposed: entity::line_fulfillment::Model,
    ) -> Result<(), ScopeError> {
        self.locked
            .replace_line_fulfillment(expected, proposed)
            .await
    }
    /// D-198 dispatch grant. The engine admits it only on a transition whose planned effects
    /// increment the dispatch generation (row 12; post-spawn resume).
    ///
    /// # Errors
    /// Foreign parent, scope or store failure.
    pub async fn insert_fulfillment_grant(
        &self,
        row: entity::fulfillment_grant::Model,
    ) -> Result<entity::fulfillment_grant::Model, ScopeError> {
        self.locked.insert_fulfillment_grant(row).await
    }
}

/// A slice's child-document contribution (step 19), applied by the engine after the version
/// append and before the aggregate write and audit.
#[async_trait]
pub trait DocumentWriter: Send + Sync {
    /// # Errors
    /// Any store failure; the engine aborts the whole attempt.
    async fn write(&self, documents: &ChildDocuments<'_, '_>) -> Result<(), ScopeError>;
}

/// The row's one typed event (step 24), built from the engine's committed post-state summary.
///
/// A detail receives only a sealed [`EventEnqueue`] handle: it supplies the payload, and the
/// engine supplies the locked order, the transaction's event sink and the post-state summary.
/// It never receives the aggregate, so an event detail cannot write it (DESIGN §4.1).
#[async_trait]
pub trait EventSpec: Send + Sync {
    /// The catalog event this detail belongs to; must equal the row's declared event.
    fn kind(&self) -> bss_orders_lifecycle_sdk::catalog::EventKind;
    /// # Errors
    /// Contract, size, serialization or producer failure; the engine aborts.
    async fn enqueue(&self, enqueue: EventEnqueue<'_, '_>) -> Result<(), EventEnqueueError>;
}

/// The single-use step-24 enqueue: exactly one payload through this transaction's sink, bound to
/// the locked order and the engine-built summary. Its fields are private to the engine.
pub struct EventEnqueue<'l, 'a> {
    events: &'l TxEvents,
    locked: &'l LockedOrder<'a, DbTx<'a>>,
    summary: OrderSummary,
}
impl<'l, 'a> EventEnqueue<'l, 'a> {
    pub(super) fn new(
        events: &'l TxEvents,
        locked: &'l LockedOrder<'a, DbTx<'a>>,
        summary: OrderSummary,
    ) -> Self {
        Self {
            events,
            locked,
            summary,
        }
    }
    /// Enqueue the row's payload; consumes the handle.
    ///
    /// # Errors
    /// Contract, size, serialization or producer failure; the engine aborts.
    pub async fn enqueue<K: EventKind>(self, detail: K) -> Result<(), EventEnqueueError> {
        self.events.enqueue(self.locked, self.summary, detail).await
    }
}

/// Any concrete event payload as an [`EventSpec`].
pub struct EventDetail<K>(pub K);
#[async_trait]
impl<K: EventKind> EventSpec for EventDetail<K> {
    fn kind(&self) -> bss_orders_lifecycle_sdk::catalog::EventKind {
        serde_json::from_value(serde_json::Value::String(K::NAME.to_owned()))
            .unwrap_or_else(|_| unreachable!("every Orders event NAME is a catalog event"))
    }
    async fn enqueue(&self, enqueue: EventEnqueue<'_, '_>) -> Result<(), EventEnqueueError> {
        enqueue.enqueue(self.0.clone()).await
    }
}

//! Scoped storage primitives for the engine, never independently callable business operations.
//!
//! Parent reads apply every PDP predicate to the current aggregate. Child writes require a
//! transaction-bound locked parent; narrowing to its ID does not invent authorization. Full
//! Models, rather than partial `ActiveModels`, make insert validation total. Private repositories
//! consume independently supplied restricted service scopes and never return business authority.
use super::entity::order;
// The discovery statements reach the storage modules through `super`, as the D-184 fixtures
// mount them beside `discovery.rs`.
use super::{entity, sql, status};
use sea_orm::{ActiveModelTrait, Iterable, ModelTrait};
use sea_orm::{EntityTrait, IntoActiveModel, QuerySelect};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

pub mod audit;
pub mod children;
pub mod discovery;
pub mod idempotency;
pub mod private;
pub mod privilege;

pub async fn find_order(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Option<order::Model>, ScopeError> {
    order::Entity::find_by_id(id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Validate all three axes and the standard ID in the complete proposed arrangement.
pub fn validate_order(row: &order::Model, scope: &AccessScope) -> Result<(), ScopeError> {
    let model = row.clone().into_active_model();
    order::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)?;
    Ok(())
}

pub async fn insert_order(
    tx: &impl DBRunner,
    scope: &AccessScope,
    row: order::Model,
) -> Result<order::Model, ScopeError> {
    let model = row.into_active_model();
    order::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)?
        .exec_with_returning(tx)
        .await
}

/// Cannot be constructed from IDs or reused with a different transaction runner.
pub struct LockedOrder<'a, T: TransactionRunner> {
    tx: &'a T,
    row: order::Model,
    scope: AccessScope,
}
impl<'a, T: TransactionRunner> LockedOrder<'a, T> {
    pub(crate) fn row(&self) -> &order::Model {
        &self.row
    }
    /// The transaction holding this lock: same-transaction collaborators (the event enqueue)
    /// write through it and nothing else.
    pub(in crate::infra) fn transaction(&self) -> &'a T {
        self.tx
    }
    pub(crate) async fn acquire(
        tx: &'a T,
        scope: &AccessScope,
        observed: &order::Model,
    ) -> Result<Self, ScopeError> {
        let row = order::Entity::find_by_id(observed.order_id)
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .one(tx)
            .await?
            .ok_or(ScopeError::Denied("hidden, changed, or foreign order"))?;
        if &row != observed {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        Ok(Self {
            tx,
            row,
            scope: scope.clone(),
        })
    }
    /// Lock the current row visible through `scope` without comparing observed facts; the
    /// caller (the authorization adapter) classifies changed facts versus lost access.
    pub(crate) async fn lock_current(
        tx: &'a T,
        scope: &AccessScope,
        order_id: Uuid,
    ) -> Result<Option<Self>, ScopeError> {
        let row = order::Entity::find_by_id(order_id)
            .lock_exclusive()
            .secure()
            .scope_with(scope)
            .one(tx)
            .await?;
        Ok(row.map(|row| Self {
            tx,
            row,
            scope: scope.clone(),
        }))
    }
    /// Used only after the full parent was authorized and locked in this transaction.
    fn child_scope(&self) -> AccessScope {
        AccessScope::for_resources(vec![self.row.order_id])
    }
    pub(crate) async fn replace(
        &mut self,
        proposed_scope: &AccessScope,
        proposed: order::Model,
    ) -> Result<(), ScopeError> {
        if proposed.order_id != self.row.order_id {
            return Err(ScopeError::Denied("hidden, changed, or foreign order"));
        }
        // The audit counter advances only with a sealed append (`repo::audit`), so a business
        // write can never open a chain gap or reuse a position (DESIGN §4.4).
        if proposed.audit_sequence != self.row.audit_sequence {
            return Err(ScopeError::Invalid(
                "audit_sequence advances only through the sealed audit writer",
            ));
        }
        validate_order(&proposed, proposed_scope)?;
        // Only changed fields become Set, preserving column-level immutable grants.
        let mut active = self.row.clone().into_active_model();

        for col in order::Column::iter() {
            if self.row.get(col) != proposed.get(col) {
                active.set(col, proposed.get(col));
            }
        }
        toolkit_db::secure::secure_update_with_scope::<order::Entity>(
            active,
            &self.scope,
            self.row.order_id,
            self.tx,
        )
        .await?;
        // A moved arrangement may leave the old scope; later writes in this transaction (the
        // audit counter) address the row through the scope that authorized its new facts.
        self.scope = proposed_scope.clone();
        self.row = proposed;
        Ok(())
    }
    /// Advance only `audit_sequence` (S2-06 writer). The row lock serializes allocation; the
    /// column grant, aggregate guard and `(order_id, sequence)` uniqueness back it in SQL.
    async fn advance_audit_sequence(&mut self, next: i64) -> Result<(), ScopeError> {
        let mut active = self.row.clone().into_active_model();
        active.set(order::Column::AuditSequence, next.into());
        toolkit_db::secure::secure_update_with_scope::<order::Entity>(
            active,
            &self.scope,
            self.row.order_id,
            self.tx,
        )
        .await?;
        self.row.audit_sequence = next;
        Ok(())
    }
    /// Storage-test fixture only: position the counter for deliberately unsealed raw rows that
    /// exercise database CHECKs, grants and FKs. Production code has no such path.
    #[cfg(test)]
    pub(in crate::infra::storage) async fn set_audit_sequence_for_raw_fixture(
        &mut self,
        next: i64,
    ) -> Result<(), ScopeError> {
        self.advance_audit_sequence(next).await
    }
}
pub mod mutable;

/// Test-only: this transaction's PostgreSQL backend PID, so a fault test can terminate the
/// session between the last statement and COMMIT (a lost commit acknowledgement).
///
/// # Errors
/// Store failure.
#[cfg(test)]
pub async fn backend_pid_for_test(
    runner: &impl DBRunner,
    order_id: Uuid,
) -> Result<i32, ScopeError> {
    use crate::infra::storage::sql;
    #[derive(Debug, sea_orm::FromQueryResult)]
    struct Pid {
        pid: i32,
    }
    let rows = order::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_resources(vec![order_id]))
        .project_all(runner, |q| {
            q.select_only()
                .column_as(sql::backend_pid(), "pid")
                .column_as(sql::row_anchor(), "n")
                .into_model::<Pid>()
        })
        .await?;
    rows.first()
        .map(|p| p.pid)
        .ok_or(ScopeError::Invalid("no backend pid"))
}

// The current Db facade supplies DbTx; legacy SecureConn supplies SecureTx.
// Seal the accepted runners so a connection cannot masquerade as a locked transaction.
//
// Single-statement repository leaves take `&impl DBRunner` (TOOLKIT-DB-001). This sealed bound
// stays only on the row-lock path: `LockedOrder`, the scoped lock entry points and the
// `FOR UPDATE` repository functions. A `SELECT ... FOR UPDATE` on a connection runs in its own
// autocommit transaction and releases the lock at once, so the lock would serialize nothing;
// toolkit-db has no transaction-only runner bound, so the gear seals one (#253).
mod transaction_seal {
    pub trait Sealed {}
    impl Sealed for toolkit_db::DbTx<'_> {}
    impl Sealed for toolkit_db::secure::SecureTx<'_> {}
}
pub trait TransactionRunner: DBRunner + transaction_seal::Sealed {}
impl TransactionRunner for toolkit_db::DbTx<'_> {}
impl TransactionRunner for toolkit_db::secure::SecureTx<'_> {}
pub mod claims;
pub mod read;
pub mod retention;

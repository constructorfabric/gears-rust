//! Privilege-census statements for the maintenance class connections (S2-11; migration 08).
//!
//! Startup attestation (`infra::workers::connections`) decides which statements each class must
//! and must not hold; this module owns the statements. Each one touches no evidence: the reads
//! and deletes run under a scope that matches nothing, so PostgreSQL checks the privilege at
//! planning time, and an append right is asked of PostgreSQL directly.
use super::{AccessScope, DBRunner, EntityTrait, QuerySelect, SecureEntityExt, Uuid};
use crate::infra::storage::{entity, sql};
use toolkit_db::secure::SecureDeleteExt;

/// One census statement.
#[derive(Debug, Clone, Copy)]
pub enum Probe {
    SelectOrder,
    SelectIdempotency,
    SelectAudit,
    SelectCheckpoint,
    SelectCheckpointMember,
    DeleteIdempotency,
    DeleteAudit,
    DeleteCheckpoint,
    InsertCheckpoint,
    InsertCheckpointMember,
}
impl Probe {
    /// The statement as attestation errors name it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::SelectOrder => "select bss_orders__order",
            Self::SelectIdempotency => "select bss_orders__idempotency",
            Self::SelectAudit => "select bss_orders__transition_audit",
            Self::SelectCheckpoint => "select bss_orders__audit_checkpoint",
            Self::SelectCheckpointMember => "select bss_orders__audit_checkpoint_member",
            Self::DeleteIdempotency => "delete bss_orders__idempotency (zero rows)",
            Self::DeleteAudit => "delete bss_orders__transition_audit (zero rows)",
            Self::DeleteCheckpoint => "delete bss_orders__audit_checkpoint (zero rows)",
            Self::InsertCheckpoint => "insert bss_orders__audit_checkpoint (privilege)",
            Self::InsertCheckpointMember => {
                "insert bss_orders__audit_checkpoint_member (privilege)"
            }
        }
    }
}

/// `has_table_privilege` answered false: the typed denial of [`insert_privilege`], classified
/// like a refused statement's SQLSTATE 42501.
#[derive(Debug, thiserror::Error)]
#[error("permission denied for table {0}")]
pub struct PrivilegeDenied(pub &'static str);

/// The session role's INSERT right on `table`, asked of PostgreSQL directly: an append cannot be
/// attempted without writing evidence, so `has_table_privilege` is read through a secured,
/// count-anchored projection (one row even when the scope matches nothing). A missing right is
/// the typed [`PrivilegeDenied`].
///
/// # Errors
/// [`PrivilegeDenied`], or a scope/store failure.
pub async fn insert_privilege(
    runner: &impl DBRunner,
    table: &'static str,
    scope: &AccessScope,
) -> anyhow::Result<()> {
    #[derive(Debug, sea_orm::FromQueryResult)]
    struct Privilege {
        allowed: bool,
    }
    let rows = entity::audit_checkpoint::Entity::find()
        .secure()
        .scope_with(scope)
        .project_all(runner, |q| {
            q.select_only()
                .column_as(sql::insert_privilege(table), "allowed")
                .column_as(sql::row_anchor(), "n")
                .into_model::<Privilege>()
        })
        .await?;
    match rows.first() {
        Some(row) if row.allowed => Ok(()),
        Some(_) => Err(PrivilegeDenied(table).into()),
        None => Err(anyhow::anyhow!("privilege check returned no row")),
    }
}

/// Run one census statement: a zero-row statement under a scope that matches nothing, so the
/// privilege is checked by PostgreSQL at planning time without touching evidence.
///
/// # Errors
/// The statement's refusal (SQLSTATE 42501 or [`PrivilegeDenied`]) or another store failure.
pub async fn run_probe(runner: &impl DBRunner, probe: Probe) -> anyhow::Result<()> {
    let nil_resource = AccessScope::for_resource(Uuid::nil());
    let nil_tenant = AccessScope::for_tenant(Uuid::nil());
    match probe {
        Probe::SelectOrder => {
            entity::order::Entity::find()
                .secure()
                .scope_with(&nil_resource)
                .one(runner)
                .await?;
        }
        Probe::SelectIdempotency => {
            entity::idempotency::Entity::find()
                .secure()
                .scope_with(&nil_resource)
                .one(runner)
                .await?;
        }
        Probe::SelectAudit => {
            entity::transition_audit::Entity::find()
                .secure()
                .scope_with(&nil_resource)
                .one(runner)
                .await?;
        }
        Probe::SelectCheckpoint => {
            entity::audit_checkpoint::Entity::find()
                .secure()
                .scope_with(&nil_tenant)
                .one(runner)
                .await?;
        }
        Probe::SelectCheckpointMember => {
            entity::audit_checkpoint_member::Entity::find()
                .secure()
                .scope_with(&nil_tenant)
                .one(runner)
                .await?;
        }
        Probe::DeleteIdempotency => {
            entity::idempotency::Entity::delete_many()
                .secure()
                .scope_with(&nil_resource)
                .exec(runner)
                .await?;
        }
        Probe::DeleteAudit => {
            entity::transition_audit::Entity::delete_many()
                .secure()
                .scope_with(&nil_resource)
                .exec(runner)
                .await?;
        }
        Probe::DeleteCheckpoint => {
            entity::audit_checkpoint::Entity::delete_many()
                .secure()
                .scope_with(&nil_tenant)
                .exec(runner)
                .await?;
        }
        Probe::InsertCheckpoint => {
            insert_privilege(runner, "bss_orders__audit_checkpoint", &nil_tenant).await?;
        }
        Probe::InsertCheckpointMember => {
            insert_privilege(runner, "bss_orders__audit_checkpoint_member", &nil_tenant).await?;
        }
    }
    Ok(())
}

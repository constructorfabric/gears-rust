//! Per-privilege-class database connections for the Orders-owned workers (S2-11; DESIGN
//! "Database privilege is runtime-owned"; CAPABILITIES: "use separate verifier/checkpoint/
//! retention/discovery credentials").
//!
//! Migration 08 creates the restricted `NOLOGIN` group roles; a deployment provisions one login
//! identity per class that is a member of exactly that group and hands its DSN to the gear as a
//! secret reference. The gear never widens a class: each worker reaches storage only through
//! its class connection, and startup attests every configured connection with zero-row probes
//! that must succeed where the class is granted and fail with `permission denied` where it is
//! not. A misprovisioned connection (missing or excess privilege) refuses startup.
//!
//! Advisory locks are never taken on these connections: the host connection owns the one
//! attested lock session (`lock_route`).
use std::collections::BTreeMap;

use sea_orm::EntityTrait;
use secrecy::{ExposeSecret, SecretString};
use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt};
use toolkit_db::{ConnectOpts, Db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::maintenance::MaintenanceTask;
use crate::infra::storage::entity;

/// The five restricted maintenance privilege classes of migration 08.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleClass {
    /// Read-only candidate discovery for the expiry/auto-void sweeps.
    Discovery,
    /// Idempotency-window cleanup and unresolved-execution discovery.
    Maintenance,
    /// Bounded deletion of the three retention stores.
    Retention,
    /// Read-only chain and checkpoint verification.
    Verifier,
    /// Checkpoint header/member append.
    Checkpoint,
}
impl RoleClass {
    /// The PostgreSQL group role the class connection must be a member of.
    #[must_use]
    pub fn role_name(self) -> &'static str {
        match self {
            Self::Discovery => "bss_orders_discovery",
            Self::Maintenance => "bss_orders_maintenance",
            Self::Retention => "bss_orders_retention",
            Self::Verifier => "bss_orders_verifier",
            Self::Checkpoint => "bss_orders_checkpoint",
        }
    }
    /// The configuration key of the class connection.
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Maintenance => "maintenance",
            Self::Retention => "retention",
            Self::Verifier => "verifier",
            Self::Checkpoint => "checkpoint",
        }
    }
    /// The class a maintenance task runs under.
    #[must_use]
    pub fn for_task(task: MaintenanceTask) -> Self {
        match task {
            MaintenanceTask::StateExpiry | MaintenanceTask::DraftAutoVoid => Self::Discovery,
            MaintenanceTask::IdempotencyCleanup => Self::Maintenance,
            MaintenanceTask::RetentionPurge => Self::Retention,
            MaintenanceTask::AuditVerification => Self::Verifier,
            MaintenanceTask::AuditCheckpoint => Self::Checkpoint,
        }
    }
    const ALL: [Self; 5] = [
        Self::Discovery,
        Self::Maintenance,
        Self::Retention,
        Self::Verifier,
        Self::Checkpoint,
    ];
}

/// Why a class connection was refused at startup. Messages never carry a DSN.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("maintenance connection `{0}` is an unresolved secret reference")]
    Unresolved(&'static str),
    #[error("maintenance connection `{class}` failed to connect: {detail}")]
    Connect { class: &'static str, detail: String },
    #[error("maintenance connection `{class}` lacks the `{role}` privilege: {probe}")]
    Missing {
        class: &'static str,
        role: &'static str,
        probe: &'static str,
    },
    #[error(
        "maintenance connection `{class}` holds a privilege outside `{role}`: {probe} succeeded"
    )]
    Excess {
        class: &'static str,
        role: &'static str,
        probe: &'static str,
    },
    #[error("maintenance connection `{class}` probe `{probe}` failed: {detail}")]
    Probe {
        class: &'static str,
        probe: &'static str,
        detail: String,
    },
}

/// The configured class connections.
#[derive(Clone)]
pub struct WorkerConnections {
    classes: BTreeMap<RoleClass, Db>,
}
impl std::fmt::Debug for WorkerConnections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConnections")
            .field("classes", &self.classes.keys().collect::<Vec<_>>())
            .finish()
    }
}
impl WorkerConnections {
    /// Open every configured class connection (bounded pools; no advisory-lock session is ever
    /// used on them) and attest its privileges before any worker may use it.
    ///
    /// # Errors
    /// An unresolved secret reference, a failed connection or a failed attestation; the error
    /// names the class and probe, never the DSN.
    pub async fn connect(
        dsns: &BTreeMap<RoleClass, SecretString>,
        opts: ConnectOpts,
    ) -> Result<Self, ConnectionError> {
        let mut classes = BTreeMap::new();
        for (class, dsn) in dsns {
            let dsn = dsn.expose_secret();
            if dsn.contains("${") {
                return Err(ConnectionError::Unresolved(class.config_key()));
            }
            let db = toolkit_db::connect_db(dsn, opts.clone())
                .await
                .map_err(|e| ConnectionError::Connect {
                    class: class.config_key(),
                    detail: sanitized(&e.to_string()),
                })?;
            classes.insert(*class, db);
        }
        let connections = Self { classes };
        connections.attest().await?;
        Ok(connections)
    }

    /// Test-only: already opened class connections (attested by the caller's fixture roles).
    #[cfg(test)]
    pub(crate) fn from_connections(classes: impl IntoIterator<Item = (RoleClass, Db)>) -> Self {
        Self {
            classes: classes.into_iter().collect(),
        }
    }

    /// The class connection, if configured.
    #[must_use]
    pub fn get(&self, class: RoleClass) -> Option<&Db> {
        self.classes.get(&class)
    }

    /// The configured classes.
    #[must_use]
    pub fn classes(&self) -> Vec<RoleClass> {
        self.classes.keys().copied().collect()
    }

    /// Zero-row privilege census of every configured class: the granted statements must run
    /// and the ungranted ones must be refused by PostgreSQL, so a connection provisioned with
    /// the wrong or a wider role never starts a worker.
    ///
    /// # Errors
    /// The first missing or excess privilege, or a probe that failed for another reason.
    pub async fn attest(&self) -> Result<(), ConnectionError> {
        for class in RoleClass::ALL {
            let Some(db) = self.classes.get(&class) else {
                continue;
            };
            for (probe, expected) in probes(class) {
                let outcome = run_probe(db, *probe).await;
                match (outcome, expected) {
                    (ProbeOutcome::Allowed, Expectation::Allowed)
                    | (ProbeOutcome::Denied, Expectation::Denied) => {}
                    (ProbeOutcome::Denied, Expectation::Allowed) => {
                        return Err(ConnectionError::Missing {
                            class: class.config_key(),
                            role: class.role_name(),
                            probe: probe.name(),
                        });
                    }
                    (ProbeOutcome::Allowed, Expectation::Denied) => {
                        return Err(ConnectionError::Excess {
                            class: class.config_key(),
                            role: class.role_name(),
                            probe: probe.name(),
                        });
                    }
                    (ProbeOutcome::Failed(detail), _) => {
                        return Err(ConnectionError::Probe {
                            class: class.config_key(),
                            probe: probe.name(),
                            detail,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// Strip anything that looks like a DSN from a driver message.
fn sanitized(message: &str) -> String {
    message
        .split_whitespace()
        .filter(|word| !word.contains("://"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy)]
enum Probe {
    SelectOrder,
    SelectIdempotency,
    SelectAudit,
    SelectCheckpoint,
    SelectCheckpointMember,
    DeleteIdempotency,
    DeleteAudit,
    DeleteCheckpoint,
}
impl Probe {
    fn name(self) -> &'static str {
        match self {
            Self::SelectOrder => "select bss_orders__order",
            Self::SelectIdempotency => "select bss_orders__idempotency",
            Self::SelectAudit => "select bss_orders__transition_audit",
            Self::SelectCheckpoint => "select bss_orders__audit_checkpoint",
            Self::SelectCheckpointMember => "select bss_orders__audit_checkpoint_member",
            Self::DeleteIdempotency => "delete bss_orders__idempotency (zero rows)",
            Self::DeleteAudit => "delete bss_orders__transition_audit (zero rows)",
            Self::DeleteCheckpoint => "delete bss_orders__audit_checkpoint (zero rows)",
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum Expectation {
    Allowed,
    Denied,
}
enum ProbeOutcome {
    Allowed,
    Denied,
    Failed(String),
}

/// The census per class: each granted statement class and one statement the class must not
/// hold (migration 08 grants).
fn probes(class: RoleClass) -> &'static [(Probe, Expectation)] {
    use Expectation::{Allowed, Denied};
    match class {
        RoleClass::Discovery => &[
            (Probe::SelectOrder, Allowed),
            (Probe::SelectIdempotency, Allowed),
            (Probe::DeleteIdempotency, Denied),
            (Probe::SelectAudit, Denied),
        ],
        RoleClass::Maintenance => &[
            (Probe::SelectOrder, Allowed),
            (Probe::SelectIdempotency, Allowed),
            (Probe::DeleteIdempotency, Allowed),
            (Probe::SelectAudit, Denied),
        ],
        RoleClass::Retention => &[
            (Probe::SelectAudit, Allowed),
            (Probe::DeleteAudit, Allowed),
            (Probe::SelectOrder, Denied),
            (Probe::DeleteIdempotency, Denied),
        ],
        RoleClass::Verifier => &[
            (Probe::SelectOrder, Allowed),
            (Probe::SelectAudit, Allowed),
            (Probe::SelectCheckpoint, Allowed),
            (Probe::SelectCheckpointMember, Allowed),
            (Probe::DeleteAudit, Denied),
            (Probe::DeleteCheckpoint, Denied),
        ],
        RoleClass::Checkpoint => &[
            (Probe::SelectOrder, Allowed),
            (Probe::SelectAudit, Allowed),
            (Probe::SelectCheckpoint, Allowed),
            (Probe::SelectCheckpointMember, Allowed),
            (Probe::DeleteCheckpoint, Denied),
            (Probe::DeleteIdempotency, Denied),
        ],
    }
}

/// A zero-row statement under a scope that matches nothing, so the privilege is checked by
/// PostgreSQL at planning time without touching evidence.
async fn run_probe(db: &Db, probe: Probe) -> ProbeOutcome {
    let nil_resource = AccessScope::for_resource(Uuid::nil());
    let nil_tenant = AccessScope::for_tenant(Uuid::nil());
    let result: Result<(), anyhow::Error> = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                match probe {
                    Probe::SelectOrder => {
                        entity::order::Entity::find()
                            .secure()
                            .scope_with(&nil_resource)
                            .one(tx)
                            .await?;
                    }
                    Probe::SelectIdempotency => {
                        entity::idempotency::Entity::find()
                            .secure()
                            .scope_with(&nil_resource)
                            .one(tx)
                            .await?;
                    }
                    Probe::SelectAudit => {
                        entity::transition_audit::Entity::find()
                            .secure()
                            .scope_with(&nil_resource)
                            .one(tx)
                            .await?;
                    }
                    Probe::SelectCheckpoint => {
                        entity::audit_checkpoint::Entity::find()
                            .secure()
                            .scope_with(&nil_tenant)
                            .one(tx)
                            .await?;
                    }
                    Probe::SelectCheckpointMember => {
                        entity::audit_checkpoint_member::Entity::find()
                            .secure()
                            .scope_with(&nil_tenant)
                            .one(tx)
                            .await?;
                    }
                    Probe::DeleteIdempotency => {
                        entity::idempotency::Entity::delete_many()
                            .secure()
                            .scope_with(&nil_resource)
                            .exec(tx)
                            .await?;
                    }
                    Probe::DeleteAudit => {
                        entity::transition_audit::Entity::delete_many()
                            .secure()
                            .scope_with(&nil_resource)
                            .exec(tx)
                            .await?;
                    }
                    Probe::DeleteCheckpoint => {
                        entity::audit_checkpoint::Entity::delete_many()
                            .secure()
                            .scope_with(&nil_tenant)
                            .exec(tx)
                            .await?;
                    }
                }
                Ok(())
            })
        })
        .await;
    match result {
        Ok(()) => ProbeOutcome::Allowed,
        Err(error) if error.to_string().contains("permission denied") => ProbeOutcome::Denied,
        Err(error) => ProbeOutcome::Failed(sanitized(&error.to_string())),
    }
}

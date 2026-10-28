//! Per-privilege-class database connections for the Orders-owned workers (S2-11; DESIGN
//! "Database privilege is runtime-owned"; CAPABILITIES: "use separate verifier/checkpoint/
//! retention/discovery credentials").
//!
//! Migration 08 creates the restricted `NOLOGIN` group roles; a deployment provisions one login
//! identity per class that is a member of exactly that group and hands its DSN to the gear as a
//! secret reference. The gear never widens a class: each worker reaches storage only through
//! its class connection, and startup attests every configured connection with zero-row probes
//! that must succeed where the class is granted and fail with `permission denied` (SQLSTATE
//! 42501) where it is not. A misprovisioned connection (missing or excess privilege) refuses
//! startup.
//!
//! Advisory locks are never taken on these connections: the host connection owns the one
//! attested lock session (`lock_route`).
use std::collections::BTreeMap;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use toolkit_db::{ConnectOpts, Db};

use crate::infra::engine::errors::is_insufficient_privilege;
use crate::infra::maintenance::MaintenanceTask;
use crate::infra::storage::repo::privilege::{self, PrivilegeDenied, Probe};

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
    /// The class DSN is still an unresolved `${...}` secret reference.
    #[error("maintenance connection `{0}` is an unresolved secret reference")]
    Unresolved(&'static str),
    /// Opening the class connection failed.
    #[error("maintenance connection `{class}` failed to connect: {detail}")]
    Connect {
        /// The class configuration key.
        class: &'static str,
        /// The driver message with anything DSN-like stripped.
        detail: String,
    },
    /// A probe the class must be allowed to run was denied.
    #[error("maintenance connection `{class}` lacks the `{role}` privilege: {probe}")]
    Missing {
        /// The class configuration key.
        class: &'static str,
        /// The PostgreSQL group role of the class.
        role: &'static str,
        /// The probe that was denied.
        probe: &'static str,
    },
    /// A probe the class must be refused succeeded.
    #[error(
        "maintenance connection `{class}` holds a privilege outside `{role}`: {probe} succeeded"
    )]
    Excess {
        /// The class configuration key.
        class: &'static str,
        /// The PostgreSQL group role of the class.
        role: &'static str,
        /// The probe that succeeded.
        probe: &'static str,
    },
    /// A probe failed for a reason other than `permission denied` (SQLSTATE 42501).
    #[error("maintenance connection `{class}` probe `{probe}` failed: {detail}")]
    Probe {
        /// The class configuration key.
        class: &'static str,
        /// The probe that failed.
        probe: &'static str,
        /// The database error detail.
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
            if dsn.expose_secret().contains("${") {
                return Err(ConnectionError::Unresolved(class.config_key()));
            }
            let dsn = with_session_bounds(dsn.expose_secret());
            let db = toolkit_db::connect_db(dsn.expose_secret(), opts.clone())
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the connection tests assert the configured class set; no runtime path lists it"
        )
    )]
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

/// Per-statement bound on every class connection (D-218): below the shortest worker pass
/// budget, so one hung statement fails its pass instead of outliving it.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Row-lock wait bound on every class connection (D-218): a conditional delete or checkpoint
/// append blocked behind another writer gives up and retries on the next tick.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The class DSN with `statement_timeout` and `lock_timeout` set as startup runtime parameters
/// (`options[...]`, applied by PostgreSQL to every pooled session). A deployment DSN that
/// already names either parameter keeps its own value.
fn with_session_bounds(dsn: &str) -> SecretString {
    let mut bounded = dsn.to_owned();
    for (name, value) in [
        ("statement_timeout", STATEMENT_TIMEOUT),
        ("lock_timeout", LOCK_TIMEOUT),
    ] {
        if bounded.contains(name) {
            continue;
        }
        bounded.push(if bounded.contains('?') { '&' } else { '?' });
        for part in ["options[", name, "]=", &value.as_millis().to_string(), "ms"] {
            bounded.push_str(part);
        }
    }
    SecretString::from(bounded)
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
            // Reads match the checkpoint class; only the append right tells the two apart.
            (Probe::InsertCheckpoint, Denied),
            (Probe::InsertCheckpointMember, Denied),
        ],
        RoleClass::Checkpoint => &[
            (Probe::SelectOrder, Allowed),
            (Probe::SelectAudit, Allowed),
            (Probe::SelectCheckpoint, Allowed),
            (Probe::SelectCheckpointMember, Allowed),
            (Probe::DeleteCheckpoint, Denied),
            (Probe::DeleteIdempotency, Denied),
            (Probe::InsertCheckpoint, Allowed),
            (Probe::InsertCheckpointMember, Allowed),
        ],
    }
}

/// Whether a probe failure is a privilege denial: SQLSTATE 42501 on a refused statement, or the
/// typed [`PrivilegeDenied`] of an INSERT right asked of PostgreSQL directly. Never decided by
/// the server's (localized) message text.
fn is_denied(error: &anyhow::Error) -> bool {
    error.downcast_ref::<PrivilegeDenied>().is_some() || is_insufficient_privilege(error)
}

/// One census probe in its own transaction (the statement itself is
/// [`privilege::run_probe`]), classified as allowed, denied or failed.
async fn run_probe(db: &Db, probe: Probe) -> ProbeOutcome {
    let result: Result<(), anyhow::Error> = db
        .transaction_ref_mapped(move |tx| Box::pin(privilege::run_probe(tx, probe)))
        .await;
    match result {
        Ok(()) => ProbeOutcome::Allowed,
        Err(error) if is_denied(&error) => ProbeOutcome::Denied,
        Err(error) => ProbeOutcome::Failed(sanitized(&error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_probe_is_typed_or_sqlstate_42501_never_message_text() {
        use crate::infra::engine::errors::tests::driver_error;
        assert!(is_denied(
            &PrivilegeDenied("bss_orders__audit_checkpoint").into()
        ));
        assert!(is_denied(&anyhow::Error::from(
            toolkit_db::secure::ScopeError::Db(driver_error("42501", "Zugriff verweigert"))
        )));
        assert!(!is_denied(&anyhow::anyhow!(
            "permission denied for table t"
        )));
    }

    #[test]
    fn class_dsns_carry_the_session_bounds_unless_the_deployment_names_them() {
        let bounded = with_session_bounds("postgres://u:p@db:5432/orders");
        assert_eq!(
            bounded.expose_secret(),
            "postgres://u:p@db:5432/orders?options[statement_timeout]=30000ms&options[lock_timeout]=5000ms"
        );
        let tuned = with_session_bounds(
            "postgres://u:p@db/orders?sslmode=require&options[lock_timeout]=1s",
        );
        assert_eq!(
            tuned.expose_secret(),
            "postgres://u:p@db/orders?sslmode=require&options[lock_timeout]=1s&options[statement_timeout]=30000ms"
        );
    }
}

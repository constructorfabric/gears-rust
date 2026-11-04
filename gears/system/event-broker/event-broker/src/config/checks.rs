//! Configuration-level startup checks (feature 0005 section 2.4). Any failure stops
//! the process from starting.
//!
//! The sections they read are held as written, as plain strings, so every
//! rule is enforced here rather than half of them inside `serde`: a failure
//! then always names the section, the rule it broke and what the rule
//! requires, in one shape. A failure reproduces only names that passed
//! validation - a registry name, a routing key that parsed as a GTS
//! identifier - and never a settings value or an unvalidated key.
//!
//! C1 and C2 need the linked providers and are enforced where the registry is
//! built, `crate::infra::backend::Registry::build`. C10's half on the gear's
//! own database needs the opened database, and [`gear_database`] is called
//! where it is known.

use std::collections::{BTreeMap, BTreeSet};

use event_broker_sdk::gts::TopicV1;
use sea_orm::DbBackend;
use toolkit_gts::GtsSchema;

use super::{BackendEntry, DeploymentMode, EventBrokerConfig};
use crate::domain::backend::{self, Routing};
use crate::domain::outbox::{INGEST_QUEUE_NAME, OutboxPartitions};

/// The deployment as the checks proved it: every name valid, every route
/// naming an entry, the instance-less route present, every entry routed to.
/// Constructed only by [`check`].
#[derive(Debug, Clone)]
pub struct Deployment {
    pub backends: BTreeMap<backend::Name, BackendEntry>,
    pub routing: Routing,
    /// The `default` outbox group's partitions. Checked only where ingest
    /// runs (C8 is an ingest rule); elsewhere nothing enqueues, and it holds
    /// the default.
    pub outbox_partitions: OutboxPartitions,
}

/// A startup check that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Feature 0005 §2.1. The key itself is not reproduced: it failed
    /// validation, so nothing proves it is safe to print.
    #[error(
        "backends: key {position} (in sorted order) is not a registry name; a registry name is 1 to 255 characters of [a-zA-Z0-9_-] (feature 0005 section 2.1)"
    )]
    InvalidBackendName { position: usize },

    /// Feature 0005 §2.2. Not reproduced, for the same reason.
    #[error(
        "backend_routing: key {position} (in sorted order) is not a topic key; a key is `{base}`, the instance-less key of a topic type derived from it, or a topic instance identifier (feature 0005 section 2.2)",
        base = TopicV1::TYPE_ID
    )]
    InvalidRoutingKey { position: usize },

    /// C3.
    #[error(
        "backend_routing: the route for '{key}' names no registry entry; every route names an entry in `backends` (feature 0005 section 2.4, C3)"
    )]
    UnknownBackend { key: String },

    /// C4.
    #[error(
        "backend_routing: there is no route for `{base}`; it is required so that every topic resolves to a registry entry (feature 0005 section 2.4, C4)",
        base = TopicV1::TYPE_ID
    )]
    MissingBaseRoute,

    /// C5.
    #[error(
        "backends: entry '{name}' is named by no route; every registry entry is named by at least one route (feature 0005 section 2.4, C5)"
    )]
    UnreferencedBackend { name: backend::Name },

    /// C10. A `SQLite` file serves one process.
    #[error(
        "backends: entry '{name}' has the SQLite backend type; a SQLite backend serves a single process, so only `standalone` mode runs one (feature 0005 section 2.4, C10)"
    )]
    SqliteBackendOutsideStandalone { name: backend::Name },

    /// C10, for the gear's own database.
    #[error(
        "database: the gear's database is SQLite; a SQLite database serves a single process, so only `standalone` mode runs on one (feature 0005 section 2.4, C10)"
    )]
    SqliteGearDatabaseOutsideStandalone,

    /// Named outbox groups are not implemented: their tables would have to be
    /// created at runtime. The name is not reproduced, being unvalidated.
    #[error(
        "outbox_groups: key {position} (in sorted order) is not `default`; only the reserved group `default` is supported, and named outbox groups are not implemented yet (feature 0005 section 2.5)"
    )]
    UnsupportedOutboxGroup { position: usize },

    /// C8.
    #[error(
        "outbox_groups: group 'default' has {partitions} partitions; `partitions` is a power of 2 from 1 to 64 (feature 0005 section 2.4, C8)"
    )]
    OutboxPartitionsOutOfRange { partitions: u32 },

    /// C8.
    #[error(
        "outbox_groups: group 'default' states `topics`; `default` always carries every topic and sets nothing but `partitions` (feature 0005 section 2.4, C8)"
    )]
    DefaultOutboxGroupTopics,
}

/// Runs every configuration-level check this instance's role is subject to.
///
/// `None` for `cluster_dispatcher`: it holds no registry and no routing, so
/// it carries the sections without being subject to their rules.
///
/// # Errors
/// The first rule the configuration breaks, checked in this order: registry
/// names; each routing key in sorted order, its form and then its target
/// (C3); the instance-less route (C4); unreferenced entries (C5); outside
/// `standalone`, a `SQLite` entry (C10); and, where
/// ingest runs, the outbox groups - a group other than `default`, then the
/// partition count, then `topics` (C8).
pub fn check(cfg: &EventBrokerConfig, mode: DeploymentMode) -> Result<Option<Deployment>, Error> {
    if mode.backend_role().is_none() {
        return Ok(None);
    }
    let backends = backends(cfg)?;
    let routing = routing(cfg)?;

    let routed: BTreeSet<&backend::Name> = routing.names().collect();
    if let Some(name) = backends.keys().find(|name| !routed.contains(name)) {
        return Err(Error::UnreferencedBackend { name: name.clone() });
    }
    if mode != DeploymentMode::Standalone
        && let Some(name) = backends
            .iter()
            .find(|(_, entry)| entry.r#type.as_ref() == sqlite_event_broker_plugin::BACKEND_TYPE)
            .map(|(name, _)| name)
    {
        return Err(Error::SqliteBackendOutsideStandalone { name: name.clone() });
    }
    let outbox_partitions = if mode.ingest_active() {
        outbox_partitions(cfg)?
    } else {
        OutboxPartitions::DEFAULT
    };
    Ok(Some(Deployment {
        backends,
        routing,
        outbox_partitions,
    }))
}

/// C10 for the gear's own database: every mode but `standalone` refuses a
/// `SQLite` one, `cluster_dispatcher` included.
///
/// # Errors
/// [`Error::SqliteGearDatabaseOutsideStandalone`] for a `SQLite` database
/// outside `standalone`.
pub fn gear_database(mode: DeploymentMode, engine: DbBackend) -> Result<(), Error> {
    if mode != DeploymentMode::Standalone && engine == DbBackend::Sqlite {
        return Err(Error::SqliteGearDatabaseOutsideStandalone);
    }
    Ok(())
}

/// The `default` group's partitions: C8, and no group but `default`.
fn outbox_partitions(cfg: &EventBrokerConfig) -> Result<OutboxPartitions, Error> {
    if let Some(position) = cfg
        .outbox_groups
        .keys()
        .position(|name| name != INGEST_QUEUE_NAME)
    {
        return Err(Error::UnsupportedOutboxGroup { position });
    }
    let Some(entry) = cfg.outbox_groups.get(INGEST_QUEUE_NAME) else {
        return Ok(OutboxPartitions::DEFAULT);
    };
    let partitions =
        OutboxPartitions::new(entry.partitions).ok_or(Error::OutboxPartitionsOutOfRange {
            partitions: entry.partitions,
        })?;
    if entry.topics.is_some() {
        return Err(Error::DefaultOutboxGroupTopics);
    }
    Ok(partitions)
}

/// The routing alone: registry names, routing keys, C3 and C4. Exposed for
/// the specification refresh, which resolves every topic's route against the
/// configuration it was started with.
///
/// # Errors
/// As [`check`], short of C5.
pub fn routing(cfg: &EventBrokerConfig) -> Result<Routing, Error> {
    let backends = backends(cfg)?;
    let mut base = None;
    let mut keyed = BTreeMap::new();
    for (position, (key, target)) in cfg.backend_routing.iter().enumerate() {
        if !is_topic_key(key) {
            return Err(Error::InvalidRoutingKey { position });
        }
        let name = backend::Name::parse(target)
            .ok()
            .filter(|name| backends.contains_key(name))
            .ok_or_else(|| Error::UnknownBackend { key: key.clone() })?;
        if key == TopicV1::TYPE_ID {
            base = Some(name);
        } else {
            keyed.insert(key.clone(), name);
        }
    }
    let base = base.ok_or(Error::MissingBaseRoute)?;
    Ok(Routing::new(base, keyed))
}

fn backends(cfg: &EventBrokerConfig) -> Result<BTreeMap<backend::Name, BackendEntry>, Error> {
    cfg.backends
        .iter()
        .enumerate()
        .map(|(position, (name, entry))| {
            backend::Name::parse(name)
                .map(|name| (name, entry.clone()))
                .map_err(|_| Error::InvalidBackendName { position })
        })
        .collect()
}

/// A GTS identifier under the topic base: the base itself, a type derived
/// from it, or a topic instance.
fn is_topic_key(key: &str) -> bool {
    key.starts_with(TopicV1::TYPE_ID) && gts::GtsId::try_new(key).is_ok()
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod checks_tests;

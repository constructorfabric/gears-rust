//! What differs between `MySQL` and `MariaDB` (`docs/DESIGN.md` §3.2): the
//! engine and version read from `SELECT VERSION()` at start, the oldest
//! release of each the plugin runs on, and the few server variables whose
//! names differ. Everything else the plugin says is said the same way to
//! both.

use std::fmt;

/// The server's engine and release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `MySQL` at the given release.
    Mysql(Version),
    /// `MariaDB` at the given release.
    Mariadb(Version),
}

/// `major.minor.patch` of a server release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// The major release number.
    pub major: u32,
    /// The minor release number.
    pub minor: u32,
    /// The patch release number.
    pub patch: u32,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mysql(version) => write!(f, "MySQL {version}"),
            Self::Mariadb(version) => write!(f, "MariaDB {version}"),
        }
    }
}

/// The oldest `MySQL` the plugin runs on: atomic DDL, `SKIP LOCKED`, and
/// in-place `ADD`/`DROP PARTITION` on `InnoDB`.
pub const MYSQL_FLOOR: Version = Version {
    major: 8,
    minor: 0,
    patch: 0,
};

/// The oldest `MariaDB` the plugin runs on: atomic DDL arrived in 10.6.
pub const MARIADB_FLOOR: Version = Version {
    major: 10,
    minor: 6,
    patch: 0,
};

impl Dialect {
    /// The dialect `SELECT VERSION()` names: `8.4.3`, `8.0.39-log`,
    /// `11.8.2-MariaDB-ubu2404`, `5.5.5-10.6.21-MariaDB` (the replication
    /// prefix older `MariaDB` clients see). `None` for anything else.
    pub fn parse(version: &str) -> Option<Self> {
        let mariadb = version.contains("MariaDB");
        let release = version.strip_prefix("5.5.5-").unwrap_or(version);
        let numbers = release.split(['-', '+', ' ']).next()?;
        let mut parts = numbers.split('.').map(str::parse::<u32>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().and_then(Result::ok).unwrap_or(0);
        let version = Version {
            major,
            minor,
            patch,
        };
        Some(if mariadb {
            Self::Mariadb(version)
        } else {
            Self::Mysql(version)
        })
    }

    /// The oldest release of this engine the plugin runs on.
    pub fn floor(self) -> Version {
        match self {
            Self::Mysql(_) => MYSQL_FLOOR,
            Self::Mariadb(_) => MARIADB_FLOOR,
        }
    }

    /// Whether this release is at or above its engine's floor.
    pub fn is_supported(self) -> bool {
        self.version() >= self.floor()
    }

    /// The release, whichever the engine.
    pub fn version(self) -> Version {
        match self {
            Self::Mysql(version) | Self::Mariadb(version) => version,
        }
    }

    /// The query of the session's isolation level: `MariaDB` before 11.1
    /// knows only `tx_isolation`, `MySQL` 8 only `transaction_isolation`.
    pub fn isolation_query(self) -> &'static str {
        match self {
            Self::Mysql(_) => "SELECT @@SESSION.transaction_isolation",
            Self::Mariadb(_) => "SELECT @@SESSION.tx_isolation",
        }
    }
}

/// The server variables a durable commit depends on, as read at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Durability {
    /// `innodb_flush_log_at_trx_commit`; `1` flushes the redo log at every
    /// commit.
    pub flush_log_at_trx_commit: i64,
    /// Whether the binary log is on.
    pub log_bin: bool,
    /// `sync_binlog`; `1` flushes the binary log at every commit.
    pub sync_binlog: i64,
    /// `binlog_format`; row events are needed under `READ COMMITTED`.
    pub binlog_format: String,
}

impl Durability {
    /// The query reading every field, in order; the same on both engines.
    pub const QUERY: &'static str = "SELECT CAST(@@GLOBAL.innodb_flush_log_at_trx_commit AS SIGNED), \
         CAST(@@GLOBAL.log_bin AS SIGNED), CAST(@@GLOBAL.sync_binlog AS SIGNED), \
         CAST(@@GLOBAL.binlog_format AS CHAR)";

    /// What keeps a commit from being durable once acknowledged on
    /// `dialect`; empty when nothing does. `MariaDB`'s `3` flushes at prepare
    /// and at commit, and is as durable as `1`; `MySQL` has no `3`.
    pub fn shortfalls(&self, dialect: Dialect) -> Vec<&'static str> {
        let mut shortfalls = Vec::new();
        match dialect {
            Dialect::Mysql(_) if self.flush_log_at_trx_commit != 1 => {
                shortfalls.push("innodb_flush_log_at_trx_commit is not 1");
            }
            Dialect::Mariadb(_) if !matches!(self.flush_log_at_trx_commit, 1 | 3) => {
                shortfalls.push("innodb_flush_log_at_trx_commit is neither 1 nor 3");
            }
            _ => {}
        }
        if self.log_bin && self.sync_binlog != 1 {
            shortfalls.push("the binary log is on and sync_binlog is not 1");
        }
        shortfalls
    }

    /// Whether `READ COMMITTED` writes are refused by the binary log: a
    /// statement-based binary log does not accept them.
    pub fn refuses_read_committed(&self) -> bool {
        self.log_bin && self.binlog_format.eq_ignore_ascii_case("STATEMENT")
    }
}

/// How the server is replicated, as far as it says: each field is the value
/// of its server variable, `None` where the server has no such variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// `wsrep_on`: Galera, on `MariaDB` builds that carry it.
    pub wsrep_on: Option<String>,
    /// `group_replication_single_primary_mode`: present once the Group
    /// Replication plugin is loaded.
    pub group_replication_single_primary: Option<String>,
}

impl Topology {
    /// The variables read, by name; `SHOW VARIABLES` answers no row for a
    /// variable the server does not have, where `@@name` would fail.
    pub const QUERY: &'static str = "SHOW GLOBAL VARIABLES WHERE Variable_name IN \
         ('wsrep_on', 'group_replication_single_primary_mode')";

    /// Why the plugin refuses to run on this server, if it does. Named locks
    /// (`GET_LOCK`) are local to one node, so with more than one node taking
    /// writes two instances could change one partition's segments at once.
    pub fn refusal(&self) -> Option<&'static str> {
        let on = |value: &Option<String>| {
            value
                .as_deref()
                .map(|v| v.eq_ignore_ascii_case("ON") || v == "1")
        };
        if on(&self.wsrep_on) == Some(true) {
            return Some(
                "Galera (wsrep_on is ON): named locks are per node, so maintenance on two nodes would collide; single-primary servers only",
            );
        }
        if on(&self.group_replication_single_primary) == Some(false) {
            return Some(
                "multi-primary Group Replication (group_replication_single_primary_mode is OFF): named locks are per member; single-primary servers only",
            );
        }
        None
    }
}

//! The cluster change log: how replicas tell each other what changed
//! (ADR-0020).
//!
//! A cluster watch event carries no value, so several quick writes to one key
//! reach a watcher as one event. The key therefore holds a short log: the last
//! changes, each with a sequence number and the process that made it. The log
//! has a random ID, set when it is created. A reader that sees another ID, a
//! gap in the sequence numbers, or a value it cannot read cannot know what it
//! missed, and empties its cache. The log's own ID and sequence numbers are
//! used, not the cluster entry's version, which restarts at 1 when the key is
//! deleted and created again.

use std::borrow::Borrow;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Change;
use crate::domain::services::management::MAX_ALIAS_LENGTH;

/// The cluster key of the log, on the `oagw`-scoped facade.
pub(crate) const LOG_KEY: &str = "proxy-config-changes";

/// The most changes the log keeps; the oldest go first.
pub(super) const MAX_CHANGES: usize = 128;

/// The most bytes the encoded log takes; the oldest changes go first. Well
/// under the k8s backend's default value limit of 256 KiB.
const MAX_BYTES: usize = 32 * 1024;

/// The wire format this build reads and writes.
const VERSION: u64 = 1;

/// A change as the log holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Logged {
    /// Its sequence number in the log.
    pub(crate) seq: u64,
    /// The process that made it.
    pub(crate) origin: Uuid,
    /// The change.
    pub(crate) change: Change,
}

/// Why a stored value is not a log this build reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Unreadable {
    /// Not a log of any version, or a version 1 log that breaks its rules.
    #[error("not a change log: {0}")]
    Malformed(String),
    /// A log of an older or unknown version.
    #[error("change log version {0} is not supported")]
    Version(u64),
    /// A log a newer build wrote during a rolling upgrade; this build must
    /// leave it alone.
    #[error("change log version {0} is newer than this build's ({VERSION})")]
    Newer(u64),
}

/// The log: its ID, the sequence number of its last change, and its changes,
/// oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeLog {
    id: Uuid,
    seq: u64,
    changes: Vec<ChangeV1Entry>,
}

impl ChangeLog {
    /// An empty log with a new ID.
    pub(crate) fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            seq: 0,
            changes: Vec::new(),
        }
    }

    /// The log's ID.
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    /// The sequence number of the last change; 0 before the first.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// The sequence number of the oldest change kept.
    pub(crate) fn oldest(&self) -> Option<u64> {
        self.changes.first().map(|entry| entry.seq)
    }

    /// The changes kept, oldest first.
    #[cfg(test)]
    pub(crate) fn changes(&self) -> impl Iterator<Item = Logged> + '_ {
        self.changes_after(0)
    }

    /// The changes kept with a sequence number above `last`, oldest first.
    pub(crate) fn changes_after(&self, last: u64) -> impl Iterator<Item = Logged> + '_ {
        let newer = self.changes.partition_point(|entry| entry.seq <= last);
        self.changes[newer..].iter().map(|entry| Logged {
            seq: entry.seq,
            origin: entry.origin,
            change: entry.c.to_change(),
        })
    }

    /// Append `changes` made by `origin`, drop the oldest beyond
    /// [`MAX_CHANGES`] or [`MAX_BYTES`] (the newest is always kept), and
    /// return the log as stored. [`Change::All`] is not logged. A log whose
    /// sequence numbers would run out starts over with a new ID.
    pub(crate) fn append<C: Borrow<Change>>(
        &mut self,
        origin: Uuid,
        changes: impl IntoIterator<Item = C>,
    ) -> Vec<u8> {
        for change in changes {
            let Some(c) = ChangeV1::from_change(change.borrow()) else {
                continue;
            };
            if self.seq == u64::MAX {
                *self = Self::new();
            }
            self.seq += 1;
            self.changes.push(ChangeV1Entry {
                seq: self.seq,
                origin,
                c,
            });
        }
        let over = self.changes.len().saturating_sub(MAX_CHANGES);
        self.changes.drain(..over);
        let encoded = self.encode();
        // Dropping the first of several entries shortens the encoded array
        // by the entry and its comma.
        let mut size = encoded.len();
        let mut over = 0;
        while size > MAX_BYTES && over + 1 < self.changes.len() {
            size -= encoded_len(&self.changes[over]) + 1;
            over += 1;
        }
        if over == 0 {
            return encoded;
        }
        self.changes.drain(..over);
        self.encode()
    }

    /// The log as stored in the cluster.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let log = ChangeLogV1 {
            v: VERSION,
            log: self.id,
            seq: self.seq,
            changes: &self.changes,
        };
        serde_json::to_vec(&log).expect("a change log always encodes")
    }

    /// The log a stored value holds.
    ///
    /// The version is read first, so a newer build's log is reported as
    /// such whatever its size. A version 1 log is then held to the bounds
    /// its writers keep; the byte bound is checked before the full parse.
    ///
    /// # Errors
    ///
    /// [`Unreadable`] when the value is no version 1 log, breaks those
    /// bounds, or its sequence numbers do not count up by one to the last.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Unreadable> {
        let malformed = |e: serde_json::Error| Unreadable::Malformed(e.to_string());
        let version: VersionProbe = serde_json::from_slice(bytes).map_err(malformed)?;
        if version.v > VERSION {
            return Err(Unreadable::Newer(version.v));
        }
        if version.v != VERSION {
            return Err(Unreadable::Version(version.v));
        }
        if bytes.len() > MAX_BYTES {
            return Err(Unreadable::Malformed(format!(
                "{} bytes, over the {MAX_BYTES} a log takes",
                bytes.len()
            )));
        }
        let log: StoredLogV1 = serde_json::from_slice(bytes).map_err(malformed)?;
        if log.changes.len() > MAX_CHANGES {
            return Err(Unreadable::Malformed(format!(
                "{} changes, over the {MAX_CHANGES} a log keeps",
                log.changes.len()
            )));
        }
        let long_alias = log.changes.iter().any(|entry| match &entry.c {
            ChangeV1::Upstream {
                tenant: _,
                id: _,
                alias,
            } => alias.len() > MAX_ALIAS_LENGTH,
            ChangeV1::UpstreamDeleted { .. }
            | ChangeV1::Route { .. }
            | ChangeV1::RouteDeleted { .. }
            | ChangeV1::RoutesOfUpstream { .. } => false,
        });
        if long_alias {
            return Err(Unreadable::Malformed(format!(
                "an alias longer than {MAX_ALIAS_LENGTH} bytes"
            )));
        }
        let counts_up = log
            .changes
            .windows(2)
            .all(|w| w[0].seq.checked_add(1) == Some(w[1].seq))
            && log.changes.last().is_none_or(|last| last.seq == log.seq);
        if !counts_up {
            return Err(Unreadable::Malformed(
                "sequence numbers do not count up to the last".to_owned(),
            ));
        }
        Ok(Self {
            id: log.log,
            seq: log.seq,
            changes: log.changes,
        })
    }
}

/// The bytes one change takes in the encoded log.
fn encoded_len(entry: &ChangeV1Entry) -> usize {
    serde_json::to_vec(entry)
        .expect("a change always encodes")
        .len()
}

/// The version every log value carries, read before the rest.
#[derive(Deserialize)]
struct VersionProbe {
    v: u64,
}

/// Version 1 of the stored log, as written: it borrows the changes.
#[derive(Serialize)]
struct ChangeLogV1<'a> {
    v: u64,
    log: Uuid,
    seq: u64,
    changes: &'a [ChangeV1Entry],
}

/// Version 1 of the stored log, as read. The fields of [`ChangeLogV1`].
#[derive(Deserialize)]
struct StoredLogV1 {
    log: Uuid,
    seq: u64,
    changes: Vec<ChangeV1Entry>,
}

/// One change in a version 1 log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChangeV1Entry {
    seq: u64,
    origin: Uuid,
    c: ChangeV1,
}

/// A [`Change`] in a version 1 log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ChangeV1 {
    Upstream {
        tenant: Uuid,
        id: Uuid,
        alias: String,
    },
    UpstreamDeleted {
        tenant: Uuid,
        id: Uuid,
    },
    Route {
        tenant: Uuid,
        id: Uuid,
        upstream: Uuid,
    },
    RouteDeleted {
        tenant: Uuid,
        id: Uuid,
    },
    RoutesOfUpstream {
        tenant: Uuid,
        upstream: Uuid,
    },
}

impl ChangeV1 {
    /// `change` on the wire; `None` for [`Change::All`].
    fn from_change(change: &Change) -> Option<Self> {
        Some(match change {
            Change::Upstream {
                tenant_id,
                id,
                alias,
            } => Self::Upstream {
                tenant: *tenant_id,
                id: *id,
                alias: alias.clone(),
            },
            Change::UpstreamDeleted { tenant_id, id } => Self::UpstreamDeleted {
                tenant: *tenant_id,
                id: *id,
            },
            Change::Route {
                tenant_id,
                id,
                upstream_id,
            } => Self::Route {
                tenant: *tenant_id,
                id: *id,
                upstream: *upstream_id,
            },
            Change::RouteDeleted { tenant_id, id } => Self::RouteDeleted {
                tenant: *tenant_id,
                id: *id,
            },
            Change::RoutesOfUpstream {
                tenant_id,
                upstream_id,
            } => Self::RoutesOfUpstream {
                tenant: *tenant_id,
                upstream: *upstream_id,
            },
            Change::All => return None,
        })
    }

    fn to_change(&self) -> Change {
        match self {
            Self::Upstream { tenant, id, alias } => Change::Upstream {
                tenant_id: *tenant,
                id: *id,
                alias: alias.clone(),
            },
            Self::UpstreamDeleted { tenant, id } => Change::UpstreamDeleted {
                tenant_id: *tenant,
                id: *id,
            },
            Self::Route {
                tenant,
                id,
                upstream,
            } => Change::Route {
                tenant_id: *tenant,
                id: *id,
                upstream_id: *upstream,
            },
            Self::RouteDeleted { tenant, id } => Change::RouteDeleted {
                tenant_id: *tenant,
                id: *id,
            },
            Self::RoutesOfUpstream { tenant, upstream } => Change::RoutesOfUpstream {
                tenant_id: *tenant,
                upstream_id: *upstream,
            },
        }
    }
}

#[cfg(test)]
#[path = "change_log_tests.rs"]
mod tests;

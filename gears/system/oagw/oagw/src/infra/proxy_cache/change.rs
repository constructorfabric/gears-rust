//! Configuration changes, and the cache entries each one evicts (ADR-0020).
//!
//! An alias entry depends only on the upstream rows with its alias in its
//! chain; a route entry only on the route rows of its upstream. A change
//! evicts every entry that holds the written row, and every entry the row
//! could join: an upstream joins the entries of its alias whose chain holds
//! its tenant (an ancestor's write reaches its descendants that way), a
//! route joins its upstream's entry.

use std::fmt;

use uuid::Uuid;

use super::AliasKey;
use super::section::{Evict, Index};
use crate::domain::repo::RowKey;

/// One configuration write, built from the repository call's own arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// An upstream was created or updated.
    Upstream {
        /// The upstream's tenant.
        tenant_id: Uuid,
        /// The upstream's ID.
        id: Uuid,
        /// The normalized alias it was written with.
        alias: String,
    },
    /// An upstream was deleted, and its routes with it.
    UpstreamDeleted {
        /// The upstream's tenant.
        tenant_id: Uuid,
        /// The upstream's ID.
        id: Uuid,
    },
    /// A route was created or updated.
    Route {
        /// The route's tenant, which is also its upstream's.
        tenant_id: Uuid,
        /// The route's ID.
        id: Uuid,
        /// The upstream it was written to.
        upstream_id: Uuid,
    },
    /// A route was deleted. Its upstream is not an argument of the delete;
    /// the route's ID finds its entry.
    RouteDeleted {
        /// The route's tenant.
        tenant_id: Uuid,
        /// The route's ID.
        id: Uuid,
    },
    /// The routes left on a deleted upstream were deleted.
    RoutesOfUpstream {
        /// The upstream's tenant.
        tenant_id: Uuid,
        /// The upstream's ID.
        upstream_id: Uuid,
    },
    /// Anything may have changed: the change log watcher's flush.
    #[cfg_attr(
        all(not(feature = "cluster"), not(test)),
        expect(dead_code, reason = "only the change log watcher flushes the cache")
    )]
    All,
}

impl Change {
    /// What the change evicts from the alias section.
    pub(super) fn aliases(&self, index: &Index<AliasKey>) -> Evict<AliasKey> {
        match self {
            Self::Upstream {
                tenant_id,
                id,
                alias,
            } => Evict::Keys(
                index
                    .holding(*id)
                    .chain(
                        index
                            .with_alias(alias)
                            .filter(|key| key.chain.contains(tenant_id)),
                    )
                    .cloned()
                    .collect(),
            ),
            Self::UpstreamDeleted { id, .. } => Evict::Keys(index.holding(*id).cloned().collect()),
            Self::Route { .. } | Self::RouteDeleted { .. } | Self::RoutesOfUpstream { .. } => {
                Evict::Nothing
            }
            Self::All => Evict::Everything,
        }
    }

    /// What the change evicts from the route section.
    pub(super) fn routes(&self, index: &Index<RowKey>) -> Evict<RowKey> {
        match self {
            Self::Upstream { .. } => Evict::Nothing,
            Self::UpstreamDeleted { tenant_id, id } => Evict::Keys(vec![RowKey {
                tenant_id: *tenant_id,
                id: *id,
            }]),
            Self::Route {
                tenant_id,
                id,
                upstream_id,
            } => Evict::Keys(
                index
                    .holding(*id)
                    .copied()
                    .chain([RowKey {
                        tenant_id: *tenant_id,
                        id: *upstream_id,
                    }])
                    .collect(),
            ),
            Self::RouteDeleted { tenant_id: _, id } => {
                Evict::Keys(index.holding(*id).copied().collect())
            }
            Self::RoutesOfUpstream {
                tenant_id,
                upstream_id,
            } => Evict::Keys(vec![RowKey {
                tenant_id: *tenant_id,
                id: *upstream_id,
            }]),
            Self::All => Evict::Everything,
        }
    }
}

/// The change's kind and the IDs it names, for logs.
impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upstream {
                tenant_id,
                id,
                alias: _,
            } => {
                write!(f, "upstream {id} of tenant {tenant_id}")
            }
            Self::UpstreamDeleted { tenant_id, id } => {
                write!(f, "upstream_deleted {id} of tenant {tenant_id}")
            }
            Self::Route {
                tenant_id,
                id,
                upstream_id,
            } => write!(
                f,
                "route {id} of tenant {tenant_id} on upstream {upstream_id}"
            ),
            Self::RouteDeleted { tenant_id, id } => {
                write!(f, "route_deleted {id} of tenant {tenant_id}")
            }
            Self::RoutesOfUpstream {
                tenant_id,
                upstream_id,
            } => write!(f, "routes_of_upstream {upstream_id} of tenant {tenant_id}"),
            Self::All => f.write_str("all"),
        }
    }
}

/// Several changes for one log line: the first [`Self::SHOWN`], separated by
/// `; `, then how many more there are.
#[cfg(feature = "cluster")]
pub(crate) struct Changes<'a>(
    /// The changes to name.
    pub(crate) &'a [Change],
);

#[cfg(feature = "cluster")]
impl Changes<'_> {
    /// The most changes a line names.
    const SHOWN: usize = 16;
}

#[cfg(feature = "cluster")]
impl fmt::Display for Changes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, change) in self.0.iter().take(Self::SHOWN).enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{change}")?;
        }
        if let Some(more) = self.0.len().checked_sub(Self::SHOWN).filter(|n| *n > 0) {
            write!(f, "; and {more} more")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "change_tests.rs"]
mod tests;

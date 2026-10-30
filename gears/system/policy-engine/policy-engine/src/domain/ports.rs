//! Outbound ports of the domain: tenant hierarchy, types registry and the
//! active-content read of the decision path.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use tenant_resolver_sdk::{BarrierMode, TenantId};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::model::{Assignment, Document, VersionId};

/// Why a dependency could not answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortError {
    /// The dependency did not answer in time.
    #[error("dependency timed out")]
    Timeout,
    /// The dependency failed or answered unusably.
    #[error("dependency unavailable: {0}")]
    Unavailable(String),
    /// The dependency does not know (or will not show) the subject.
    #[error("not found: {0}")]
    NotFound(String),
}

/// Tenant hierarchy reads, always under the caller's context.
#[async_trait]
pub trait HierarchyPort: Send + Sync {
    /// The tenant and its ancestors, nearest first, stopping at the first
    /// self-managed barrier. Every tenant status counts.
    async fn ancestors(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
    ) -> Result<Vec<Uuid>, PortError>;

    /// Whether `from_tenant` is `target_tenant` or an ancestor of it.
    async fn is_reachable(
        &self,
        ctx: &SecurityContext,
        from_tenant: TenantId,
        target_tenant: TenantId,
        barrier: BarrierMode,
    ) -> Result<bool, PortError>;

    /// The tenant and all its descendants, barriers ignored, each with
    /// whether it is self-managed, so barrier-respecting reachability within
    /// it is known without asking again. Every tenant status counts.
    async fn subtree(&self, ctx: &SecurityContext, root: TenantId) -> Result<Subtree, PortError>;
}

/// A tenant and all its descendants, barriers ignored, each with its parent
/// and whether it is self-managed (a barrier).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtree {
    root: Uuid,
    /// Each descendant's parent and barrier flag.
    nodes: HashMap<Uuid, (Uuid, bool)>,
}

impl Subtree {
    /// `root` with `descendants`, each given as `(tenant, parent,
    /// self_managed)`. A descendant whose parent chain does not lead to
    /// `root` is left out.
    #[must_use]
    pub fn new(root: Uuid, descendants: impl IntoIterator<Item = (Uuid, Uuid, bool)>) -> Self {
        let mut tree = Self {
            root,
            nodes: descendants
                .into_iter()
                .filter(|&(tenant, _, _)| tenant != root)
                .map(|(tenant, parent, managed)| (tenant, (parent, managed)))
                .collect(),
        };
        let attached = tree
            .nodes
            .iter()
            .filter(|&(&tenant, _)| tree.leads_to_root(tenant))
            .map(|(&tenant, &node)| (tenant, node))
            .collect();
        tree.nodes = attached;
        tree
    }

    /// The tenant the subtree hangs from.
    #[must_use]
    pub const fn root(&self) -> Uuid {
        self.root
    }

    /// Whether `tenant` is the root or one of its descendants.
    #[must_use]
    pub fn contains(&self, tenant: Uuid) -> bool {
        tenant == self.root || self.nodes.contains_key(&tenant)
    }

    /// The root and all its descendants, the root first.
    #[must_use]
    pub fn tenants(&self) -> Vec<Uuid> {
        let mut tenants = Vec::with_capacity(self.nodes.len() + 1);
        tenants.push(self.root);
        tenants.extend(self.nodes.keys().copied());
        tenants
    }

    /// The root and the descendants it reaches with barriers respected, the
    /// root first.
    #[must_use]
    pub fn reachable(&self) -> Vec<Uuid> {
        self.tenants()
            .into_iter()
            .filter(|&tenant| self.reaches(self.root, tenant))
            .collect()
    }

    /// Whether `from` is `target` or an ancestor of it with no self-managed
    /// tenant below `from` on the way, both within the subtree: the tenant
    /// resolver's barrier-respecting reachability.
    #[must_use]
    pub fn reaches(&self, from: Uuid, target: Uuid) -> bool {
        let mut current = target;
        for _ in 0..=self.nodes.len() {
            if current == from {
                return true;
            }
            match self.nodes.get(&current) {
                Some(&(parent, false)) => current = parent,
                _ => return false,
            }
        }
        false
    }

    /// Whether `tenant`'s parent chain leads to the root (and not round a
    /// cycle).
    fn leads_to_root(&self, tenant: Uuid) -> bool {
        let mut current = tenant;
        for _ in 0..=self.nodes.len() {
            if current == self.root {
                return true;
            }
            match self.nodes.get(&current) {
                Some(&(parent, _)) => current = parent,
                None => return false,
            }
        }
        false
    }
}

/// Lookup of concrete type identifiers, used at validation time only.
#[async_trait]
pub trait TypeCatalogPort: Send + Sync {
    /// The subset of `ids` the types registry knows.
    async fn known_types(&self, ids: &[String]) -> Result<HashSet<String>, PortError>;
}

/// An assignment with the id of its bundle's active version. Carries no
/// document content: the decision path reads content only for versions its
/// compile cache does not hold ([`BindingSource::version_documents`]).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveBinding {
    /// The assignment.
    pub assignment: Assignment,
    /// The active version of the assignment's bundle.
    pub version_id: VersionId,
}

/// Reads the active bindings of a tenant chain, and version content on
/// demand.
#[async_trait]
pub trait BindingSource: Send + Sync {
    /// Assignments targeting any of `tenants`, made by one of `tenants`,
    /// whose bundle has an active version.
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError>;

    /// The documents of each of `versions`. A version absent from the
    /// result has no documents.
    async fn version_documents(
        &self,
        versions: &[VersionId],
    ) -> Result<HashMap<VersionId, Vec<Document>>, PortError>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "ports_tests.rs"]
mod ports_tests;

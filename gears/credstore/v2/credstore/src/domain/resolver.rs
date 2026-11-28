//! Tenant hierarchy resolution boundary used by inherited-secret lookup.

use async_trait::async_trait;
use credstore_sdk::TenantId;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// A tenant's ancestor chain with the visibility of its isolation barriers.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestorChain {
    /// `[req, parent, …, root]`, isolation barriers ignored (what inheritance walks).
    pub ids: Vec<Uuid>,
    /// Length of the prefix of `ids` the caller sees with barriers respected:
    /// `req` itself, plus ancestors up to and including the first
    /// self-managed one; just `req` when `req` is self-managed.
    pub visible_len: usize,
}

impl AncestorChain {
    /// A chain with no isolation barrier: everything is visible.
    #[must_use]
    pub fn unbarriered(ids: Vec<Uuid>) -> Self {
        let visible_len = ids.len();
        Self { ids, visible_len }
    }

    /// The part of the chain the caller sees with barriers respected.
    #[must_use]
    pub fn visible(&self) -> &[Uuid] {
        &self.ids[..self.visible_len.min(self.ids.len())]
    }
}

/// Resolves a tenant's ancestor chain (self first, root last).
#[async_trait]
pub trait TenantDirectory: Send + Sync {
    /// Returns `[req, parent, …, root]` (req first), self included, with the
    /// length of the part visible across isolation barriers.
    async fn ancestor_chain(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
    ) -> Result<AncestorChain, DomainError>;

    /// True iff `ancestor` is a strict ancestor of `descendant` in the
    /// tenant hierarchy, isolation barriers ignored (the hierarchy
    /// inheritance walks). A `descendant` tenant-resolver does not know
    /// (deleted, rows left behind) is simply not a descendant.
    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor: TenantId,
        descendant: TenantId,
    ) -> Result<bool, DomainError>;
}

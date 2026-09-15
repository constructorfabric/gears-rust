// Created: 2026-09-07 by Constructor Tech
//! Tenant access restrictions: what one tenant may do with one setting.
//!
//! A restriction is a sparse row — `read_only` or `hidden` for one
//! `(declaration, tenant)` pair — recorded by an administrator of a strict
//! ancestor. Absence means `overridable`. Effective access is the strictest row
//! on the tenant's root-to-self chain, so a descendant can never widen an
//! ancestor's restriction. Access gates the caller, never the value: a
//! restricted tenant's existing override keeps resolving and being inherited,
//! and the in-process reader is not gated at all.

pub mod repo;
pub mod service;

use std::sync::Arc;

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::api::precondition::ETag;
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveCache, TenantHierarchy};

pub use repo::AccessRepository;
pub use service::{AccessActor, AccessReadout, AccessService};

/// What a tenant may do with a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TenantAccess {
    /// Read and set its own override: the default, represented by no row.
    Overridable,
    /// Read only.
    ReadOnly,
    /// Not even visible: reported as absent on every administrative surface.
    Hidden,
}

impl TenantAccess {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Overridable => "overridable",
            Self::ReadOnly => "read_only",
            Self::Hidden => "hidden",
        }
    }

    /// From the stored spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "overridable" => Some(Self::Overridable),
            "read_only" => Some(Self::ReadOnly),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

/// One stored restriction row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    /// Row identity.
    pub id: Uuid,
    /// The declaration the row is about.
    pub declaration_id: Uuid,
    /// The tenant restricted — never the one who decided.
    pub tenant_id: Uuid,
    /// `read_only` or `hidden`; never `overridable`, which is no row.
    pub access: TenantAccess,
    /// The administrator of a strict ancestor who recorded it.
    pub set_by: String,
    /// When it was first recorded.
    pub created_at: OffsetDateTime,
    /// Row version, which the restriction state tag derives from.
    pub updated_at: OffsetDateTime,
}

/// What a set supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictionDraft {
    /// The declaration.
    pub declaration_id: Uuid,
    /// The tenant restricted.
    pub tenant_id: Uuid,
    /// `read_only` or `hidden`.
    pub access: TenantAccess,
    /// Who recorded it.
    pub set_by: String,
}

/// A tenant's effective access for one setting, and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveAccess {
    /// The strictest access on the chain.
    pub access: TenantAccess,
    /// The tenant whose row supplied it; absent for `overridable`.
    pub supplied_by: Option<Uuid>,
}

impl EffectiveAccess {
    /// The default when no row exists anywhere on the chain.
    pub const OVERRIDABLE: Self = Self {
        access: TenantAccess::Overridable,
        supplied_by: None,
    };

    /// Whether the setting is invisible to the tenant.
    #[must_use]
    pub const fn is_hidden(self) -> bool {
        matches!(self.access, TenantAccess::Hidden)
    }
}

/// The strictest row on a chain: `hidden` over `read_only`, absence
/// `overridable`.
///
/// Rows outside the chain are the caller's to exclude; this takes what the
/// exact-match set query returned.
#[must_use]
pub fn strictest(rows: &[Restriction]) -> EffectiveAccess {
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-3
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-4
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-5
    // Absence is the default, not a state to materialize: nothing is created
    // here, and nothing here ever chooses a value row.
    rows.iter()
        .max_by_key(|row| row.access)
        .map_or(EffectiveAccess::OVERRIDABLE, |row| EffectiveAccess {
            access: row.access,
            supplied_by: Some(row.tenant_id),
        })
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-5
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-4
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-resolve:p1:inst-ta-resolve-3
}

/// The tag of a pair that holds no row.
pub const ABSENT_RESTRICTION_TAG: &str = "absent";

/// The restriction state tag a mutation of a pair must present.
#[must_use]
pub fn restriction_tag(row: Option<&Restriction>) -> ETag {
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-1
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-2
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-3
    // A stored row: its normalized UTC `updated_at`, as every other tag in this
    // service. No row: a tag stable for the pair and distinct from every
    // stored-row tag, so a PUT may create a row only against the caller's
    // knowledge that none existed. Comparison and mutation share one
    // transaction, so a row changed in between fails the comparison.
    row.map_or_else(
        || ETag::new(ABSENT_RESTRICTION_TAG),
        |row| ETag::new(row.updated_at.unix_timestamp_nanos().to_string()),
    )
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-3
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-2
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-etag:p1:inst-ta-etag-1
}

/// Whether `caller` may restrict `target`: a strict descendant, reachable
/// without crossing a standalone barrier, and not standalone itself.
///
/// # Errors
/// [`DomainError`] when the tenant resolver cannot answer.
pub async fn may_restrict(
    hierarchy: &dyn TenantHierarchy,
    caller: Uuid,
    target: Uuid,
) -> Result<bool, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-1
    // A tenant that could restrict itself could lift the restriction again.
    if caller == target {
        return Ok(false);
    }
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-1
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-2
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-3
    // Ancestors, siblings and tenants outside the subtree fail the first
    // question; a standalone tenant, or one below a standalone tenant, fails
    // the second — nothing traverses downward into it from above.
    if !hierarchy.is_within_subtree(caller, target).await?
        || hierarchy.is_standalone(target).await?
    {
        return Ok(false);
    }
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-3
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-2
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-4
    Ok(true)
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-target:p1:inst-ta-target-4
}

/// Evict a setting's cached entries for a tenant and every descendant after
/// its access changed, whatever the setting's scope class: their effective
/// access may have changed even where their effective value has not.
///
/// # Errors
/// [`DomainError`] when the tenant resolver cannot list the descendants.
pub async fn evict_access_change(
    cache: &EffectiveCache,
    hierarchy: &dyn TenantHierarchy,
    key: &str,
    tenant: Uuid,
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-1
    let mut tenants = hierarchy.descendants(tenant).await?;
    tenants.push(tenant);
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-1
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-2
    cache.invalidate_tenants(key, &tenants);
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-2
    // @cpt-begin:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-3
    // Locally only; peer replicas converge through the R2 broadcast.
    Ok(())
    // @cpt-end:cpt-cf-settings-service-algo-tenant-access-evict:p1:inst-ta-evict-3
}

/// A port for callers that only need the effective access, such as the write
/// path checking the caller's own.
#[async_trait]
pub trait AccessResolution: Send + Sync {
    /// The effective access of `tenant` for the declaration.
    ///
    /// # Errors
    /// [`DomainError`] when the chain or the rows cannot be read.
    async fn effective_access(
        &self,
        declaration_id: Uuid,
        tenant: Uuid,
    ) -> Result<EffectiveAccess, DomainError>;
}

/// Shared handle type for the port.
pub type SharedAccessResolution = Arc<dyn AccessResolution>;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

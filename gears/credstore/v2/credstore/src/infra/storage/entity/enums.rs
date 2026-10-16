//! Storage-only enumerations for the `SMALLINT` columns of the credstore
//! tables.
//!
//! The numbers are a storage contract: they are written by the migrations'
//! `CHECK` constraints and by every row already stored, so they never change,
//! and a retired number is never reused. They live here and nowhere else: the
//! domain model and the SDK know the variants, not the numbers, and these types
//! derive no `Serialize`/`Deserialize`, so the numbers cannot reach the wire.
//!
//! Each type is named `Db*` so it cannot be confused with the domain or SDK type
//! it mirrors (or with the metrics `CleanupOp`). The conversions to and from
//! those types are exhaustive matches: a variant added on either side fails to
//! compile until its counterpart and the migrations' `CHECK` agree.

use credstore_sdk::{DestroySelector, SharingMode, ValueVersion};
use sea_orm::entity::prelude::*;

use crate::domain::secret::model::{CleanupTask, Fallback, SecretStatus};

/// `credstore_secrets.sharing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum DbSharing {
    #[sea_orm(num_value = 1)]
    Private,
    #[sea_orm(num_value = 2)]
    Tenant,
    #[sea_orm(num_value = 3)]
    Shared,
}

/// `credstore_secrets.status`. Codes 1 (provisioning) and 3 (deprovisioning)
/// are retired by ADR-0006: reserved, never reassigned, and rejected by the
/// column's `CHECK`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum DbStatus {
    #[sea_orm(num_value = 2)]
    Active,
    #[sea_orm(num_value = 4)]
    Declared,
}

/// `credstore_secrets.fallback`; `Inherit` is the column default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum DbFallback {
    #[sea_orm(num_value = 1)]
    Inherit,
    #[sea_orm(num_value = 2)]
    None,
}

/// `credstore_store_cleanup.op`; `Purge` runs `delete_key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum DbCleanupOp {
    #[sea_orm(num_value = 1)]
    Purge,
    #[sea_orm(num_value = 2)]
    Destroy,
}

/// `credstore_store_cleanup.selector`, set for `Destroy` only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum DbDestroySelector {
    #[sea_orm(num_value = 1)]
    Below,
    #[sea_orm(num_value = 2)]
    Exactly,
}

// ---------------------------------------------------------------------------
// Storage <-> domain / SDK conversions
// ---------------------------------------------------------------------------

impl From<SharingMode> for DbSharing {
    fn from(v: SharingMode) -> Self {
        match v {
            SharingMode::Private => Self::Private,
            SharingMode::Tenant => Self::Tenant,
            SharingMode::Shared => Self::Shared,
        }
    }
}

impl From<DbSharing> for SharingMode {
    fn from(v: DbSharing) -> Self {
        match v {
            DbSharing::Private => Self::Private,
            DbSharing::Tenant => Self::Tenant,
            DbSharing::Shared => Self::Shared,
        }
    }
}

impl From<SecretStatus> for DbStatus {
    fn from(v: SecretStatus) -> Self {
        match v {
            SecretStatus::Active => Self::Active,
            SecretStatus::Declared => Self::Declared,
        }
    }
}

impl From<DbStatus> for SecretStatus {
    fn from(v: DbStatus) -> Self {
        match v {
            DbStatus::Active => Self::Active,
            DbStatus::Declared => Self::Declared,
        }
    }
}

impl From<Fallback> for DbFallback {
    fn from(v: Fallback) -> Self {
        match v {
            Fallback::Inherit => Self::Inherit,
            Fallback::None => Self::None,
        }
    }
}

impl From<DbFallback> for Fallback {
    fn from(v: DbFallback) -> Self {
        match v {
            DbFallback::Inherit => Self::Inherit,
            DbFallback::None => Self::None,
        }
    }
}

impl From<&CleanupTask> for DbCleanupOp {
    fn from(task: &CleanupTask) -> Self {
        match task {
            CleanupTask::Purge(_) => Self::Purge,
            CleanupTask::Destroy { .. } => Self::Destroy,
        }
    }
}

impl From<&DestroySelector> for DbDestroySelector {
    /// The kind of the selector; its version is stored in the `version` column.
    fn from(selector: &DestroySelector) -> Self {
        match selector {
            DestroySelector::Below(_) => Self::Below,
            DestroySelector::Exactly(_) => Self::Exactly,
        }
    }
}

impl DbDestroySelector {
    /// Rebuilds the SDK selector from its stored kind and `version` column.
    #[must_use]
    pub fn with_version(self, version: ValueVersion) -> DestroySelector {
        match self {
            Self::Below => DestroySelector::Below(version),
            Self::Exactly => DestroySelector::Exactly(version),
        }
    }
}

#[cfg(test)]
#[path = "enums_tests.rs"]
mod enums_tests;

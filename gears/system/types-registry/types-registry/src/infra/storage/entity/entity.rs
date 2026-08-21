//! `types_registry__entity` — one logical entity per admitted managed GTS
//! Identifier.
//!
//! Mirror of the table in `docs/database.sql`. Deleted rows remain as tombstones
//! so issued Registry References stay reverse-resolvable until purge (ADR-0008,
//! ADR-0013).
//!
//! Two derivable columns are materialized because neither is portably indexable
//! in derived form:
//!
//! * `gts_uuid` — the `UUIDv5` Registry Reference derived from `gts_id`. Stored so
//!   reverse lookup and collision rejection work (ADR-0001). **SDK clients MUST
//!   NOT derive it locally.**
//! * `entity_kind` — follows from the trailing `~`, but suffix predicates are not
//!   portably indexable and the value drives kind-specific constraints.
//!
//! `owning_gear` is caller-declared attribution and **MUST NOT authorize**: in a
//! single-process deployment every gear shares the process workload identity, so
//! the platform cannot tell which gear is registering. It answers "who do I ask
//! about this contract", which is why `ck_tr_entity_owner` makes it NOT NULL for a
//! global entity, whose owner side is otherwise all null.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use super::enums::{EntityKind, LifecycleStatus, OwnershipScope};

// ponytail: ceiling C6 — no PDP. P0 reads and writes are authenticated but not
// authorized, deviating from `06_authn_authz_secure_orm.md`'s "every sensitive DB
// access MUST be covered by a PDP decision". `unrestricted` rather than
// `tenant_col = "owner_tenant_id"` because P0 never populates tenant scope — every
// row carries `ownership_scope = 1` and a NULL owner, so a tenant-scoped predicate
// would match nothing and `unrestricted` states that intent instead of faking a
// dimension.
//
// Upgrade path (and why the column exists already): switch to
// `#[secure(tenant_col = "owner_tenant_id", ...)]` and add the `PolicyEnforcer`
// calls once the identity-to-permission binding lands (SPEC §9 C6, §12). No DDL
// migration is needed for that step.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__entity")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub gts_uuid: Uuid,
    pub gts_id: String,
    pub entity_kind: EntityKind,
    pub family_id: i64,
    /// Copied from `version_family` for join-free visibility checks; admission
    /// verifies the copy under the family lock. A composite foreign key would
    /// not validate global rows, because `owner_tenant_id` is NULL under MATCH
    /// SIMPLE.
    pub ownership_scope: OwnershipScope,
    pub owner_tenant_id: Option<Uuid>,
    /// The `#[toolkit::gear(name = ...)]` name. Rewritten on each admission
    /// rather than write-once.
    pub owning_gear: Option<String>,
    pub lifecycle_status: LifecycleStatus,
    /// Optimistic-concurrency version, starting at 1. Reserved for writes:
    /// dependency-driven read changes move `type_schema.resolution_fingerprint`
    /// instead.
    pub resource_version: i64,
    pub deleted_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

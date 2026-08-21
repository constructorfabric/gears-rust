//! `types_registry__operation` — request identity and client-visible workflow
//! state in one row, because every accepted request creates an operation
//! (ADR-0012).
//!
//! Mirror of the table in `docs/database.sql`.
//!
//! `idempotency_scope_hash` digests `(plane, tenant_id, principal_id)`. Including
//! the principal prevents cross-subject replay; digesting also keeps the nullable
//! `tenant_id` out of the unique key, since all three backends permit multiple
//! NULLs there.
//!
//! `dry_run` is orthogonal to `kind` and is part of the request fingerprint, so
//! reusing one `Idempotency-Key` for a dry run and a commit is a fingerprint
//! mismatch rather than a replay of the dry-run result.
//!
//! Worker leases, attempts, retries and dead letters are **not** here — they live
//! in the `toolkit-db` outbox tables under the `types_registry_outbox` prefix.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use super::enums::{OperationKind, OperationStatus, Plane};

// ponytail: ceiling C6 — no PDP; and ceiling C8 — every P0 operation is
// `plane = 1` platform, but the plane is expressed by this column and the contract,
// not enforced by the transport (an in-process gear has no inbound
// platform-identity validator, and `OperationBuilder` cannot mark a route
// platform-only). `unrestricted` matches: `tenant_id` is always NULL in P0.
//
// Upgrade path: a platform listener with `PlatformIdentity`, then
// `#[secure(tenant_col = "tenant_id", ...)]` plus `PolicyEnforcer` (SPEC §9 C6 and
// C8, §12). Neither needs a schema migration.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__operation")]
#[secure(unrestricted)]
pub struct Model {
    /// Client-visible operation id. Not auto-increment: the acceptance path
    /// allocates it so the `202` can name it.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub kind: OperationKind,
    pub dry_run: bool,
    pub plane: Plane,
    pub tenant_id: Option<Uuid>,
    /// The subject of the `SecurityContext`.
    pub principal_id: Uuid,
    pub idempotency_key: String,
    pub idempotency_scope_hash: Vec<u8>,
    pub request_fingerprint: Vec<u8>,
    /// Progress only. `Completed` means every item is terminal; outcomes stay on
    /// `operation_item` and are not aggregated here.
    pub status: OperationStatus,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

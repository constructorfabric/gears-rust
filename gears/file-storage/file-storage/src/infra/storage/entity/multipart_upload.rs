//! `SeaORM` entity for the `multipart_uploads` table.
//!
//! No `tenant_id` column — tenant boundary is enforced through the parent
//! `files` row. All queries use `AccessScope::allow_all()`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A multipart upload session row.
///
/// `declared_size` and `part_size` were added by the
/// `m20260701_000002_multipart_plan_columns` migration (server-authoritative
/// multipart-coordinator feature, §6).
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "multipart_uploads")]
#[secure(no_tenant, resource_col = "upload_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub upload_id: Uuid,
    pub file_id: Uuid,
    pub version_id: Uuid,
    pub backend_upload_handle: String,
    pub state: String,
    pub declared_mime: String,
    pub mime_validated: bool,
    /// Total file size declared at initiate time (bytes). Gates complete-time
    /// actual-vs-declared check and reconstitutes the plan for resume.
    pub declared_size: i64,
    /// Server-chosen plan unit (bytes). Together with `declared_size` this
    /// lets `P2-5` (introspect) reconstitute the full parts plan without
    /// persisting every per-part planned row.
    pub part_size: i64,
    /// Whether `complete` should bind the finalized version as the file's
    /// current content itself (upload-flow redesign; set only by the merged
    /// `POST /files` create+plan path with `bind: "auto"`). `FALSE` = the
    /// pre-redesign staged behaviour (client binds manually).
    ///
    /// Added by `m20260722_000001_multipart_auto_bind`.
    #[sea_orm(default_value = false)]
    pub auto_bind: bool,
    /// Completion-lease expiry (`state = 'completing'` only) — a later
    /// `complete` takes over once this passes. Same migration as `auto_bind`.
    #[sea_orm(nullable)]
    pub lease_until: Option<OffsetDateTime>,
    /// Opaque id of the completer currently holding the lease (diagnostics +
    /// scoped lease release). Same migration as `auto_bind`.
    #[sea_orm(nullable)]
    pub lease_owner: Option<String>,
    /// Persisted JSON of the successful complete response
    /// (`domain::multipart::StoredCompleteResult`) once `state = 'completed'`.
    /// Same migration as `auto_bind`.
    #[sea_orm(nullable)]
    pub complete_result: Option<String>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

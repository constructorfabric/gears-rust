//! `OData` filter / order surface for the `sessions` listing endpoint.
//!
//! [`SessionQuery`] declares the public set of filterable / orderable
//! columns `GET /chat-engine/v1/sessions` exposes via `?$filter=` /
//! `?$orderby=` / `?$top=`. It is **never** constructed — its only role is
//! to drive the [`ODataFilterable`] derive, which expands into
//! `SessionQueryFilterField`. [`SessionODataMapper`] then projects those
//! fields onto [`session::Column`] and surfaces cursor tiebreaker values for
//! `paginate_odata`.
//!
//! Tenant / user scoping is NOT exposed here: those identifiers come from
//! the caller's JWT and are pinned in the repository's base query, never
//! accepted through the caller-controlled `$filter`.
//
// @cpt-cf-chat-engine-session-repo:p4

use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata_macros::ODataFilterable;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::infra::db::entity::session::{Column, Entity, Model};

/// `OData` filter / order column declaration for the sessions listing.
///
/// The struct is never instantiated; the `dead_code` allow keeps clippy
/// quiet on the unused fields — the derive consumes them at compile time.
#[derive(ODataFilterable)]
#[allow(dead_code)]
pub struct SessionQuery {
    /// `sessions.session_id` (primary key). Exposed as the unique cursor
    /// tiebreaker so the listing composes a total order
    /// (`created_at DESC, session_id DESC`) and never silently drops rows
    /// that share a `created_at` instant.
    #[odata(filter(kind = "Uuid"))]
    pub session_id: Uuid,
    /// `sessions.created_at` — default chronological pagination key. When
    /// the caller omits `$orderby`, the repository injects `created_at DESC`
    /// to preserve the most-recent-first posture.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
    /// `sessions.updated_at` — alternate chronological key for callers that
    /// want recently-touched ordering.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: OffsetDateTime,
    /// `sessions.lifecycle_state` (`"active"`, `"archived"`,
    /// `"soft_deleted"`, …). `hard_deleted` rows are excluded by the base
    /// query, so a caller filtering on that value sees an empty result set.
    #[odata(filter(kind = "String"))]
    pub lifecycle_state: String,
    /// `sessions.session_type_id` — filter sessions by their declared type.
    #[odata(filter(kind = "Uuid"))]
    pub session_type_id: Uuid,
    /// `sessions.client_id` — filter sessions originated by a given client.
    #[odata(filter(kind = "String"))]
    pub client_id: String,
}

/// Maps [`SessionQueryFilterField`] onto `SeaORM` columns and extracts
/// cursor values from a [`Model`] row.
pub struct SessionODataMapper;

impl FieldToColumn<SessionQueryFilterField> for SessionODataMapper {
    type Column = Column;

    fn map_field(field: SessionQueryFilterField) -> Column {
        match field {
            SessionQueryFilterField::SessionId => Column::SessionId,
            SessionQueryFilterField::CreatedAt => Column::CreatedAt,
            SessionQueryFilterField::UpdatedAt => Column::UpdatedAt,
            SessionQueryFilterField::LifecycleState => Column::LifecycleState,
            SessionQueryFilterField::SessionTypeId => Column::SessionTypeId,
            SessionQueryFilterField::ClientId => Column::ClientId,
        }
    }
}

impl ODataFieldMapping<SessionQueryFilterField> for SessionODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: SessionQueryFilterField) -> sea_orm::Value {
        match field {
            SessionQueryFilterField::SessionId => {
                sea_orm::Value::Uuid(Some(Box::new(model.session_id)))
            }
            SessionQueryFilterField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
            SessionQueryFilterField::UpdatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.updated_at)))
            }
            SessionQueryFilterField::LifecycleState => {
                sea_orm::Value::String(Some(Box::new(model.lifecycle_state.clone())))
            }
            SessionQueryFilterField::SessionTypeId => {
                sea_orm::Value::Uuid(model.session_type_id.map(Box::new))
            }
            SessionQueryFilterField::ClientId => {
                sea_orm::Value::String(model.client_id.as_ref().map(|s| Box::new(s.clone())))
            }
        }
    }
}

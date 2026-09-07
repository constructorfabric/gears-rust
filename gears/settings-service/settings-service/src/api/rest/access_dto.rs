// Created: 2026-09-07 by Constructor Tech
//! Wire shapes of the tenant access restriction surface.

use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::domain::access::{AccessReadout, Restriction, restriction_tag};

/// `PUT /settings/{key}/permissions`: the access to store.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct SetRestrictionRequest {
    /// `read_only` or `hidden`; `overridable` is expressed by DELETE.
    pub access: String,
}

/// A stored restriction row.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct RestrictionDto {
    /// The tenant restricted.
    pub tenant_id: Uuid,
    /// `read_only` or `hidden`.
    pub access: String,
    /// The administrator of a strict ancestor who recorded it.
    pub set_by: String,
    /// When it was last changed, RFC 3339.
    pub updated_at: String,
    /// The row's state tag, what a mutation of this pair must present.
    pub etag: String,
}

/// Render a stored row.
#[must_use]
pub fn render_restriction(row: &Restriction) -> RestrictionDto {
    RestrictionDto {
        tenant_id: row.tenant_id,
        access: row.access.as_str().to_owned(),
        set_by: row.set_by.clone(),
        updated_at: row
            .updated_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| row.updated_at.to_string()),
        etag: restriction_tag(Some(row)).as_str().to_owned(),
    }
}

/// The effective access and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct EffectiveAccessDto {
    /// `overridable`, `read_only` or `hidden`.
    pub access: String,
    /// The tenant whose row supplies it; absent for `overridable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplied_by: Option<Uuid>,
}

/// One tenant's access for one setting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct AccessReadDto {
    /// The setting key.
    pub key: String,
    /// The tenant asked about.
    pub tenant_id: Uuid,
    /// The pair's own row, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored: Option<RestrictionDto>,
    /// The strictest access on the tenant's chain.
    pub effective: EffectiveAccessDto,
    /// The tag a mutation of this pair must present: the stored row's, or the
    /// absent-state tag. Also sent as the `ETag` header.
    pub etag: String,
}

/// Render a readout.
#[must_use]
pub fn render_readout(readout: &AccessReadout) -> AccessReadDto {
    AccessReadDto {
        key: readout.declaration.key.clone(),
        tenant_id: readout.tenant_id,
        stored: readout.stored.as_ref().map(render_restriction),
        effective: EffectiveAccessDto {
            access: readout.effective.access.as_str().to_owned(),
            supplied_by: readout.effective.supplied_by,
        },
        etag: readout.etag.as_str().to_owned(),
    }
}

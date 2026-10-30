//! Wire shapes of the tenant access restriction surface.

use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::api::rest::setting_dto::{Reader, disclose, is_false};
use crate::domain::access::{AccessListing, AccessReadout, Restriction, restriction_tag};

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
    /// The administrator of a strict ancestor who recorded it: an
    /// administrator's identity, masked for a caller not authorized for
    /// unmasked PII.
    pub set_by: String,
    /// Whether `set_by` is withheld by the upward disclosure boundary rather
    /// than by the PII entitlement: its tenant is not on the path from the
    /// reader tenant down to the restricted tenant.
    #[serde(default, skip_serializing_if = "is_false")]
    pub set_by_withheld: bool,
    /// When it was last changed, RFC 3339.
    pub updated_at: String,
    /// The row's state tag, what a mutation of this pair must present.
    pub etag: String,
}

/// Render a stored row.
///
/// The setter is an ancestor's administrator, shown to the restricted tenant
/// among others: masked unless `may_read_pii`, as the inheritance trail masks
/// its setters and the audit history its actor, and withheld unless
/// `setter_shown` — its recorded tenant lies on the path from the reader
/// tenant down to the restricted tenant.
#[must_use]
pub fn render_restriction(
    row: &Restriction,
    may_read_pii: bool,
    setter_shown: bool,
) -> RestrictionDto {
    let (set_by, set_by_withheld) = disclose(&row.set_by, setter_shown, may_read_pii);
    RestrictionDto {
        tenant_id: row.tenant_id,
        access: row.access.as_str().to_owned(),
        set_by,
        set_by_withheld,
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
    /// The tenant whose row supplies it; absent for `overridable`, and absent
    /// when that tenant is above the reader tenant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplied_by: Option<Uuid>,
    /// Set when the supplying tenant is above the reader tenant, so it is
    /// not named: the access is inherited from above.
    #[serde(default, skip_serializing_if = "is_false")]
    pub supplied_from_above: bool,
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

/// Render a readout for `reader`, the stored row's setter masked and withheld
/// as [`render_restriction`] does it, and a supplying tenant above the reader
/// tenant collapsed.
#[must_use]
pub fn render_readout(
    readout: &AccessReadout,
    may_read_pii: bool,
    reader: Reader,
) -> AccessReadDto {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-8
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-16
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-11
    // The path from the reader tenant down to the target is the target's own
    // chain from the reader; the stored row is at the target, and the
    // supplier lies on the chain, so it is above the reader exactly when it is
    // not on that path.
    let path = reader.path_in(&readout.chain);
    let stored = readout
        .stored
        .as_ref()
        .map(|row| render_restriction(row, may_read_pii, reader.is_shown(row.set_by_tenant, path)));
    let supplier = readout.effective.supplied_by;
    let above =
        supplier.is_some_and(|tenant| reader != Reader::Platform && !path.contains(&tenant));
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-11
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-16
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-8
    AccessReadDto {
        key: readout.declaration.key.clone(),
        tenant_id: readout.tenant_id,
        stored,
        effective: EffectiveAccessDto {
            access: readout.effective.access.as_str().to_owned(),
            supplied_by: if above { None } else { supplier },
            supplied_from_above: above,
        },
        etag: readout.etag.as_str().to_owned(),
    }
}

/// Render the listing of a caller's subtree: each row's setter is shown only
/// where its recorded tenant lies on the path from the caller down to the
/// row's tenant, taken from the walk's parent links.
#[must_use]
pub fn render_listing(listing: &AccessListing, may_read_pii: bool) -> Vec<RestrictionDto> {
    // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-6
    let reader = Reader::new(listing.caller, listing.root);
    listing
        .rows
        .iter()
        .map(|row| {
            let path = listing.walk.path_from(listing.caller, row.tenant_id);
            render_restriction(row, may_read_pii, reader.is_shown(row.set_by_tenant, &path))
        })
        .collect()
    // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-6
}

#[cfg(test)]
#[path = "access_dto_tests.rs"]
mod access_dto_tests;

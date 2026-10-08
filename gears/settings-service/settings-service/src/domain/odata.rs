// Created: 2026-08-26 by Virtuozzo International GmbH
//! Query options this gear declines across every listing, the fields a
//! listing orders by, and the shape every listing answers with.

use toolkit_macros::domain_model;
use toolkit_odata::PageInfo;
use toolkit_odata::filter::{FieldKind, FilterField};

use crate::domain::error::DomainError;

/// One page of a listing, with the size of the set it was cut from.
///
/// `total_count` is what a client needs that a cursor cannot give it: how
/// many rows the walk would return in all, for a count beside a heading and
/// for a scrollbar that is the right length. It is counted under the same
/// predicate as the page — the caller's scope, the administrative-domain
/// visibility, the `hidden` exclusion and the `$filter` — and never under the
/// cursor, so every page of one walk reports the same total; a row the caller
/// may not see is absent from it as it is absent from the page.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Listing<T> {
    /// The page.
    pub items: Vec<T>,
    /// Its cursors and the page size applied.
    pub page_info: PageInfo,
    /// How many rows the whole walk holds, at the time of this read.
    pub total_count: u64,
}

impl<T> Listing<T> {
    /// The same listing with each item mapped — a domain row to its wire
    /// shape, the cursors and the total untouched.
    pub fn map_items<U>(self, f: impl FnMut(T) -> U) -> Listing<U> {
        Listing {
            items: self.items.into_iter().map(f).collect(),
            page_info: self.page_info,
            total_count: self.total_count,
        }
    }
}

/// Refuse the `OData` options no listing here implements.
///
/// `$select` is parsed by the platform but not honoured: supporting it means a
/// response whose shape varies per request, and no caller has asked for one.
/// Refusing is deliberate rather than ignoring — a caller whose projection was
/// silently dropped receives every field believing it asked for two, which is
/// the same failure the declared filter surface exists to prevent.
///
/// Shared rather than restated per resource: two listings that answered
/// differently would be a difference no caller could predict, and the message
/// names the resource so the refusal is still specific.
///
/// # Errors
/// [`DomainError::Validation`] naming the unsupported option.
pub fn reject_unsupported_options(
    query: &toolkit_odata::ODataQuery,
    resource: &str,
) -> Result<(), DomainError> {
    if query.select.is_some() {
        return Err(DomainError::Validation {
            field: "$select".to_owned(),
            code: crate::field::ODATA_UNSUPPORTED_OPTION,
            message: format!(
                "$select is not supported on {resource}; omit it to receive the full \
                 representation"
            ),
        });
    }
    Ok(())
}

/// The fields `GET /declarations` orders by: its filter fields less the two
/// that may be empty, `domain_affinity` and `owner_module`.
///
/// A page cursor carries the sort value of the last row served, and the shared
/// pagination library has no spelling for an empty one: a listing ordered on a
/// column that may be empty fails as soon as a page has a row after it. Such a
/// column still filters; it does not order.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeclarationOrderField {
    Key,
    CategoryId,
    Mode,
    Status,
}

impl FilterField for DeclarationOrderField {
    const FIELDS: &'static [Self] = &[Self::Key, Self::CategoryId, Self::Mode, Self::Status];

    fn name(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::CategoryId => "category_id",
            Self::Mode => "mode",
            Self::Status => "status",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::CategoryId => FieldKind::Uuid,
            Self::Key | Self::Mode | Self::Status => FieldKind::String,
        }
    }
}

/// The fields `GET /categories` orders by: its filter fields less the optional
/// `domain_affinity`, for the reason [`DeclarationOrderField`] gives.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CategoryOrderField {
    Key,
    Name,
}

impl FilterField for CategoryOrderField {
    const FIELDS: &'static [Self] = &[Self::Key, Self::Name];

    fn name(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Name => "name",
        }
    }

    fn kind(&self) -> FieldKind {
        FieldKind::String
    }
}

/// The fields `GET /settings` orders by. The browse pages declarations, so an
/// order is a declaration column, and one that is never empty: `needs_review`
/// belongs to value rows and orders nothing here, and the columns that may be
/// empty would break the page's cursor for the reason [`DeclarationOrderField`]
/// gives. What remains is what the browse also filters on.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingOrderField {
    Key,
    CategoryId,
}

impl FilterField for SettingOrderField {
    const FIELDS: &'static [Self] = &[Self::Key, Self::CategoryId];

    fn name(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::CategoryId => "category_id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::Key => FieldKind::String,
            Self::CategoryId => FieldKind::Uuid,
        }
    }
}

/// Refuse an `$orderby` naming a field outside `O`, the fields `resource` is
/// ordered by — before any page is read, so the refusal names the field rather
/// than surfacing later as a cursor that will not encode.
///
/// # Errors
/// [`DomainError::Validation`] on `$orderby`, naming the field and the fields
/// that do order the listing.
pub fn reject_unsortable<O: FilterField>(
    query: &toolkit_odata::ODataQuery,
    resource: &str,
) -> Result<(), DomainError> {
    let Some(refused) = query
        .order
        .0
        .iter()
        .find(|key| O::from_name(&key.field).is_none())
    else {
        return Ok(());
    };
    let offered = O::FIELDS
        .iter()
        .map(|f| format!("`{}`", f.name()))
        .collect::<Vec<_>>()
        .join(", ");
    Err(DomainError::Validation {
        field: "$orderby".to_owned(),
        code: crate::field::ODATA_UNSORTABLE_FIELD,
        message: format!(
            "{resource} are not ordered by `{}`: a field that may be empty cannot carry a \
             page cursor; order by {offered}",
            refused.field
        ),
    })
}

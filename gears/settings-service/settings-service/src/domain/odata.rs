// Created: 2026-08-26 by Virtuozzo International GmbH
//! Query options this gear declines across every listing, and the fields a
//! listing orders by.

use toolkit_odata::filter::{FieldKind, FilterField};

use crate::domain::error::DomainError;

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

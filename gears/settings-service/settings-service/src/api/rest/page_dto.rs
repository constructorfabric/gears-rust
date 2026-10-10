// Created: 2026-10-08 by Virtuozzo International GmbH
//! The envelope every listing of this gear answers with.
//!
//! The platform's shared page carries `items` and a `page_info` of cursors
//! and page size. This one is that shape with one more field in `page_info`,
//! `total_count`: how many rows the whole walk holds, which a cursor cannot
//! say and a console needs for a count beside a heading and a scrollbar of
//! the right length. A client reading the shared contract reads this one
//! unchanged; one reading the total finds it beside the cursors.

use toolkit_odata::PageInfo;

use crate::domain::odata::Listing;

/// The cursors, the page size, and the size of the set the page was cut from.
///
/// Aliased to `CountedPageInfoDto` in the `OpenAPI` schema: types-registry
/// also exposes a `PageInfoDto`, of another shape, and every gear registers
/// into the one `OpenAPI` registry of the server, where the bare ident would
/// collide in `components.schemas`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
#[schema(as = CountedPageInfoDto)]
pub struct PageInfoDto {
    /// The cursor of the next page; `null` on the last one.
    pub next_cursor: Option<String>,
    /// The cursor of the previous page; `null` on the first one.
    pub prev_cursor: Option<String>,
    /// The page size applied, which is the default when the caller named none.
    pub limit: u64,
    /// How many rows the walk holds in all, counted under the page's own
    /// predicate — the caller's scope, the visibility rules and the `$filter`,
    /// never the cursor — at the time of the read, so every page of one walk
    /// reports the same total. Absent on the one listing whose items are not
    /// the set it pages: the needs-review browse, which pages declarations
    /// and lists their flagged rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<u64>,
}

impl PageInfoDto {
    /// The shared cursors with a total beside them.
    #[must_use]
    pub fn of(info: PageInfo, total_count: Option<u64>) -> Self {
        Self {
            next_cursor: info.next_cursor,
            prev_cursor: info.prev_cursor,
            limit: info.limit,
            total_count,
        }
    }
}

/// One page of a listing, with its [`PageInfoDto`].
///
/// Not under `api_dto` and not a derived schema, deliberately: the attribute
/// implements the response marker for the bare type name, which a generic
/// has none of, and the platform's own `toolkit_odata::Page<T>` writes its
/// schema by hand for the same envelope. So this one implements `Serialize`,
/// the response marker and the schema itself, below, and tells the gear
/// lints so.
#[allow(
    unknown_lints,
    de0203_dtos_must_use_api_dto,
    de0204_dtos_must_have_toschema_derive
)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct PageDto<T> {
    /// The page.
    pub items: Vec<T>,
    /// Its cursors, page size and total.
    pub page_info: PageInfoDto,
}

impl<T> PageDto<T> {
    /// A listing counted by its repository, each row rendered by `render`.
    pub fn counted<U>(listing: Listing<U>, render: impl FnMut(U) -> T) -> Self {
        let total_count = listing.total_count;
        let listing = listing.map_items(render);
        Self {
            items: listing.items,
            page_info: PageInfoDto::of(listing.page_info, Some(total_count)),
        }
    }

    /// A page whose total is not reported, with the reason the field docs give.
    #[must_use]
    pub fn uncounted(items: Vec<T>, info: PageInfo) -> Self {
        Self {
            items,
            page_info: PageInfoDto::of(info, None),
        }
    }

    /// A listing returned whole: one page, no cursor, the total its length.
    #[must_use]
    pub fn whole(items: Vec<T>) -> Self {
        let total_count = u64::try_from(items.len()).unwrap_or(u64::MAX);
        Self {
            items,
            page_info: PageInfoDto {
                next_cursor: None,
                prev_cursor: None,
                limit: total_count.max(1),
                total_count: Some(total_count),
            },
        }
    }
}

impl<T: serde::Serialize> toolkit::api::api_dto::ResponseApiDto for PageDto<T> {}

/// The schema of `PageDto<T>`, written by hand for the reason the shared page
/// writes its own: utoipa's derive leaves the generic parameter out of the
/// schema dependencies, and a reference to `T` would dangle.
impl<T> utoipa::PartialSchema for PageDto<T>
where
    T: utoipa::ToSchema + utoipa::PartialSchema,
{
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder};

        ObjectBuilder::new()
            .property(
                "items",
                ArrayBuilder::new().items(
                    utoipa::openapi::RefOr::<utoipa::openapi::schema::Schema>::Ref(
                        utoipa::openapi::Ref::from_schema_name(T::name().to_string()),
                    ),
                ),
            )
            .required("items")
            .property(
                "page_info",
                utoipa::openapi::RefOr::<utoipa::openapi::schema::Schema>::Ref(
                    utoipa::openapi::Ref::from_schema_name(
                        <PageInfoDto as utoipa::ToSchema>::name().to_string(),
                    ),
                ),
            )
            .required("page_info")
            .into()
    }
}

impl<T> utoipa::ToSchema for PageDto<T>
where
    T: utoipa::ToSchema + utoipa::PartialSchema,
{
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(format!("CountedPage_{}", T::name()))
    }

    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        schemas.push((
            <PageInfoDto as utoipa::ToSchema>::name().to_string(),
            <PageInfoDto as utoipa::PartialSchema>::schema(),
        ));
        <PageInfoDto as utoipa::ToSchema>::schemas(schemas);
        schemas.push((
            <T as utoipa::ToSchema>::name().to_string(),
            <T as utoipa::PartialSchema>::schema(),
        ));
        <T as utoipa::ToSchema>::schemas(schemas);
    }
}

#[cfg(test)]
#[path = "page_dto_tests.rs"]
mod page_dto_tests;

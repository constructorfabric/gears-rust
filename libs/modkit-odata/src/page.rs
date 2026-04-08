use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "with-utoipa", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page_info: PageInfo,
}

/// Manual [`utoipa::PartialSchema`] impl for `Page<T>`.
///
/// utoipa's derive macro for generic structs omits the generic parameter `T`
/// from the `schemas()` dependency list, producing a dangling `$ref`.
/// This hand-written impl ensures `T`'s schema is always registered.
#[cfg(feature = "with-utoipa")]
impl<T> utoipa::PartialSchema for Page<T>
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
                        <PageInfo as utoipa::ToSchema>::name().to_string(),
                    ),
                ),
            )
            .required("page_info")
            .into()
    }
}

#[cfg(feature = "with-utoipa")]
impl<T> utoipa::ToSchema for Page<T>
where
    T: utoipa::ToSchema + utoipa::PartialSchema,
{
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(format!("Page_{}", T::name()))
    }

    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        // PageInfo (same as derive)
        schemas.push((
            <PageInfo as utoipa::ToSchema>::name().to_string(),
            <PageInfo as utoipa::PartialSchema>::schema(),
        ));
        <PageInfo as utoipa::ToSchema>::schemas(schemas);

        // T — the generic parameter (this is what the derive omits)
        schemas.push((
            <T as utoipa::ToSchema>::name().to_string(),
            <T as utoipa::PartialSchema>::schema(),
        ));
        <T as utoipa::ToSchema>::schemas(schemas);
    }
}

impl<T> Page<T> {
    /// Create a new page with items and page info
    #[must_use]
    pub fn new(items: Vec<T>, page_info: PageInfo) -> Self {
        Self { items, page_info }
    }

    /// Create an empty page with the given limit
    #[must_use]
    pub fn empty(limit: u64) -> Self {
        Self {
            items: Vec::new(),
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
        }
    }

    /// Map items while preserving `page_info` (Domain->DTO mapping convenience)
    pub fn map_items<U>(self, mut f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(&mut f).collect(),
            page_info: self.page_info,
        }
    }
}

#[cfg(all(test, feature = "with-utoipa"))]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Dummy type to verify `Page<T>::schemas()` includes `T`'s schema.
    #[derive(utoipa::ToSchema)]
    struct DummyItem {
        #[allow(dead_code)]
        pub value: String,
    }

    #[test]
    fn test_page_name_includes_generic() {
        use utoipa::ToSchema;
        let name = <Page<DummyItem> as ToSchema>::name();
        assert_eq!(name.as_ref(), "Page_DummyItem");
    }

    #[test]
    fn test_page_schemas_includes_inner_type() {
        use utoipa::ToSchema;
        let mut schemas = Vec::new();
        <Page<DummyItem> as ToSchema>::schemas(&mut schemas);

        let names: Vec<&str> = schemas.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"DummyItem"),
            "Expected DummyItem in schemas, got: {names:?}"
        );
        assert!(
            names.contains(&"PageInfo"),
            "Expected PageInfo in schemas, got: {names:?}"
        );
    }
}

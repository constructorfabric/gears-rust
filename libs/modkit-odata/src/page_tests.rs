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

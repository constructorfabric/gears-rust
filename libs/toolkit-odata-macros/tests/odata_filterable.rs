use toolkit_odata::filter::FilterField;
use toolkit_odata_macros::ODataFilterable;

#[derive(ODataFilterable)]
#[allow(dead_code)]
struct ItemQuery {
    #[odata(filter(kind = "Uuid"))]
    id: uuid::Uuid,
    #[odata(filter(kind = "String", orderable = false))]
    region: String,
    #[odata(filter(kind = "I64", orderable = true))]
    rank: i64,
}

#[derive(ODataFilterable)]
#[allow(dead_code)]
struct PlainQuery {
    #[odata(filter(kind = "String"))]
    name: String,
}

#[test]
fn orderable_false_marks_only_that_field_filter_only() {
    assert!(ItemQueryFilterField::Id.orderable());
    assert!(!ItemQueryFilterField::Region.orderable());
    assert!(ItemQueryFilterField::Rank.orderable());
    // Still filterable: the flag does not drop the field.
    assert_eq!(ItemQueryFilterField::FIELDS.len(), 3);
}

#[test]
fn fields_are_orderable_by_default() {
    assert!(PlainQueryFilterField::Name.orderable());
}

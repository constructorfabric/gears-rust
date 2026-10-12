//! Unit tests for the listing error mapping shared by `list_children`
//! and `list_descendants`.

use super::map_listing_error;
use toolkit_db::odata::sea_orm_filter::PaginateOdataTryError;

#[test]
fn map_listing_error_routes_client_faults_to_validation() {
    let err = map_listing_error(
        "list_descendants",
        PaginateOdataTryError::OData(toolkit_odata::Error::InvalidFilter("nope".to_owned())),
    );
    assert_eq!(err.code(), "validation");
    assert!(err.to_string().contains("list_descendants query rejected"));
}

#[test]
fn map_listing_error_routes_database_failures_to_internal_without_leaking() {
    let err = map_listing_error(
        "list_descendants",
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(
            "connection to server at 10.0.0.1 failed".to_owned(),
        )),
    );
    assert_eq!(err.code(), "internal");
    // `Display` of `Internal` is the fixed "internal error" — the
    // diagnostic stays out of any public envelope.
    assert_eq!(err.to_string(), "internal error");
}

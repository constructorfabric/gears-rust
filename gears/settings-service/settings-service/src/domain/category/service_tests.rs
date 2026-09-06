// Created: 2026-08-13 by Constructor Tech
//! Tests for the category service's decidable rules.
//!
//! # Why this file is small
//!
//! Every service operation takes a [`DBRunner`](toolkit_db::secure::DBRunner),
//! and `toolkit-db` exposes no public constructor for one — `Db`, `SecureConn`
//! and `DbConn` are all sealed so a raw database handle cannot escape the
//! framework. That is a deliberate security property, and its consequence is
//! that a gear cannot drive its own services from a unit test, even against a
//! stub repository.
//!
//! So the rules that can be stated without a connection are extracted and
//! pinned here. The orchestration around them — that the precondition is
//! evaluated before the orphan guard, that a refused delete never reaches the
//! repository — is exercised by the E2E suite against a real database, and is
//! recorded in the FEATURE's acceptance criteria rather than here.

use crate::domain::error::DomainError;

#[test]
fn select_is_refused_rather_than_ignored() {
    // A caller whose projection was silently dropped receives every field
    // believing it asked for two -- the same failure the declared filter
    // surface exists to prevent.
    let query = toolkit_odata::ODataQuery {
        select: Some(vec!["key".to_owned(), "name".to_owned()]),
        ..Default::default()
    };
    match crate::domain::odata::reject_unsupported_options(&query, "categories") {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "$select");
            assert_eq!(code, crate::field::ODATA_UNSUPPORTED_OPTION);
        }
        other => panic!("expected $select to be refused, got {other:?}"),
    }
}

#[test]
fn a_query_without_select_is_accepted() {
    let query = toolkit_odata::ODataQuery::default();
    assert!(crate::domain::odata::reject_unsupported_options(&query, "categories").is_ok());
}

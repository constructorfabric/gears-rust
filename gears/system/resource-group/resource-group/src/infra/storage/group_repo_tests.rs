// Created: 2026-08-28 by Constructor Tech
//! Unit tests for `map_insert_error`.
//!
//! `ScopeError::is_unique_violation`/`is_foreign_key_violation` classify
//! through a typed `sql_err()` fast path that needs a real driver-produced
//! `DbErr` (its constructors are crate-private -- see the test module on
//! `toolkit_db::secure::error`), so these build the `DbErr::Custom` fallback
//! shape instead: the same message-matching path a caller sees when an error
//! has been re-wrapped through `to_string()` before it gets here. What is
//! under test is the *mapping* from a classified violation to a
//! `DomainError`, not the classification itself -- that lives in
//! `toolkit_db::secure::error` and is exercised there.
use sea_orm::DbErr;

use super::{FK_RESOURCE_GROUP_PARENT, map_insert_error};
use crate::domain::error::DomainError;
use crate::infra::storage::FK_RG_GTS_TYPE;
use toolkit_db::secure::ScopeError;

fn fk_violation(constraint: &str) -> ScopeError {
    ScopeError::Db(DbErr::Custom(format!(
        "error returned from database: insert or update on table \"resource_group\" violates \
         foreign key constraint \"{constraint}\" on table \"resource_group\""
    )))
}

fn unique_violation() -> ScopeError {
    ScopeError::Db(DbErr::Custom(
        "error returned from database: duplicate key value violates unique constraint \
         \"resource_group_pkey\""
            .to_owned(),
    ))
}

#[test]
fn fk_resource_group_parent_maps_to_group_not_found_by_parent_id() {
    let id = uuid::Uuid::now_v7();
    let parent_id = uuid::Uuid::now_v7();
    assert_ne!(id, parent_id, "the two ids must be distinguishable below");

    let err = map_insert_error(&fk_violation(FK_RESOURCE_GROUP_PARENT), id, Some(parent_id));

    match err {
        DomainError::GroupNotFound { id: reported_id } => {
            assert_eq!(
                reported_id, parent_id,
                "must name the parent that is missing, not the group being inserted"
            );
        }
        other => panic!("expected GroupNotFound, got: {other}"),
    }
}

#[test]
fn fk_rg_gts_type_is_not_reported_as_group_not_found() {
    // `fk_rg_gts_type` fails when a concurrent `delete_type` removes the type
    // between resolution and insert -- a different resource and a different
    // cause than a missing parent, so this must not be answered with
    // `GroupNotFound`.
    let id = uuid::Uuid::now_v7();
    let parent_id = uuid::Uuid::now_v7();

    let err = map_insert_error(&fk_violation(FK_RG_GTS_TYPE), id, Some(parent_id));

    assert!(
        !matches!(err, DomainError::GroupNotFound { .. }),
        "fk_rg_gts_type must not be mistaken for a missing parent, got: {err}"
    );
    assert!(
        matches!(err, DomainError::Database(_)),
        "expected a database error, got: {err}"
    );
}

#[test]
fn fk_resource_group_parent_without_a_parent_id_stays_a_database_error() {
    // A root group has no `parent_id` to name. `fk_resource_group_parent`
    // firing anyway would be a contradiction in terms, not a case this
    // mapping should have an opinion about -- it falls back to a plain
    // database error rather than guessing.
    let id = uuid::Uuid::now_v7();

    let err = map_insert_error(&fk_violation(FK_RESOURCE_GROUP_PARENT), id, None);

    assert!(
        matches!(err, DomainError::Database(_)),
        "expected a database error, got: {err}"
    );
}

#[test]
fn unique_violation_maps_to_group_already_exists_by_inserted_id() {
    let id = uuid::Uuid::now_v7();
    let parent_id = uuid::Uuid::now_v7();

    let err = map_insert_error(&unique_violation(), id, Some(parent_id));

    match err {
        DomainError::GroupAlreadyExists { id: reported_id } => {
            assert_eq!(
                reported_id, id,
                "must name the group being inserted, not its parent"
            );
        }
        other => panic!("expected GroupAlreadyExists, got: {other}"),
    }
}

use sea_orm::DbErr;
use sea_orm::error::{ConnAcquireErr, RuntimeErr};
use toolkit_db::secure::ScopeError;

use super::db_error;
use crate::domain::error::DomainError;

fn is_unavailable(err: &DomainError) -> bool {
    matches!(err, DomainError::Unavailable { .. })
}

#[test]
fn a_pool_that_cannot_hand_out_a_connection_is_unavailable() {
    for cause in [ConnAcquireErr::Timeout, ConnAcquireErr::ConnectionClosed] {
        let err = db_error(DbErr::ConnectionAcquire(cause));
        assert!(is_unavailable(&err), "{err:?}");
    }
}

#[test]
fn a_connection_that_cannot_be_opened_is_unavailable() {
    let err = db_error(DbErr::Conn(RuntimeErr::Internal("refused".to_owned())));
    assert!(is_unavailable(&err), "{err:?}");
}

#[test]
fn the_scoped_wrapper_is_read_through() {
    let err = db_error(ScopeError::Db(DbErr::ConnectionAcquire(
        ConnAcquireErr::Timeout,
    )));
    assert!(is_unavailable(&err), "{err:?}");
}

#[test]
fn a_failed_statement_is_the_services_own_fault() {
    for cause in [
        DbErr::Query(RuntimeErr::Internal("syntax error".to_owned())),
        DbErr::Exec(RuntimeErr::Internal("violates a constraint".to_owned())),
        DbErr::Custom("anything else".to_owned()),
    ] {
        let err = db_error(cause);
        assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
    }
}

#[test]
fn a_refusal_of_the_scope_is_not_an_outage() {
    let err = db_error(ScopeError::Denied("not in scope"));
    assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
}

#[test]
fn the_wire_detail_does_not_carry_the_drivers_text() {
    let err = db_error(DbErr::Conn(RuntimeErr::Internal(
        "postgres://user:secret@db.internal:5432".to_owned(),
    )));
    let DomainError::Unavailable { detail } = err else {
        panic!("expected Unavailable");
    };
    assert!(
        !detail.contains("secret") && !detail.contains("db.internal"),
        "{detail}"
    );
}

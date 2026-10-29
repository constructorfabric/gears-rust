use super::*;

#[test]
fn db_errors_convert_and_keep_source() {
    let e = DomainError::from(DbErr::Custom("boom".into()));
    assert!(matches!(&e, DomainError::Database { message, .. } if message.contains("boom")));

    for e in [
        e,
        DomainError::from(ScopeError::Db(DbErr::Custom("boom".into()))),
        DomainError::from(DbError::Sea(DbErr::Custom("boom".into()))),
    ] {
        assert_eq!(
            db_err(&e).map(ToString::to_string),
            Some(DbErr::Custom("boom".into()).to_string())
        );
    }
}

#[test]
fn other_errors_have_no_db_err() {
    let e = DomainError::from(ScopeError::Invalid("x"));
    assert!(matches!(e, DomainError::Database { .. }));
    assert!(db_err(&e).is_none());
    assert!(db_err(&DomainError::database("x")).is_none());
    assert!(db_err(&DomainError::InternalError).is_none());
}

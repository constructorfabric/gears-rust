use sea_orm::DbErr;

use crate::domain::error::ChatEngineError;

#[test]
fn db_err_record_not_found_maps_to_not_found() {
    let err: ChatEngineError = DbErr::RecordNotFound("missing".into()).into();
    assert!(matches!(
        err,
        ChatEngineError::NotFound {
            resource: "record",
            ..
        }
    ));
}

#[test]
fn db_err_other_maps_to_internal() {
    let err: ChatEngineError = DbErr::Custom("boom".into()).into();
    assert!(matches!(err, ChatEngineError::Internal { .. }));
}

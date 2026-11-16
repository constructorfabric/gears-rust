use async_trait::async_trait;
use sea_orm::{ActiveValue, EntityTrait};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureInsertExt};
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::record_intake::{IdInsert, RecordId, RecordIdRepository};

use super::record_ids_entity::{self as entity, Entity as RecordIdEntity};
use super::scope_error::map_scope_error;

/// `SeaORM`-backed [`RecordIdRepository`]; every insert goes through the
/// secure ORM and is checked against the caller's `AccessScope`.
#[derive(Debug, Default)]
pub struct SeaOrmRecordIdRepository;

impl SeaOrmRecordIdRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RecordIdRepository for SeaOrmRecordIdRepository {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: RecordId,
    ) -> Result<IdInsert, DomainError> {
        let row = entity::ActiveModel {
            tenant_id: ActiveValue::Set(id.tenant_id),
            record_key: ActiveValue::Set(id.key.as_str().to_owned()),
            connector: ActiveValue::Set(id.connector),
            subject_id: ActiveValue::Set(id.subject_id),
            received_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };

        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-insert
        let inserted = RecordIdEntity::insert(row.clone())
            .secure()
            .scope_with_model(scope, &row)
            .map_err(map_scope_error)?
            .exec(conn)
            .await;
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-insert

        match inserted {
            Ok(_) => Ok(IdInsert::Inserted),
            // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-repeat
            Err(err) if err.is_unique_violation() => Ok(IdInsert::AlreadyThere),
            // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-repeat
            Err(err) => Err(map_scope_error(err)),
        }
    }
}

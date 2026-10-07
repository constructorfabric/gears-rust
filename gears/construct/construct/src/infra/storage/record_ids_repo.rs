use async_trait::async_trait;
use aws_lc_rs::digest::{Context, SHA256};
use sea_orm::{ActiveValue, EntityTrait, SqlErr};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError, SecureInsertExt};
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

/// The key of a record identity: the SHA-256, in hex, of its connector,
/// provenance and version, each followed by a NUL so that no two different
/// identities run together into the same input.
#[must_use]
pub fn record_key(id: &RecordId) -> String {
    let mut hash = Context::new(&SHA256);
    for part in [&id.connector, &id.provenance, &id.version] {
        hash.update(part.as_bytes());
        hash.update(&[0]);
    }
    hex::encode(hash.finish())
}

fn is_unique_violation(err: &ScopeError) -> bool {
    matches!(
        err,
        ScopeError::Db(db) if matches!(db.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
    )
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
            record_key: ActiveValue::Set(record_key(&id)),
            connector: ActiveValue::Set(id.connector),
            provenance: ActiveValue::Set(id.provenance),
            version: ActiveValue::Set(id.version),
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
            Err(err) if is_unique_violation(&err) => Ok(IdInsert::AlreadyThere),
            // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-repeat
            Err(err) => Err(map_scope_error(err)),
        }
    }
}

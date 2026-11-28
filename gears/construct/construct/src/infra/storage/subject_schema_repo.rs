use async_trait::async_trait;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, sea_query::Expr};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use toolkit_security::AccessScope;

use construct_sdk::schema_doc::SchemaDoc;

use crate::domain::error::DomainError;
use crate::domain::profile_schema::{
    SchemaLookup, SchemaStore, StoredSchema, SubjectSchemaRepository, doc_json,
};
use crate::domain::subject_settings::SubjectKey;

use super::scope_error::map_scope_error;
use super::subject_schema_entity::{self as entity, Entity as SchemaEntity};
use super::subject_settings_repo::coverage;

/// `SeaORM`-backed [`SubjectSchemaRepository`]. Every query goes through the secure ORM under the caller's scope,
/// and a scope that does not cover the key is refused before the row is touched, as for the subject's settings.
#[derive(Debug, Default)]
pub struct SeaOrmSubjectSchemaRepository;

impl SeaOrmSubjectSchemaRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn of_key(key: SubjectKey) -> Condition {
    Condition::all()
        .add(entity::Column::TenantId.eq(key.tenant_id))
        .add(entity::Column::SubjectId.eq(key.subject_id))
}

#[async_trait]
impl SubjectSchemaRepository for SeaOrmSubjectSchemaRepository {
    #[allow(
        clippy::too_many_arguments,
        reason = "the key, the version it expects, the document, and the two counts of the compare-and-set"
    )]
    async fn reserve_names<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        current: &SchemaDoc,
        from: i64,
        to: i64,
    ) -> Result<SchemaStore, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SchemaStore::OutOfScope(refusal));
        }
        if let Some(version) = expected {
            let updated = SchemaEntity::update_many()
                .secure()
                .scope_with(scope)
                .filter(
                    of_key(key)
                        .add(entity::Column::Version.eq(version))
                        .add(entity::Column::NamesCharged.eq(from)),
                )
                .col_expr(entity::Column::NamesCharged, Expr::value(to))
                .exec(conn)
                .await
                .map_err(map_scope_error)?;
            return Ok(if updated.rows_affected > 0 {
                SchemaStore::Stored(version)
            } else {
                SchemaStore::Conflict
            });
        }
        self.insert_row(conn, scope, key, doc_json(current)?, to, 0)
            .await
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<SchemaLookup, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SchemaLookup::OutOfScope(refusal));
        }
        let row = SchemaEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_key(key))
            .one(conn)
            .await
            .map_err(map_scope_error)?;
        let Some(row) = row else {
            return Ok(SchemaLookup::Absent);
        };
        let doc: SchemaDoc = serde_json::from_str(&row.schema_json).map_err(|err| {
            DomainError::internal(format!(
                "the stored schema of subject {} in tenant {} is not a schema: {err}",
                key.subject_id, key.tenant_id
            ))
        })?;
        Ok(SchemaLookup::Stored(StoredSchema {
            doc,
            version: row.version,
            names_charged: row.names_charged,
        }))
    }

    async fn store<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        expected: Option<i64>,
        doc: &SchemaDoc,
        charged: i64,
    ) -> Result<SchemaStore, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SchemaStore::OutOfScope(refusal));
        }
        let json = doc_json(doc)?;
        if let Some(version) = expected {
            // The charge is part of the condition and is not written: only `reserve_names` moves it, so a change
            // that read the count before another was charged finds it moved and conflicts, rather than writing a
            // stale count over the charge.
            let updated = SchemaEntity::update_many()
                .secure()
                .scope_with(scope)
                .filter(
                    of_key(key)
                        .add(entity::Column::Version.eq(version))
                        .add(entity::Column::NamesCharged.eq(charged)),
                )
                .col_expr(entity::Column::SchemaJson, Expr::value(json))
                .col_expr(entity::Column::Version, Expr::value(version + 1))
                .exec(conn)
                .await
                .map_err(map_scope_error)?;
            return Ok(if updated.rows_affected > 0 {
                SchemaStore::Stored(version + 1)
            } else {
                SchemaStore::Conflict
            });
        }
        self.insert_row(conn, scope, key, json, charged, 1).await
    }
}

impl SeaOrmSubjectSchemaRepository {
    /// Inserts the first row of a person's. The first insert races with another: the primary key admits one, and
    /// the loser finds the other's row and reports a conflict; a failure with no row behind it is a defect.
    async fn insert_row<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        json: String,
        names_charged: i64,
        version: i64,
    ) -> Result<SchemaStore, DomainError> {
        let row = entity::ActiveModel {
            tenant_id: ActiveValue::Set(key.tenant_id),
            subject_id: ActiveValue::Set(key.subject_id),
            schema_json: ActiveValue::Set(json),
            names_charged: ActiveValue::Set(names_charged),
            version: ActiveValue::Set(version),
        };
        let inserted = SchemaEntity::insert(row.clone())
            .secure()
            .scope_with_model(scope, &row)
            .map_err(map_scope_error)?
            .exec(conn)
            .await;
        match inserted {
            Ok(_) => Ok(SchemaStore::Stored(version)),
            Err(err) => match self.find(conn, scope, key).await {
                Ok(SchemaLookup::Stored(_)) => Ok(SchemaStore::Conflict),
                _ => Err(map_scope_error(err)),
            },
        }
    }
}

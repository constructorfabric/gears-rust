use async_trait::async_trait;
use construct_sdk::models::FoundationNote;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use toolkit_db::DbError;
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::NoteRepository;

use super::entity::{self, Entity as NoteEntity};

pub struct SeaOrmNoteRepository;

impl SeaOrmNoteRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmNoteRepository {
    fn default() -> Self {
        Self::new()
    }
}

/// Map scope errors to domain errors.
fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::Database(DbError::Sea(e)),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
        // `ScopeError` is `#[non_exhaustive]`.
        other => DomainError::internal(format!("scope invalid: {other}")),
    }
}

#[async_trait]
impl NoteRepository for SeaOrmNoteRepository {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        note: FoundationNote,
    ) -> Result<FoundationNote, DomainError> {
        let row = || entity::ActiveModel {
            id: ActiveValue::Set(note.id),
            tenant_id: ActiveValue::Set(note.tenant_id),
            text: ActiveValue::Set(note.text.clone()),
        };

        NoteEntity::insert(row())
            .secure()
            .scope_with_model(scope, &row())
            .map_err(map_scope_error)?
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(note)
    }

    async fn find_by_id<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<FoundationNote>, DomainError> {
        let row = NoteEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(entity::Column::Id.eq(id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(row.map(Into::into))
    }
}

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::subject_settings::{SubjectSettings, SubjectSettingsRepository};

use super::scope_error::map_scope_error;
use super::subject_settings_entity::{self as entity, Entity as SubjectSettingsEntity};

/// `SeaORM`-backed [`SubjectSettingsRepository`]; every query goes through the
/// secure ORM and is constrained by the caller's `AccessScope`.
#[derive(Debug, Default)]
pub struct SeaOrmSubjectSettingsRepository;

impl SeaOrmSubjectSettingsRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SubjectSettingsRepository for SeaOrmSubjectSettingsRepository {
    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        subject_id: Uuid,
    ) -> Result<Option<SubjectSettings>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select
        let row = SubjectSettingsEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(entity::Column::TenantId.eq(tenant_id))
                    .add(entity::Column::SubjectId.eq(subject_id)),
            )
            .one(conn)
            .await
            .map_err(map_scope_error)?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select

        Ok(row.map(Into::into))
    }
}

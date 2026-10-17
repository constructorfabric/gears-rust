use async_trait::async_trait;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureInsertExt, SecureOnConflict};
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::subject_settings::{SubjectKey, SubjectSettings, SubjectSettingsRepository};

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
        key: SubjectKey,
    ) -> Result<Option<SubjectSettings>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select
        let row = SubjectSettingsEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(entity::Column::TenantId.eq(key.tenant_id))
                    .add(entity::Column::SubjectId.eq(key.subject_id)),
            )
            .one(conn)
            .await
            .map_err(map_scope_error)?;
        // @cpt-end:cpt-cf-construct-algo-subject-settings-read:p1:inst-read-select

        Ok(row.map(Into::into))
    }

    async fn set_personalization<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        personalization_enabled: bool,
    ) -> Result<(), DomainError> {
        // The erasure flag is left out of the insert, so a new row takes the
        // column default (no erasure under way), and out of the update, so an
        // existing row keeps it.
        let row = entity::ActiveModel {
            tenant_id: ActiveValue::Set(key.tenant_id),
            subject_id: ActiveValue::Set(key.subject_id),
            personalization_enabled: ActiveValue::Set(personalization_enabled),
            erasure_in_progress: ActiveValue::NotSet,
        };
        let on_conflict = SecureOnConflict::<SubjectSettingsEntity>::columns([
            entity::Column::TenantId,
            entity::Column::SubjectId,
        ])
        .update_columns([entity::Column::PersonalizationEnabled])
        .map_err(map_scope_error)?;

        // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
        SubjectSettingsEntity::insert(row.clone())
            .secure()
            .scope_with_model(scope, &row)
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-store
        Ok(())
    }
}

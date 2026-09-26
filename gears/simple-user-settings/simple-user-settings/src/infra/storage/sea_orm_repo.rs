use async_trait::async_trait;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, Order};
use simple_user_settings_sdk::models::{NamedSetting, SimpleUserSettings, SimpleUserSettingsPatch};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::SettingsRepository;

use super::entity::{self, Entity as SettingsEntity};
use super::named_entity::{self, Entity as NamedEntity};

/// A stored row back into a setting. The value was serialized by this
/// repository, so a row that does not parse is corruption, not user error.
fn named_from_row(row: named_entity::Model) -> Result<NamedSetting, DomainError> {
    let value = serde_json::from_str(&row.value).map_err(|e| {
        DomainError::internal(format!(
            "named setting '{}' holds invalid JSON: {e}",
            row.key
        ))
    })?;
    Ok(NamedSetting {
        key: row.key,
        value,
    })
}

/// The caller's own rows. The PDP scope says what the caller may touch, and a
/// tenant-wide or multi-tenant grant is a legitimate answer; whose rows these
/// are, and in which tenant, is always pinned here.
fn owned_by(tenant_id: Uuid, user_id: Uuid) -> Condition {
    Condition::all()
        .add(named_entity::Column::TenantId.eq(tenant_id))
        .add(named_entity::Column::UserId.eq(user_id))
}

fn owned_key(tenant_id: Uuid, user_id: Uuid, key: &str) -> Condition {
    owned_by(tenant_id, user_id).add(named_entity::Column::Key.eq(key))
}

pub struct SeaOrmSettingsRepository;

impl SeaOrmSettingsRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmSettingsRepository {
    fn default() -> Self {
        Self::new()
    }
}

/// Map scope errors to domain errors.
fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::internal(format!("database error: {e}")),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
        // `ScopeError` is `#[non_exhaustive]`: variants this gear has no
        // specific answer for (today the graph-query refusals, which it can
        // never trigger) map to an internal error, like `Invalid`.
        other => DomainError::internal(format!("scope invalid: {other}")),
    }
}

#[async_trait]
impl SettingsRepository for SeaOrmSettingsRepository {
    async fn find_by_user<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
    ) -> Result<Option<SimpleUserSettings>, DomainError> {
        let result = SettingsEntity::find()
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(result.map(Into::into))
    }

    async fn upsert_full<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        theme: Option<String>,
        language: Option<String>,
    ) -> Result<SimpleUserSettings, DomainError> {
        let active_model = entity::ActiveModel {
            tenant_id: ActiveValue::Set(tenant_id),
            user_id: ActiveValue::Set(user_id),
            theme: ActiveValue::Set(theme.clone()),
            language: ActiveValue::Set(language.clone()),
        };

        // Full replacement - overwrites all columns (SecureOnConflict validates tenant immutability)
        let on_conflict = SecureOnConflict::<SettingsEntity>::columns([
            entity::Column::TenantId,
            entity::Column::UserId,
        ])
        .update_columns([entity::Column::Theme, entity::Column::Language])
        .map_err(map_scope_error)?;

        SettingsEntity::insert(active_model)
            .secure()
            .scope_with_model(
                scope,
                &entity::ActiveModel {
                    tenant_id: ActiveValue::Set(tenant_id),
                    user_id: ActiveValue::Set(user_id),
                    theme: ActiveValue::Set(theme.clone()),
                    language: ActiveValue::Set(language.clone()),
                },
            )
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(SimpleUserSettings {
            user_id,
            tenant_id,
            theme,
            language,
        })
    }

    async fn upsert_patch<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        patch: SimpleUserSettingsPatch,
    ) -> Result<SimpleUserSettings, DomainError> {
        // Read existing settings to merge with patch
        // This approach is database-agnostic and avoids SQLite COALESCE type issues
        let existing = SettingsEntity::find()
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        // Merge patch with existing values
        let (theme, language) = match existing {
            Some(e) => {
                let theme = patch.theme.or(e.theme);
                let language = patch.language.or(e.language);
                (theme, language)
            }
            None => {
                // No existing record - use patch values directly
                (patch.theme, patch.language)
            }
        };

        // Use upsert_full with merged values
        self.upsert_full(conn, scope, user_id, tenant_id, theme, language)
            .await
    }

    async fn list_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<NamedSetting>, DomainError> {
        let rows = NamedEntity::find()
            .secure()
            .scope_with(scope)
            .filter(owned_by(tenant_id, user_id))
            .order_by(named_entity::Column::Key, Order::Asc)
            .all(conn)
            .await
            .map_err(map_scope_error)?;
        // One unreadable row must not take the caller's other settings down
        // with it: skip it here and say so in the log. A direct read of that
        // key (`find_named`) still reports the corruption.
        Ok(rows
            .into_iter()
            .filter_map(|row| match named_from_row(row) {
                Ok(setting) => Some(setting),
                Err(e) => {
                    tracing::error!(error = %e, "skipping unreadable named setting in list");
                    None
                }
            })
            .collect())
    }

    async fn find_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        key: &str,
    ) -> Result<Option<NamedSetting>, DomainError> {
        NamedEntity::find()
            .secure()
            .scope_with(scope)
            .filter(owned_key(tenant_id, user_id, key))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(named_from_row)
            .transpose()
    }

    async fn count_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<u64, DomainError> {
        NamedEntity::find()
            .secure()
            .scope_with(scope)
            .filter(owned_by(tenant_id, user_id))
            .count(conn)
            .await
            .map_err(map_scope_error)
    }

    async fn upsert_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        setting: NamedSetting,
    ) -> Result<NamedSetting, DomainError> {
        let value = serde_json::to_string(&setting.value)
            .map_err(|e| DomainError::internal(format!("named setting value: {e}")))?;
        let row = || named_entity::ActiveModel {
            tenant_id: ActiveValue::Set(tenant_id),
            user_id: ActiveValue::Set(user_id),
            key: ActiveValue::Set(setting.key.clone()),
            value: ActiveValue::Set(value.clone()),
        };

        let on_conflict = SecureOnConflict::<NamedEntity>::columns([
            named_entity::Column::TenantId,
            named_entity::Column::UserId,
            named_entity::Column::Key,
        ])
        .update_columns([named_entity::Column::Value])
        .map_err(map_scope_error)?;

        NamedEntity::insert(row())
            .secure()
            .scope_with_model(scope, &row())
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(setting)
    }

    async fn delete_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        key: &str,
    ) -> Result<bool, DomainError> {
        let result = NamedEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(owned_key(tenant_id, user_id, key))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(result.rows_affected > 0)
    }

    async fn delete_all_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<u64, DomainError> {
        let result = NamedEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(owned_by(tenant_id, user_id))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(result.rows_affected)
    }
}

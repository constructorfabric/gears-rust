use async_trait::async_trait;
use simple_user_settings_sdk::models::{NamedSetting, SimpleUserSettings, SimpleUserSettingsPatch};
use toolkit::domain::DomainModel;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::error::DomainError;

#[async_trait]
pub trait SettingsRepository: Send + Sync
where
    SimpleUserSettings: DomainModel,
    SimpleUserSettingsPatch: DomainModel,
{
    async fn find_by_user<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
    ) -> Result<Option<SimpleUserSettings>, DomainError>;

    async fn upsert_full<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        theme: Option<String>,
        language: Option<String>,
    ) -> Result<SimpleUserSettings, DomainError>;

    async fn upsert_patch<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        patch: SimpleUserSettingsPatch,
    ) -> Result<SimpleUserSettings, DomainError>;

    /// Every named setting of `user_id` in scope, ordered by key.
    async fn list_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<NamedSetting>, DomainError>;

    async fn find_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        key: &str,
    ) -> Result<Option<NamedSetting>, DomainError>;

    async fn count_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<u64, DomainError>;

    async fn upsert_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        user_id: Uuid,
        tenant_id: Uuid,
        setting: NamedSetting,
    ) -> Result<NamedSetting, DomainError>;

    /// Delete one named setting of `user_id` in scope; `true` if a row was removed.
    async fn delete_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
        key: &str,
    ) -> Result<bool, DomainError>;

    /// Delete every named setting of `user_id` in scope; the number removed.
    async fn delete_all_named<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<u64, DomainError>;
}

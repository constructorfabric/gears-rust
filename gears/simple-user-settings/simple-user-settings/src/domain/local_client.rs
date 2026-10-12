use async_trait::async_trait;
use simple_user_settings_sdk::{
    NamedSetting, NamedSettingsClientV1, SimpleUserSettings, SimpleUserSettingsClientV1,
    SimpleUserSettingsPatch, SimpleUserSettingsUpdate,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::repo::SettingsRepository;
use crate::domain::service::Service;

#[domain_model]
pub struct LocalClient<R: SettingsRepository + 'static> {
    service: Arc<Service<R>>,
}

impl<R: SettingsRepository + 'static> LocalClient<R> {
    #[must_use]
    pub fn new(service: Arc<Service<R>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<R: SettingsRepository + 'static> SimpleUserSettingsClientV1 for LocalClient<R> {
    async fn get_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<SimpleUserSettings, CanonicalError> {
        self.service
            .get_settings(ctx)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_settings(
        &self,
        ctx: &SecurityContext,
        update: SimpleUserSettingsUpdate,
    ) -> Result<SimpleUserSettings, CanonicalError> {
        self.service
            .update_settings(ctx, update)
            .await
            .map_err(CanonicalError::from)
    }

    async fn patch_settings(
        &self,
        ctx: &SecurityContext,
        patch: SimpleUserSettingsPatch,
    ) -> Result<SimpleUserSettings, CanonicalError> {
        self.service
            .patch_settings(ctx, patch)
            .await
            .map_err(CanonicalError::from)
    }
}

#[async_trait]
impl<R: SettingsRepository + 'static> NamedSettingsClientV1 for LocalClient<R> {
    async fn list_named_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<NamedSetting>, CanonicalError> {
        self.service
            .list_named_settings(ctx)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<Option<NamedSetting>, CanonicalError> {
        self.service
            .get_named_setting(ctx, key)
            .await
            .map_err(CanonicalError::from)
    }

    async fn put_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
        value: serde_json::Value,
    ) -> Result<NamedSetting, CanonicalError> {
        self.service
            .put_named_setting(ctx, key, value)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_named_setting(
        &self,
        ctx: &SecurityContext,
        key: &str,
    ) -> Result<bool, CanonicalError> {
        self.service
            .delete_named_setting(ctx, key)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_all_named_settings(
        &self,
        ctx: &SecurityContext,
    ) -> Result<u64, CanonicalError> {
        self.service
            .delete_all_named_settings(ctx)
            .await
            .map_err(CanonicalError::from)
    }
}

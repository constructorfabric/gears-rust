//! [`ModelRegistryClientV1`] over the catalog service (DESIGN §3.2 Local
//! Client): delegation and one error conversion per method.

use std::sync::Arc;

use async_trait::async_trait;
use model_registry_sdk::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelEntryV1, ModelRegistryClientV1, ModelV1,
    ProviderEntryV1, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::service::Service;

#[domain_model]
pub struct LocalClient {
    service: Arc<Service>,
}

impl LocalClient {
    #[must_use]
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ModelRegistryClientV1 for LocalClient {
    async fn resolve_model(
        &self,
        ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, CanonicalError> {
        self.service
            .resolve_model(ctx, canonical_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_available_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelV1>, CanonicalError> {
        self.service
            .list_available_models(ctx, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn create_provider(
        &self,
        ctx: &SecurityContext,
        req: CreateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        self.service
            .create_provider(ctx, req)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        self.service
            .get_provider(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_providers(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ProviderEntryV1>, CanonicalError> {
        self.service
            .list_providers(ctx, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateProviderRequestV1,
    ) -> Result<ProviderEntryV1, CanonicalError> {
        self.service
            .update_provider(ctx, id, req)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_provider(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), CanonicalError> {
        self.service
            .delete_provider(ctx, id, expected_version)
            .await
            .map_err(CanonicalError::from)
    }

    async fn create_model(
        &self,
        ctx: &SecurityContext,
        req: CreateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError> {
        self.service
            .create_model(ctx, req)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ModelEntryV1, CanonicalError> {
        self.service
            .get_model(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_models(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ModelEntryV1>, CanonicalError> {
        self.service
            .list_models(ctx, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateModelRequestV1,
    ) -> Result<ModelEntryV1, CanonicalError> {
        self.service
            .update_model(ctx, id, req)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_model(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), CanonicalError> {
        self.service
            .delete_model(ctx, id, expected_version)
            .await
            .map_err(CanonicalError::from)
    }
}

//! Dispatching wrappers that route file/vector store operations to
//! the correct provider-specific implementation based on `provider_id`.
//!
//! Built at init time with a map of `provider_id → impl`. The domain
//! trait signatures already carry `provider_id`, so dispatch is transparent.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::domain::ports::{
    AddFileToVectorStoreParams, FileStorageError, FileStorageProvider, UploadFileParams,
    VectorStoreProvider,
};

/// Routes `FileStorageProvider` calls to the correct provider-specific impl.
pub struct DispatchingFileStorage {
    impls: HashMap<String, Arc<dyn FileStorageProvider>>,
}

impl DispatchingFileStorage {
    #[must_use]
    pub fn new(impls: HashMap<String, Arc<dyn FileStorageProvider>>) -> Self {
        Self { impls }
    }

    fn get(&self, provider_id: &str) -> Result<&Arc<dyn FileStorageProvider>, FileStorageError> {
        self.impls
            .get(provider_id)
            .ok_or_else(|| FileStorageError::Configuration {
                message: format!(
                    "no file storage implementation registered for provider '{provider_id}'"
                ),
            })
    }
}

#[async_trait]
impl FileStorageProvider for DispatchingFileStorage {
    async fn upload_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: UploadFileParams,
    ) -> Result<(String, u64), FileStorageError> {
        self.get(provider_id)?
            .upload_file(ctx, provider_id, params)
            .await
    }

    async fn delete_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        provider_file_id: &str,
    ) -> Result<(), FileStorageError> {
        self.get(provider_id)?
            .delete_file(ctx, provider_id, provider_file_id)
            .await
    }
}

/// Routes `VectorStoreProvider` calls to the correct provider-specific impl.
pub struct DispatchingVectorStore {
    impls: HashMap<String, Arc<dyn VectorStoreProvider>>,
}

impl DispatchingVectorStore {
    #[must_use]
    pub fn new(impls: HashMap<String, Arc<dyn VectorStoreProvider>>) -> Self {
        Self { impls }
    }

    fn get(&self, provider_id: &str) -> Result<&Arc<dyn VectorStoreProvider>, FileStorageError> {
        self.impls
            .get(provider_id)
            .ok_or_else(|| FileStorageError::Configuration {
                message: format!(
                    "no vector store implementation registered for provider '{provider_id}'"
                ),
            })
    }
}

#[async_trait]
impl VectorStoreProvider for DispatchingVectorStore {
    async fn create_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
    ) -> Result<String, FileStorageError> {
        self.get(provider_id)?
            .create_vector_store(ctx, provider_id)
            .await
    }

    async fn add_file_to_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: AddFileToVectorStoreParams,
    ) -> Result<(), FileStorageError> {
        self.get(provider_id)?
            .add_file_to_vector_store(ctx, provider_id, params)
            .await
    }
}

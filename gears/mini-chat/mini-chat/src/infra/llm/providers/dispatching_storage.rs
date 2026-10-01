//! Dispatching wrappers that route file/vector store operations to
//! the correct provider-specific implementation based on `provider_id`.
//!
//! Built at init time with a map of `provider_id → impl`. The domain
//! trait signatures already carry `provider_id`, so dispatch is transparent.
//!
//! Rows persisted for cleanup (`attachments.storage_backend`,
//! `chat_vector_stores.provider`) carry the provider's storage *backend
//! label* (e.g. `azure` for the `azure_openai` provider), not its id. The
//! alias map translates such a label back to the provider id, which the
//! implementation needs to resolve the OAGW upstream.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::domain::ports::{
    AddFileToVectorStoreParams, FileStorageError, FileStorageProvider, UploadFileParams,
    VectorStoreFileStatus, VectorStoreProvider,
};

/// An implementation and the provider id to hand to it.
type Resolved<'a, T> = Result<(&'a Arc<T>, &'a str), FileStorageError>;

/// Routes `FileStorageProvider` calls to the correct provider-specific impl.
pub struct DispatchingFileStorage {
    impls: HashMap<String, Arc<dyn FileStorageProvider>>,
    /// Storage backend label → provider id.
    aliases: HashMap<String, String>,
}

impl DispatchingFileStorage {
    #[must_use]
    pub fn new(impls: HashMap<String, Arc<dyn FileStorageProvider>>) -> Self {
        Self {
            impls,
            aliases: HashMap::new(),
        }
    }

    /// Registers storage backend labels (`label → provider_id`) that persisted
    /// rows may carry instead of a provider id.
    #[must_use]
    pub fn with_aliases(mut self, aliases: HashMap<String, String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// Resolves a provider id or backend label to the implementation and the
    /// provider id to hand to it.
    fn get<'a>(&'a self, key: &'a str) -> Resolved<'a, dyn FileStorageProvider> {
        let provider_id = if self.impls.contains_key(key) {
            key
        } else {
            self.aliases.get(key).map_or(key, String::as_str)
        };
        self.impls
            .get(provider_id)
            .map(|imp| (imp, provider_id))
            .ok_or_else(|| FileStorageError::Configuration {
                message: format!("no file storage implementation registered for provider '{key}'"),
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
        let (imp, provider_id) = self.get(provider_id)?;
        imp.upload_file(ctx, provider_id, params).await
    }

    async fn delete_file(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        provider_file_id: &str,
    ) -> Result<(), FileStorageError> {
        let (imp, provider_id) = self.get(provider_id)?;
        imp.delete_file(ctx, provider_id, provider_file_id).await
    }
}

/// Routes `VectorStoreProvider` calls to the correct provider-specific impl.
pub struct DispatchingVectorStore {
    impls: HashMap<String, Arc<dyn VectorStoreProvider>>,
    /// Storage backend label → provider id.
    aliases: HashMap<String, String>,
}

impl DispatchingVectorStore {
    #[must_use]
    pub fn new(impls: HashMap<String, Arc<dyn VectorStoreProvider>>) -> Self {
        Self {
            impls,
            aliases: HashMap::new(),
        }
    }

    /// Registers storage backend labels (`label → provider_id`) that persisted
    /// rows may carry instead of a provider id.
    #[must_use]
    pub fn with_aliases(mut self, aliases: HashMap<String, String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// Resolves a provider id or backend label to the implementation and the
    /// provider id to hand to it.
    fn get<'a>(&'a self, key: &'a str) -> Resolved<'a, dyn VectorStoreProvider> {
        let provider_id = if self.impls.contains_key(key) {
            key
        } else {
            self.aliases.get(key).map_or(key, String::as_str)
        };
        self.impls
            .get(provider_id)
            .map(|imp| (imp, provider_id))
            .ok_or_else(|| FileStorageError::Configuration {
                message: format!("no vector store implementation registered for provider '{key}'"),
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
        let (imp, provider_id) = self.get(provider_id)?;
        imp.create_vector_store(ctx, provider_id).await
    }

    async fn add_file_to_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        params: AddFileToVectorStoreParams,
    ) -> Result<VectorStoreFileStatus, FileStorageError> {
        let (imp, provider_id) = self.get(provider_id)?;
        imp.add_file_to_vector_store(ctx, provider_id, params).await
    }

    async fn get_vector_store_file_status(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        vector_store_id: &str,
        provider_file_id: &str,
    ) -> Result<VectorStoreFileStatus, FileStorageError> {
        let (imp, provider_id) = self.get(provider_id)?;
        imp.get_vector_store_file_status(ctx, provider_id, vector_store_id, provider_file_id)
            .await
    }

    async fn delete_vector_store(
        &self,
        ctx: SecurityContext,
        provider_id: &str,
        vector_store_id: &str,
    ) -> Result<(), FileStorageError> {
        let (imp, provider_id) = self.get(provider_id)?;
        imp.delete_vector_store(ctx, provider_id, vector_store_id)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records the provider id each call receives.
    #[derive(Default)]
    struct RecordingStorage {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl FileStorageProvider for RecordingStorage {
        async fn upload_file(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _params: UploadFileParams,
        ) -> Result<(String, u64), FileStorageError> {
            unreachable!("not used")
        }

        async fn delete_file(
            &self,
            _ctx: SecurityContext,
            provider_id: &str,
            _provider_file_id: &str,
        ) -> Result<(), FileStorageError> {
            self.seen.lock().unwrap().push(provider_id.to_owned());
            Ok(())
        }
    }

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(uuid::Uuid::new_v4())
            .subject_tenant_id(uuid::Uuid::new_v4())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn backend_label_resolves_to_provider_id() {
        let azure = Arc::new(RecordingStorage::default());
        let dispatch = DispatchingFileStorage::new(HashMap::from([(
            "azure_openai".to_owned(),
            Arc::clone(&azure) as Arc<dyn FileStorageProvider>,
        )]))
        .with_aliases(HashMap::from([(
            "azure".to_owned(),
            "azure_openai".to_owned(),
        )]));

        // A cleanup row stores the backend label; the impl gets the provider id.
        dispatch
            .delete_file(ctx(), "azure", "file-1")
            .await
            .unwrap();
        dispatch
            .delete_file(ctx(), "azure_openai", "file-2")
            .await
            .unwrap();
        assert_eq!(
            *azure.seen.lock().unwrap(),
            ["azure_openai", "azure_openai"]
        );

        let err = dispatch
            .delete_file(ctx(), "unknown", "file-3")
            .await
            .unwrap_err();
        assert!(matches!(err, FileStorageError::Configuration { .. }));
    }

    /// Records the provider id each vector store call receives.
    #[derive(Default)]
    struct RecordingVectorStore {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl VectorStoreProvider for RecordingVectorStore {
        async fn create_vector_store(
            &self,
            _ctx: SecurityContext,
            provider_id: &str,
        ) -> Result<String, FileStorageError> {
            self.seen.lock().unwrap().push(provider_id.to_owned());
            Ok("vs-1".to_owned())
        }

        async fn add_file_to_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _params: AddFileToVectorStoreParams,
        ) -> Result<VectorStoreFileStatus, FileStorageError> {
            unreachable!("not used")
        }

        async fn get_vector_store_file_status(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _vector_store_id: &str,
            _provider_file_id: &str,
        ) -> Result<VectorStoreFileStatus, FileStorageError> {
            unreachable!("not used")
        }

        async fn delete_vector_store(
            &self,
            _ctx: SecurityContext,
            provider_id: &str,
            _vector_store_id: &str,
        ) -> Result<(), FileStorageError> {
            self.seen.lock().unwrap().push(provider_id.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn vector_store_backend_label_resolves_to_provider_id() {
        let azure = Arc::new(RecordingVectorStore::default());
        let dispatch = DispatchingVectorStore::new(HashMap::from([(
            "azure_openai".to_owned(),
            Arc::clone(&azure) as Arc<dyn VectorStoreProvider>,
        )]))
        .with_aliases(HashMap::from([(
            "azure".to_owned(),
            "azure_openai".to_owned(),
        )]));

        // `chat_vector_stores.provider` stores the backend label.
        dispatch
            .delete_vector_store(ctx(), "azure", "vs-1")
            .await
            .unwrap();
        dispatch
            .create_vector_store(ctx(), "azure_openai")
            .await
            .unwrap();
        assert_eq!(
            *azure.seen.lock().unwrap(),
            ["azure_openai", "azure_openai"]
        );

        let err = dispatch
            .delete_vector_store(ctx(), "unknown", "vs-2")
            .await
            .unwrap_err();
        assert!(matches!(err, FileStorageError::Configuration { .. }));
    }
}

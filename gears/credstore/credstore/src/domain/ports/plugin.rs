//! Storage-plugin selection boundary.
//!
//! The domain resolves a value-store implementation lazily without depending
//! on types-registry or `ClientHub` details.

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::CredStorePluginClientV1;

use crate::domain::error::DomainError;

/// Selects the active backend storage plugin (one per deployment).
#[async_trait]
pub trait PluginSelector: Send + Sync {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV1>, DomainError>;
}

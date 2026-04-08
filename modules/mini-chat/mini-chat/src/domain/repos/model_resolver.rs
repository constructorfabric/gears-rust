use async_trait::async_trait;
use mini_chat_sdk::KillSwitches;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;

/// Resolves and validates model IDs against the policy catalog.
///
/// All catalog access (resolve, list, get) is encapsulated here.
/// The implementation fetches the policy snapshot internally.
#[async_trait]
pub trait ModelResolver: Send + Sync {
    /// Resolve a model selection to a [`ResolvedModel`] with routing metadata.
    ///
    /// If `model` is `None`, returns the default model for the given `user_id`.
    /// If `model` is `Some`, validates it is non-empty and exists in the catalog.
    async fn resolve_model(
        &self,
        user_id: Uuid,
        model: Option<String>,
    ) -> Result<ResolvedModel, DomainError>;

    /// List all globally enabled models visible to the user.
    async fn list_visible_models(&self, user_id: Uuid) -> Result<Vec<ResolvedModel>, DomainError>;

    /// Get a single globally enabled model by ID.
    ///
    /// Returns `ModelNotFound` if the model does not exist or is globally disabled.
    async fn get_visible_model(
        &self,
        user_id: Uuid,
        model_id: &str,
    ) -> Result<ResolvedModel, DomainError>;

    /// Return the current kill switches from the active policy snapshot.
    async fn get_kill_switches(&self, user_id: Uuid) -> Result<KillSwitches, DomainError>;
}

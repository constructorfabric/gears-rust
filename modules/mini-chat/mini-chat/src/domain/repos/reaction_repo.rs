use std::collections::HashMap;

use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::ReactionKind;
use crate::infra::db::entity::message_reaction::Model as ReactionModel;

/// Parameters for upserting a reaction.
#[domain_model]
pub struct UpsertReactionParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub message_id: Uuid,
    pub user_id: Uuid,
    pub reaction: ReactionKind,
}

/// Repository trait for reaction persistence operations.
///
/// All methods accept:
/// - `conn: &C` where `C: DBRunner` - database runner (connection or transaction)
/// - `scope: &AccessScope` - security scope prepared by the service layer
#[async_trait]
pub trait ReactionRepository: Send + Sync {
    /// Upsert a reaction for (`message_id`, `user_id`). Returns the model.
    async fn upsert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: UpsertReactionParams,
    ) -> Result<ReactionModel, DomainError>;

    /// Batch-fetch the current user's reaction for each of the given messages.
    /// Returns at most one entry per message.
    async fn batch_by_user<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        message_ids: &[Uuid],
        user_id: Uuid,
    ) -> Result<HashMap<Uuid, ReactionKind>, DomainError>;

    /// Delete reaction for (`message_id`, `user_id`). Returns true if deleted.
    async fn delete_by_message_and_user<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<bool, DomainError>;
}

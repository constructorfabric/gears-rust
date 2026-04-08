use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use tracing::instrument;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::{Reaction, ReactionKind};
use crate::domain::repos::{
    ChatRepository, MessageRepository, ReactionRepository, UpsertReactionParams,
};
use crate::infra::db::entity::message::MessageRole;

use super::{DbProvider, actions, resources};

/// Service handling message reaction operations.
#[domain_model]
pub struct ReactionService<RR: ReactionRepository, MR: MessageRepository, CR: ChatRepository> {
    db: Arc<DbProvider>,
    reaction_repo: Arc<RR>,
    message_repo: Arc<MR>,
    chat_repo: Arc<CR>,
    enforcer: PolicyEnforcer,
}

impl<RR: ReactionRepository, MR: MessageRepository, CR: ChatRepository>
    ReactionService<RR, MR, CR>
{
    pub(crate) fn new(
        db: Arc<DbProvider>,
        reaction_repo: Arc<RR>,
        message_repo: Arc<MR>,
        chat_repo: Arc<CR>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            reaction_repo,
            message_repo,
            chat_repo,
            enforcer,
        }
    }

    /// Set or update a reaction on an assistant message.
    #[instrument(skip(self, ctx, reaction), fields(chat_id = %chat_id, msg_id = %msg_id))]
    pub async fn set_reaction(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        msg_id: Uuid,
        reaction: &str,
    ) -> Result<Reaction, DomainError> {
        tracing::debug!("Setting reaction on message");

        // Validate reaction value
        let kind = ReactionKind::parse(reaction)
            .ok_or_else(|| DomainError::validation("Reaction must be 'like' or 'dislike'"))?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::SET_REACTION, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        // Verify chat exists (scoped)
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::chat_not_found(chat_id))?;

        let msg_scope = chat_scope.tenant_only();
        let reaction_scope = chat_scope.tenant_and_owner();

        // Verify message exists in this chat and is an assistant message
        let message = self
            .message_repo
            .get_by_chat(&conn, &msg_scope, msg_id, chat_id)
            .await?
            .ok_or_else(|| DomainError::message_not_found(msg_id))?;

        if message.role != MessageRole::Assistant {
            return Err(DomainError::invalid_reaction_target(msg_id));
        }

        let params = UpsertReactionParams {
            id: Uuid::now_v7(),
            tenant_id: ctx.subject_tenant_id(),
            message_id: msg_id,
            user_id: ctx.subject_id(),
            reaction: kind,
        };

        let model = self
            .reaction_repo
            .upsert(&conn, &reaction_scope, params)
            .await?;

        let stored_kind = ReactionKind::parse(&model.reaction).ok_or_else(|| {
            DomainError::database("invalid reaction value returned from repository".to_owned())
        })?;

        tracing::debug!("Successfully set reaction");
        Ok(Reaction {
            message_id: model.message_id,
            kind: stored_kind,
            created_at: model.created_at,
        })
    }

    /// Delete a reaction from a message (idempotent).
    #[instrument(skip(self, ctx), fields(chat_id = %chat_id, msg_id = %msg_id))]
    pub async fn delete_reaction(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        msg_id: Uuid,
    ) -> Result<(), DomainError> {
        tracing::debug!("Deleting reaction from message");

        let conn = self.db.conn().map_err(DomainError::from)?;

        let chat_scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::DELETE_REACTION,
                Some(chat_id),
            )
            .await?
            .ensure_owner(ctx.subject_id());

        // Verify chat exists (scoped)
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::chat_not_found(chat_id))?;

        // Messages use `no_owner` — strip owner_id constraints to avoid
        // deny-all when the PDP returns owner-scoped predicates.
        let msg_scope = chat_scope.tenant_only();
        let reaction_scope = chat_scope.tenant_and_owner();

        // Verify message exists in this chat
        self.message_repo
            .get_by_chat(&conn, &msg_scope, msg_id, chat_id)
            .await?
            .ok_or_else(|| DomainError::message_not_found(msg_id))?;

        let user_id = ctx.subject_id();

        self.reaction_repo
            .delete_by_message_and_user(&conn, &reaction_scope, msg_id, user_id)
            .await?;

        tracing::debug!("Finished deleting reaction");
        Ok(())
    }
}

#[cfg(test)]
#[path = "reaction_service_test.rs"]
mod tests;

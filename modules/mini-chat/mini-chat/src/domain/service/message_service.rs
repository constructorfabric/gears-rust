use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use modkit_macros::domain_model;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
use tracing::instrument;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::Message;
use crate::domain::repos::{ChatRepository, MessageRepository, ReactionRepository};
use crate::infra::db::entity::message::MessageRole;

use super::{DbProvider, actions, resources};

/// Service handling message query operations.
#[domain_model]
pub struct MessageService<MR: MessageRepository, CR: ChatRepository, RR: ReactionRepository> {
    db: Arc<DbProvider>,
    message_repo: Arc<MR>,
    chat_repo: Arc<CR>,
    reaction_repo: Arc<RR>,
    enforcer: PolicyEnforcer,
}

impl<MR: MessageRepository, CR: ChatRepository, RR: ReactionRepository> MessageService<MR, CR, RR> {
    pub(crate) fn new(
        db: Arc<DbProvider>,
        message_repo: Arc<MR>,
        chat_repo: Arc<CR>,
        reaction_repo: Arc<RR>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            message_repo,
            chat_repo,
            reaction_repo,
            enforcer,
        }
    }

    /// List messages in a chat with cursor-based pagination.
    #[instrument(skip(self, ctx, query), fields(chat_id = %chat_id))]
    pub async fn list_messages(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Message>, DomainError> {
        tracing::debug!("Listing messages for chat");

        let conn = self.db.conn().map_err(DomainError::from)?;

        let chat_scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::LIST_MESSAGES, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        // Verify chat exists (scoped)
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::chat_not_found(chat_id))?;

        let msg_scope = chat_scope.tenant_only();
        let page = self
            .message_repo
            .list_by_chat(&conn, &msg_scope, chat_id, query)
            .await?;

        // Batch-fetch attachment summaries for all returned messages (single query).
        let msg_ids: Vec<Uuid> = page.items.iter().map(|m| m.id).collect();
        let mut att_map = self
            .message_repo
            .batch_attachment_summaries(&conn, &msg_scope, chat_id, &msg_ids)
            .await?;

        // Batch-fetch the current user's reactions for all returned messages.
        let reaction_scope = chat_scope.tenant_and_owner();
        let mut reaction_map = self
            .reaction_repo
            .batch_by_user(&conn, &reaction_scope, &msg_ids, ctx.subject_id())
            .await?;

        let items: Vec<Message> = page
            .items
            .into_iter()
            .map(|m| {
                // list_by_chat SQL already filters `request_id IS NOT NULL`
                let request_id = m.request_id.ok_or_else(|| {
                    DomainError::internal("list_by_chat returned message with null request_id")
                })?;
                let attachments = att_map.remove(&m.id).unwrap_or_default();
                let my_reaction = reaction_map.remove(&m.id);
                Ok(Message {
                    id: m.id,
                    request_id,
                    role: match m.role {
                        MessageRole::User => "user".to_owned(),
                        MessageRole::Assistant => "assistant".to_owned(),
                        MessageRole::System => "system".to_owned(),
                    },
                    content: m.content,
                    attachments,
                    my_reaction,
                    model: m.model,
                    input_tokens: if m.input_tokens == 0 {
                        None
                    } else {
                        Some(m.input_tokens)
                    },
                    output_tokens: if m.output_tokens == 0 {
                        None
                    } else {
                        Some(m.output_tokens)
                    },
                    created_at: m.created_at,
                })
            })
            .collect::<Result<_, DomainError>>()?;

        tracing::debug!("Successfully listed {} messages", items.len());
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }
}

#[cfg(test)]
#[path = "message_service_test.rs"]
mod tests;

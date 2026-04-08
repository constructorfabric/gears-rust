use async_trait::async_trait;
use modkit_macros::domain_model;
use tracing::info;

use mini_chat_sdk::{
    MiniChatAuditPluginClientV1, MiniChatAuditPluginError, TurnAuditEvent, TurnDeleteAuditEvent,
    TurnEditAuditEvent, TurnRetryAuditEvent,
};

/// Service for the static audit plugin.
///
/// When `enabled` is `false`, all emit methods return `Ok(())` immediately
/// without writing to the log.
#[domain_model]
pub struct Service {
    pub enabled: bool,
}

#[async_trait]
impl MiniChatAuditPluginClientV1 for Service {
    async fn emit_turn_audit(&self, event: TurnAuditEvent) -> Result<(), MiniChatAuditPluginError> {
        if !self.enabled {
            return Ok(());
        }
        info!(
            event_type = %event.event_type,
            tenant_id = %event.tenant_id,
            user_id = %event.user_id,
            chat_id = %event.chat_id,
            turn_id = %event.turn_id,
            request_id = %event.request_id,
            selected_model = %event.selected_model,
            effective_model = %event.effective_model,
            input_tokens = event.usage.input_tokens,
            output_tokens = event.usage.output_tokens,
            error_code = event.error_code.as_deref(),
            "audit: turn event"
        );
        Ok(())
    }

    async fn emit_turn_retry_audit(
        &self,
        event: TurnRetryAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        if !self.enabled {
            return Ok(());
        }
        info!(
            event_type = %event.event_type,
            tenant_id = %event.tenant_id,
            actor_user_id = %event.actor_user_id,
            chat_id = %event.chat_id,
            original_request_id = %event.original_request_id,
            new_request_id = %event.new_request_id,
            "audit: turn retry event"
        );
        Ok(())
    }

    async fn emit_turn_edit_audit(
        &self,
        event: TurnEditAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        if !self.enabled {
            return Ok(());
        }
        info!(
            event_type = %event.event_type,
            tenant_id = %event.tenant_id,
            actor_user_id = %event.actor_user_id,
            chat_id = %event.chat_id,
            original_request_id = %event.original_request_id,
            new_request_id = %event.new_request_id,
            "audit: turn edit event"
        );
        Ok(())
    }

    async fn emit_turn_delete_audit(
        &self,
        event: TurnDeleteAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError> {
        if !self.enabled {
            return Ok(());
        }
        info!(
            event_type = %event.event_type,
            tenant_id = %event.tenant_id,
            actor_user_id = %event.actor_user_id,
            chat_id = %event.chat_id,
            request_id = %event.request_id,
            "audit: turn delete event"
        );
        Ok(())
    }
}

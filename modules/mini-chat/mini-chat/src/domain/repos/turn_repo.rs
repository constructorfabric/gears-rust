use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::infra::db::entity::chat_turn::{Model as TurnModel, TurnState};

/// Parameters for creating a new turn.
#[domain_model]
pub struct CreateTurnParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub request_id: Uuid,
    pub requester_type: String,
    pub requester_user_id: Option<Uuid>,
    /// Preflight fields — NULL in P2, populated by P3 quota service.
    pub reserve_tokens: Option<i64>,
    pub max_output_tokens_applied: Option<i32>,
    pub reserved_credits_micro: Option<i64>,
    pub policy_version_applied: Option<i64>,
    pub effective_model: Option<String>,
    pub minimal_generation_floor_applied: Option<i32>,
}

/// Parameters for CAS update to completed state.
#[domain_model]
// Fields are read in infra::db::repo::turn_repo — #[domain_model] hides access from clippy.
#[allow(clippy::struct_field_names, dead_code)]
pub struct CasCompleteParams {
    pub turn_id: Uuid,
    pub assistant_message_id: Uuid,
    pub provider_response_id: Option<String>,
}

/// Parameters for CAS update to a terminal state (completed/failed/cancelled).
///
/// Unified CAS method: handles all terminal transitions. For completed turns,
/// `assistant_message_id` and `provider_response_id` are set; for failed/cancelled
/// they are `None` (content durability invariant — DESIGN.md §5.7).
#[domain_model]
pub struct CasTerminalParams {
    pub turn_id: Uuid,
    pub state: TurnState,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    /// Set only for completed turns — links to the persisted assistant message.
    pub assistant_message_id: Option<Uuid>,
    /// Provider response ID (e.g. `OpenAI` `response_id`); set for completed turns.
    pub provider_response_id: Option<String>,
}

/// Repository trait for turn persistence operations.
#[async_trait]
#[allow(dead_code)]
pub trait TurnRepository: Send + Sync {
    /// INSERT a new turn with `state = running`.
    async fn create_turn<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CreateTurnParams,
    ) -> Result<TurnModel, DomainError>;

    /// SELECT by `(chat_id, request_id)` for idempotency check.
    async fn find_by_chat_and_request_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        request_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError>;

    /// SELECT the running turn for a chat (state=running, `deleted_at` IS NULL).
    async fn find_running_by_chat_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError>;

    /// CAS state transition to a terminal state.
    /// Returns `rows_affected` (0 = another finalizer won, 1 = success).
    async fn cas_update_state<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CasTerminalParams,
    ) -> Result<u64, DomainError>;

    /// CAS transition to completed, setting `assistant_message_id` and
    /// `provider_response_id`.
    async fn cas_update_completed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: CasCompleteParams,
    ) -> Result<u64, DomainError>;

    /// Set `assistant_message_id` on a turn after the message has been persisted.
    ///
    /// Called within the finalization transaction, AFTER the assistant message
    /// INSERT and CAS guard. Separate from the CAS step because
    /// `assistant_message_id` has a FK to `messages(id)`.
    async fn set_assistant_message_id<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
        assistant_message_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Soft-delete a turn, linking to a replacement `request_id`.
    async fn soft_delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        turn_id: Uuid,
        replaced_by_request_id: Option<Uuid>,
    ) -> Result<(), DomainError>;

    /// SELECT the most recent non-deleted turn for a chat.
    async fn find_latest_turn<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError>;

    /// SELECT the most recent non-deleted turn for a chat with `FOR UPDATE` row lock.
    /// Used within mutation transactions to serialize concurrent retry/edit/delete.
    async fn find_latest_for_update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Option<TurnModel>, DomainError>;
}

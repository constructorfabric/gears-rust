//! Side-effect-free replay for completed turns.
//!
//! Structurally separated from the streaming execution path: this module
//! accepts only read-only dependencies (`DbProvider`, `MessageRepository`)
//! and cannot access `QuotaService`, outbox, provider, or finalization types.

use crate::domain::error::DomainError;
use crate::domain::llm::Usage;
use crate::domain::repos::MessageRepository;
use crate::domain::stream_events::{DeltaData, DoneData, StreamEvent, StreamStartedData};
use crate::infra::db::entity::chat_turn::Model as TurnModel;
use modkit_security::AccessScope;

use super::DbProvider;

/// Triple of SSE events produced by replay.
#[derive(Debug)]
#[allow(de0309_must_have_domain_model)]
pub struct ReplayEvents {
    pub stream_started: StreamEvent,
    pub delta: StreamEvent,
    pub done: StreamEvent,
}

/// Reconstruct SSE events from a completed turn's persisted data.
///
/// # Errors
/// - `DomainError::InternalError` if `assistant_message_id` is `None`
/// - `DomainError::InternalError` if the assistant message is not found
/// - `DomainError::Database` on connection / query failure
pub async fn replay_turn<MR: MessageRepository>(
    db: &DbProvider,
    message_repo: &MR,
    scope: &AccessScope,
    turn: &TurnModel,
    selected_model: &str,
) -> Result<ReplayEvents, DomainError> {
    let assistant_msg_id = turn.assistant_message_id.ok_or_else(|| {
        DomainError::internal(format!(
            "completed turn {} has no assistant_message_id",
            turn.id
        ))
    })?;

    let conn = db.conn().map_err(DomainError::from)?;

    let message = message_repo
        .get_by_chat(&conn, scope, assistant_msg_id, turn.chat_id)
        .await?
        .ok_or_else(|| {
            DomainError::internal(format!(
                "assistant message {} not found for turn {}",
                assistant_msg_id, turn.id
            ))
        })?;

    let stream_started = StreamEvent::StreamStarted(StreamStartedData {
        request_id: turn.request_id,
        message_id: assistant_msg_id,
        is_new_turn: false,
    });

    let delta = StreamEvent::Delta(DeltaData {
        r#type: "text",
        content: message.content,
    });

    let done = StreamEvent::Done(Box::new(DoneData {
        usage: Some(Usage {
            input_tokens: message.input_tokens,
            output_tokens: message.output_tokens,
        }),
        effective_model: turn.effective_model.clone().unwrap_or_default(),
        selected_model: selected_model.to_owned(),
        quota_decision: reconstruct_quota_decision(turn, selected_model),
        downgrade_from: reconstruct_downgrade_from(turn, selected_model),
        downgrade_reason: None,
        quota_warnings: None,
    }));

    Ok(ReplayEvents {
        stream_started,
        delta,
        done,
    })
}

fn reconstruct_quota_decision(turn: &TurnModel, selected_model: &str) -> String {
    match &turn.effective_model {
        Some(effective) if effective != selected_model => "downgrade".to_owned(),
        _ => "allow".to_owned(),
    }
}

fn reconstruct_downgrade_from(turn: &TurnModel, selected_model: &str) -> Option<String> {
    match &turn.effective_model {
        Some(effective) if effective != selected_model => Some(selected_model.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::repos::{
        InsertAssistantMessageParams, InsertUserMessageParams,
        MessageRepository as MessageRepositoryTrait,
    };
    use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};
    use crate::infra::db::entity::chat_turn::TurnState;
    use crate::infra::db::entity::message::{MessageRole, Model as MessageModel};
    use async_trait::async_trait;
    use modkit_db::secure::DBRunner;
    use modkit_security::AccessScope;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use time::OffsetDateTime;
    use uuid::Uuid;

    // ── Minimal mock MessageRepository ──────────────────────────────────

    #[allow(de0309_must_have_domain_model)]
    struct MockMessageRepo {
        messages: Mutex<HashMap<(Uuid, Uuid), MessageModel>>,
    }

    impl MockMessageRepo {
        fn new() -> Self {
            Self {
                messages: Mutex::new(HashMap::new()),
            }
        }

        fn insert(&self, msg_id: Uuid, chat_id: Uuid, model: MessageModel) {
            self.messages
                .lock()
                .unwrap()
                .insert((msg_id, chat_id), model);
        }
    }

    #[async_trait]
    impl MessageRepositoryTrait for MockMessageRepo {
        async fn insert_user_message<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: InsertUserMessageParams,
        ) -> Result<MessageModel, DomainError> {
            unimplemented!()
        }

        async fn insert_assistant_message<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: InsertAssistantMessageParams,
        ) -> Result<MessageModel, DomainError> {
            unimplemented!()
        }

        async fn find_user_message_by_request_id<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: Uuid,
        ) -> Result<Option<MessageModel>, DomainError> {
            unimplemented!()
        }

        async fn find_by_chat_and_request_id<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: Uuid,
        ) -> Result<Vec<MessageModel>, DomainError> {
            unimplemented!()
        }

        async fn get_by_chat<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            msg_id: Uuid,
            chat_id: Uuid,
        ) -> Result<Option<MessageModel>, DomainError> {
            Ok(self
                .messages
                .lock()
                .unwrap()
                .get(&(msg_id, chat_id))
                .cloned())
        }

        async fn list_by_chat<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: &modkit_odata::ODataQuery,
        ) -> Result<modkit_odata::Page<MessageModel>, DomainError> {
            unimplemented!()
        }

        async fn batch_attachment_summaries<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: &[Uuid],
        ) -> Result<HashMap<Uuid, Vec<crate::domain::models::AttachmentSummary>>, DomainError>
        {
            unimplemented!()
        }

        async fn soft_delete_by_request_id<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: Uuid,
        ) -> Result<u64, DomainError> {
            unimplemented!()
        }

        async fn snapshot_boundary<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
        ) -> Result<Option<crate::domain::repos::SnapshotBoundary>, DomainError> {
            Ok(None)
        }

        async fn recent_for_context<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: u32,
            _: Option<crate::domain::repos::SnapshotBoundary>,
        ) -> Result<Vec<MessageModel>, DomainError> {
            unimplemented!()
        }

        async fn recent_after_boundary<C: DBRunner>(
            &self,
            _: &C,
            _: &AccessScope,
            _: Uuid,
            _: time::OffsetDateTime,
            _: Uuid,
            _: u32,
            _: Option<crate::domain::repos::SnapshotBoundary>,
        ) -> Result<Vec<MessageModel>, DomainError> {
            unimplemented!()
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    fn make_completed_turn(
        assistant_message_id: Option<Uuid>,
        effective_model: Option<String>,
    ) -> TurnModel {
        TurnModel {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            chat_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            requester_type: "user".to_owned(),
            requester_user_id: Some(Uuid::new_v4()),
            state: TurnState::Completed,
            provider_name: None,
            provider_response_id: None,
            assistant_message_id,
            error_code: None,
            error_detail: None,
            reserve_tokens: Some(1000),
            max_output_tokens_applied: Some(500),
            reserved_credits_micro: Some(100),
            policy_version_applied: Some(1),
            effective_model,
            minimal_generation_floor_applied: Some(10),
            deleted_at: None,
            replaced_by_request_id: None,
            started_at: OffsetDateTime::now_utc(),
            completed_at: Some(OffsetDateTime::now_utc()),
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    fn make_message(id: Uuid, chat_id: Uuid, content: &str) -> MessageModel {
        MessageModel {
            id,
            tenant_id: Uuid::new_v4(),
            chat_id,
            request_id: Some(Uuid::new_v4()),
            role: MessageRole::Assistant,
            content: content.to_owned(),
            content_type: "text/plain".to_owned(),
            token_estimate: 10,
            provider_response_id: None,
            request_kind: None,
            features_used: serde_json::json!({}),
            input_tokens: 100,
            output_tokens: 50,
            model: Some("gpt-5.2".to_owned()),
            is_compressed: false,
            created_at: OffsetDateTime::now_utc(),
            deleted_at: None,
        }
    }

    // ── 5.1: Happy path ────────────────────────────────────────────────

    #[tokio::test]
    async fn replay_turn_happy_path() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let scope = AccessScope::allow_all();

        let msg_id = Uuid::new_v4();
        let turn = make_completed_turn(Some(msg_id), Some("gpt-5.2".to_owned()));
        let msg = make_message(msg_id, turn.chat_id, "Hello from assistant");

        let repo = MockMessageRepo::new();
        repo.insert(msg_id, turn.chat_id, msg);

        let result = replay_turn(&db, &repo, &scope, &turn, "gpt-5.2")
            .await
            .expect("replay should succeed");

        // Verify stream_started
        match &result.stream_started {
            StreamEvent::StreamStarted(d) => {
                assert_eq!(d.request_id, turn.request_id);
                assert_eq!(d.message_id, msg_id);
                assert!(!d.is_new_turn);
            }
            other => panic!("expected StreamStarted, got {other:?}"),
        }

        // Verify delta
        match &result.delta {
            StreamEvent::Delta(d) => {
                assert_eq!(d.content, "Hello from assistant");
                assert_eq!(d.r#type, "text");
            }
            other => panic!("expected Delta, got {other:?}"),
        }

        // Verify done
        match &result.done {
            StreamEvent::Done(d) => {
                assert_eq!(d.effective_model, "gpt-5.2");
                assert_eq!(d.selected_model, "gpt-5.2");
                let usage = d.usage.as_ref().expect("usage should be present");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    // ── 5.2: No downgrade ──────────────────────────────────────────────

    #[tokio::test]
    async fn replay_turn_no_downgrade() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let scope = AccessScope::allow_all();

        let msg_id = Uuid::new_v4();
        let turn = make_completed_turn(Some(msg_id), Some("gpt-5.2".to_owned()));
        let msg = make_message(msg_id, turn.chat_id, "content");

        let repo = MockMessageRepo::new();
        repo.insert(msg_id, turn.chat_id, msg);

        let result = replay_turn(&db, &repo, &scope, &turn, "gpt-5.2")
            .await
            .unwrap();

        match &result.done {
            StreamEvent::Done(d) => {
                assert_eq!(d.quota_decision, "allow");
                assert!(d.downgrade_from.is_none());
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    // ── 5.3: Downgrade detected ────────────────────────────────────────

    #[tokio::test]
    async fn replay_turn_downgrade_detected() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let scope = AccessScope::allow_all();

        let msg_id = Uuid::new_v4();
        // effective_model differs from selected_model → downgrade
        let turn = make_completed_turn(Some(msg_id), Some("gpt-5-mini".to_owned()));
        let msg = make_message(msg_id, turn.chat_id, "content");

        let repo = MockMessageRepo::new();
        repo.insert(msg_id, turn.chat_id, msg);

        let result = replay_turn(&db, &repo, &scope, &turn, "gpt-5.2")
            .await
            .unwrap();

        match &result.done {
            StreamEvent::Done(d) => {
                assert_eq!(d.quota_decision, "downgrade");
                assert_eq!(d.downgrade_from.as_deref(), Some("gpt-5.2"));
                assert_eq!(d.effective_model, "gpt-5-mini");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    // ── 5.4: Missing assistant_message_id ──────────────────────────────

    #[tokio::test]
    async fn replay_turn_missing_assistant_message_id() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let scope = AccessScope::allow_all();

        let turn = make_completed_turn(None, Some("gpt-5.2".to_owned()));
        let repo = MockMessageRepo::new();

        let err = replay_turn(&db, &repo, &scope, &turn, "gpt-5.2")
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("no assistant_message_id"), "got: {msg}");
    }

    // ── 5.5: Message not found ─────────────────────────────────────────

    #[tokio::test]
    async fn replay_turn_message_not_found() {
        let db_raw = inmem_db().await;
        let db = mock_db_provider(db_raw);
        let scope = AccessScope::allow_all();

        let msg_id = Uuid::new_v4();
        let turn = make_completed_turn(Some(msg_id), Some("gpt-5.2".to_owned()));
        // Don't insert any message → get_by_chat returns None
        let repo = MockMessageRepo::new();

        let err = replay_turn(&db, &repo, &scope, &turn, "gpt-5.2")
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
    }
}

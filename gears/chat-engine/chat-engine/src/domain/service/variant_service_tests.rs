use super::*;

use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ------------------------------------------------------------------
//  Mock VariantRepo that simulates UNIQUE violations.
// ------------------------------------------------------------------

/// Repository double that emulates the variant_index allocator's
/// race-and-retry behaviour. Configurable: each call to
/// `insert_user_and_assistant_stub_for_branch` fails up to
/// `fail_until_attempt` times before succeeding (mimics the
/// SERIALIZABLE retry loop's UNIQUE constraint contention).
struct RetryingMockRepo {
    /// Counts attempts per `(session_id, parent_message_id)` key.
    attempts: Mutex<std::collections::HashMap<(Uuid, Uuid), usize>>,
    /// Force every call to error out (simulates exhausted retries).
    always_fail: bool,
    /// On the Nth attempt (0-based) succeed; earlier attempts
    /// surface a UNIQUE-violation-like error.
    succeed_on_attempt: usize,
    /// Number of times the combined insert has been called across
    /// all keys.
    total_calls: AtomicUsize,
}

impl RetryingMockRepo {
    fn new(succeed_on_attempt: usize) -> Arc<Self> {
        Arc::new(Self {
            attempts: Mutex::new(std::collections::HashMap::new()),
            always_fail: false,
            succeed_on_attempt,
            total_calls: AtomicUsize::new(0),
        })
    }

    fn always_failing() -> Arc<Self> {
        Arc::new(Self {
            attempts: Mutex::new(std::collections::HashMap::new()),
            always_fail: true,
            succeed_on_attempt: usize::MAX,
            total_calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl VariantRepo for RetryingMockRepo {
    async fn list_siblings(
        &self,
        _session_id: Uuid,
        _parent_message_id: Option<Uuid>,
    ) -> Result<Vec<Message>> {
        Ok(Vec::new())
    }

    async fn insert_user_and_assistant_stub_for_branch(
        &self,
        session_id: Uuid,
        parent_message_id: Uuid,
        _parts: Vec<MessagePartInput>,
        _file_ids: Option<Vec<Uuid>>,
        _tenant_id: Option<String>,
        _user_id: Option<String>,
    ) -> Result<(Uuid, i32, Uuid)> {
        self.total_calls.fetch_add(1, Ordering::SeqCst);
        let mut attempts = self.attempts.lock();
        let key = (session_id, parent_message_id);
        let n = attempts.entry(key).or_insert(0);
        let attempt_idx = *n;
        *n += 1;
        drop(attempts);

        if self.always_fail {
            return Err(ChatEngineError::internal(
                "assign_variant_index exhausted 3 retries (uq_messages_session_parent_variant)",
            ));
        }
        if attempt_idx < self.succeed_on_attempt {
            return Err(ChatEngineError::internal(
                "UNIQUE constraint violated: uq_messages_session_parent_variant",
            ));
        }
        Ok((
            Uuid::new_v4(),
            i32::try_from(attempt_idx).unwrap_or(0),
            Uuid::new_v4(),
        ))
    }

    async fn ancestor_chain(&self, _session_id: Uuid, message_id: Uuid) -> Result<Vec<Uuid>> {
        Ok(vec![message_id])
    }

    async fn collect_descendants(&self, _session_id: Uuid, _message_id: Uuid) -> Result<Vec<Uuid>> {
        Ok(Vec::new())
    }

    async fn apply_active_flips(
        &self,
        _session_id: Uuid,
        _activate_ids: Vec<Uuid>,
        _deactivate_ids: Vec<Uuid>,
    ) -> Result<()> {
        Ok(())
    }

    async fn update_session_type(
        &self,
        _tenant_id: &str,
        _user_id: &str,
        _session_id: Uuid,
        _new_session_type_id: Uuid,
        _new_capabilities: JsonValue,
    ) -> Result<Session> {
        Err(ChatEngineError::internal("not implemented for this mock"))
    }
}

// ------------------------------------------------------------------
//  Race-with-retry: a helper that emulates the SERIALIZABLE retry
//  loop semantics directly against the mock repo.
// ------------------------------------------------------------------

/// Drive `insert_user_and_assistant_stub_for_branch` with up to
/// `max_attempts` retries on a UNIQUE-violation-like `Internal`
/// error, then promote a final exhaustion into a `Conflict`.
/// Mirrors the production retry semantics enforced by
/// `assign_variant_index` + the `map_unique_violation_to_conflict`
/// wrapper.
async fn insert_with_retry(
    repo: Arc<dyn VariantRepo>,
    session_id: Uuid,
    parent_message_id: Uuid,
    max_attempts: usize,
) -> Result<(Uuid, i32, Uuid)> {
    let mut last_err: Option<ChatEngineError> = None;
    for _ in 0..max_attempts {
        match repo
            .insert_user_and_assistant_stub_for_branch(
                session_id,
                parent_message_id,
                vec![MessagePartInput {
                    part_type: chat_engine_sdk::models::MessagePartType::Text,
                    content: json!({"text": "x"}),
                    file_citations: vec![],
                    link_citations: vec![],
                    references: vec![],
                }],
                None,
                None,
                None,
            )
            .await
        {
            Ok(v) => return Ok(v),
            Err(err) => {
                let is_retryable = matches!(
                    &err,
                    ChatEngineError::Internal { reason, .. }
                        if reason.to_lowercase().contains("unique")
                            || reason.to_lowercase().contains("exhausted")
                );
                if !is_retryable {
                    return Err(err);
                }
                last_err = Some(err);
            }
        }
    }
    Err(map_unique_violation_to_conflict(last_err.unwrap_or_else(
        || ChatEngineError::internal("exhausted retries with no recorded error"),
    )))
}

#[tokio::test]
async fn assign_variant_index_race_retries_then_succeeds() {
    // Succeed on the 3rd attempt (0-indexed: succeed_on_attempt=2).
    // Capped at the same 3 attempts the production helper uses.
    let repo: Arc<dyn VariantRepo> = RetryingMockRepo::new(2);
    let session_id = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let (_user_id, variant_index, _assistant_id) =
        insert_with_retry(Arc::clone(&repo), session_id, parent, 3)
            .await
            .expect("should succeed within 3 retries");
    assert_eq!(
        variant_index, 2,
        "should reflect the attempt that succeeded"
    );
}

#[tokio::test]
async fn assign_variant_index_exhausted_retries_returns_conflict() {
    let repo: Arc<dyn VariantRepo> = RetryingMockRepo::always_failing();
    let session_id = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let err = insert_with_retry(Arc::clone(&repo), session_id, parent, 3)
        .await
        .expect_err("must exhaust retries");
    assert!(
        matches!(err, ChatEngineError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );
}

#[test]
fn augment_complete_event_merges_variant_info_into_existing_metadata() {
    use crate::domain::message::StreamingCompleteEvent;

    let info = json!({
        "message_id": "00000000-0000-0000-0000-000000000001",
        "variant_index": 2,
        "total_variants": 3,
        "is_active": true,
    });
    let evt = StreamingEvent::Complete(StreamingCompleteEvent {
        message_id: Uuid::nil(),
        metadata: Some(json!({"model": "gpt-test"})),
        file_citations: vec![],
        link_citations: vec![],
        references: vec![],
    });
    let out = augment_complete_event(evt, &info);
    match out {
        StreamingEvent::Complete(c) => {
            let meta = c.metadata.expect("metadata must be present");
            assert_eq!(meta["model"], "gpt-test");
            assert_eq!(meta["variant_info"]["variant_index"], 2);
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn augment_complete_event_creates_metadata_when_absent() {
    use crate::domain::message::StreamingCompleteEvent;

    let info = json!({"variant_index": 0});
    let evt = StreamingEvent::Complete(StreamingCompleteEvent {
        message_id: Uuid::nil(),
        metadata: None,
        file_citations: vec![],
        link_citations: vec![],
        references: vec![],
    });
    let out = augment_complete_event(evt, &info);
    match out {
        StreamingEvent::Complete(c) => {
            let meta = c.metadata.expect("metadata must be created");
            assert_eq!(meta["variant_info"]["variant_index"], 0);
        }
        other => panic!("expected Complete, got {other:?}"),
    }
}

#[test]
fn map_unique_violation_to_conflict_only_converts_known_messages() {
    let benign = ChatEngineError::internal("totally unrelated db hiccup");
    match map_unique_violation_to_conflict(benign) {
        ChatEngineError::Internal { .. } => {}
        other => panic!("benign internal must stay Internal, got {other:?}"),
    }
    let exhausted = ChatEngineError::internal(
        "assign_variant_index exhausted 3 retries (uq_messages_session_parent_variant)",
    );
    match map_unique_violation_to_conflict(exhausted) {
        ChatEngineError::Conflict { .. } => {}
        other => panic!("exhausted internal must map to Conflict, got {other:?}"),
    }
}

#[test]
fn enabled_capability_names_handles_missing_and_malformed_inputs() {
    use chat_engine_sdk::models::{TenantId, UserId};
    let mut s = Session {
        session_id: Uuid::nil(),
        tenant_id: TenantId::new("t"),
        user_id: UserId::new("u"),
        client_id: None,
        session_type_id: None,
        enabled_capabilities: None,
        metadata: None,
        lifecycle_state: LifecycleState::Active,
        share_token: None,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    };
    assert!(enabled_capability_names(&s).is_empty());

    s.enabled_capabilities =
        Some(json!([{"name": "model", "value": "x"}, {"name": "stream", "value": true}]));
    let names = enabled_capability_names(&s);
    assert_eq!(names, vec!["model".to_string(), "stream".to_string()]);

    s.enabled_capabilities = Some(json!("not an array"));
    assert!(enabled_capability_names(&s).is_empty());
}

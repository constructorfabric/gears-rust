use super::*;

#[tokio::test]
async fn noop_returns_ok() {
    let emitter = NoopWebhookEmitter;
    emitter
        .emit(WebhookEvent::SessionArchived {
            session_id: Uuid::nil(),
            tenant_id: "t".into(),
            user_id: "u".into(),
        })
        .await
        .expect("noop emitter always succeeds");
}

#[test]
fn event_kind_strings_are_stable() {
    let cases = [
        (
            WebhookEvent::SessionCreated {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
                session_type_id: None,
            },
            "session.created",
        ),
        (
            WebhookEvent::SessionArchived {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
            },
            "session.archived",
        ),
        (
            WebhookEvent::SessionRestored {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
            },
            "session.restored",
        ),
        (
            WebhookEvent::SessionSoftDeleted {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
            },
            "session.soft_deleted",
        ),
        (
            WebhookEvent::SessionHardDeleted {
                session_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
            },
            "session.hard_deleted",
        ),
        (
            WebhookEvent::MessageDeleted {
                session_id: Uuid::nil(),
                message_id: Uuid::nil(),
                tenant_id: "t".into(),
                user_id: "u".into(),
                deleted_count: 1,
                deleted_at: time::OffsetDateTime::UNIX_EPOCH,
            },
            "message.deleted",
        ),
    ];
    for (evt, expected) in cases {
        assert_eq!(evt.kind(), expected);
    }
}

use super::*;

#[tokio::test]
async fn noop_webhook_emitter_satisfies_rest_trait() {
    let emitter: NoopWebhookEmitter = NoopWebhookEmitter::default();
    let via_rest: &dyn WebhookEmitter = &emitter;
    via_rest
        .emit_session_created(Uuid::nil(), "t", "u", None)
        .await
        .unwrap();
    via_rest
        .emit_session_type_health_check(Uuid::nil())
        .await
        .unwrap();
}

#[tokio::test]
async fn webhook_emitter_adapter_routes_to_domain_trait() {
    let emitter = std::sync::Arc::new(NoopWebhookEmitter::default());
    let adapter = WebhookEmitterAdapter::new(emitter);
    let via_domain: &dyn DomainWebhookEmitter = &adapter;
    via_domain
        .emit(WebhookEvent::SessionCreated {
            session_id: Uuid::nil(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_type_id: None,
        })
        .await
        .unwrap();
}

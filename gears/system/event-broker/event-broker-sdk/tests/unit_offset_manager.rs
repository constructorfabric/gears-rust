//! Unit tests for OffsetManager built-in implementations.
use event_broker_sdk::{
    ConsumerGroupId, Fallback, InMemoryOffsetManager, OffsetManager, ResolvedPosition,
};
use toolkit_security::SecurityContext;

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

fn group() -> ConsumerGroupId {
    ConsumerGroupId("g".into())
}

#[tokio::test]
async fn in_memory_position_fresh_returns_fallback() {
    let om = InMemoryOffsetManager::new(Fallback::Earliest);
    let ctx = ctx();
    let g = group();
    assert_eq!(
        om.position(&ctx, &g, "orders", 0).await.unwrap(),
        ResolvedPosition::Earliest
    );
}

#[tokio::test]
async fn in_memory_position_returns_stored_value_verbatim() {
    let om = InMemoryOffsetManager::new(Fallback::Latest);
    let ctx = ctx();
    let g = group();
    om.save_on_eb(&ctx, &g, "orders", 0, 42).await.unwrap();
    // Exact carries the last-processed offset verbatim (no +1 on the SDK side).
    assert_eq!(
        om.position(&ctx, &g, "orders", 0).await.unwrap(),
        ResolvedPosition::Exact(42)
    );
}

#[tokio::test]
async fn in_memory_save_on_eb_applies_max_semantics() {
    let om = InMemoryOffsetManager::new(Fallback::Earliest);
    let ctx = ctx();
    let g = group();
    om.save_on_eb(&ctx, &g, "orders", 1, 100).await.unwrap();
    om.save_on_eb(&ctx, &g, "orders", 1, 50).await.unwrap();
    assert_eq!(
        om.position(&ctx, &g, "orders", 1).await.unwrap(),
        ResolvedPosition::Exact(100)
    );
    om.save_on_eb(&ctx, &g, "orders", 1, 200).await.unwrap();
    assert_eq!(
        om.position(&ctx, &g, "orders", 1).await.unwrap(),
        ResolvedPosition::Exact(200)
    );
}

#[tokio::test]
async fn in_memory_overrides_apply_only_when_no_committed_cursor() {
    let om = InMemoryOffsetManager::new(Fallback::Latest)
        .with_overrides([(("orders".to_owned(), 0), 17)]);
    let ctx = ctx();
    let g = group();

    // Override applies when no committed cursor.
    assert_eq!(
        om.position(&ctx, &g, "orders", 0).await.unwrap(),
        ResolvedPosition::Exact(17)
    );

    // Committed cursor wins over override.
    om.save_on_eb(&ctx, &g, "orders", 0, 100).await.unwrap();
    assert_eq!(
        om.position(&ctx, &g, "orders", 0).await.unwrap(),
        ResolvedPosition::Exact(100)
    );
}

#[tokio::test]
async fn in_memory_override_miss_falls_back_to_fallback() {
    let om = InMemoryOffsetManager::new(Fallback::Latest)
        .with_overrides([(("orders".to_owned(), 0), 17)]);
    let ctx = ctx();
    let g = group();
    // Partition not in overrides → falls back to Fallback::Latest.
    assert_eq!(
        om.position(&ctx, &g, "orders", 9).await.unwrap(),
        ResolvedPosition::Latest
    );
}

#[tokio::test]
async fn broker_offset_manager_constructs_with_fallback() {
    use event_broker_sdk::BrokerOffsetManager;
    let om = BrokerOffsetManager::new(Fallback::Earliest);
    let ctx = ctx();
    let g = group();
    // Stub broker returns no cursor; falls through to fallback sentinel.
    assert_eq!(
        om.position(&ctx, &g, "orders", 0).await.unwrap(),
        ResolvedPosition::Earliest
    );
}

#[tokio::test]
async fn broker_offset_manager_override_takes_effect_when_no_cursor() {
    use event_broker_sdk::BrokerOffsetManager;
    let om =
        BrokerOffsetManager::new(Fallback::Latest).with_overrides([(("orders".to_owned(), 3), 50)]);
    let ctx = ctx();
    let g = group();
    assert_eq!(
        om.position(&ctx, &g, "orders", 3).await.unwrap(),
        ResolvedPosition::Exact(50)
    );
}

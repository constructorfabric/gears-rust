use super::*;
#[tokio::test]
async fn rejects_sqlite() {
    assert!(Queue::connect("sqlite::memory:", "test", 1).await.is_err());
}
#[test]
fn old_transport_messages_remain_decodable() {
    let delivery: Delivery =
        serde_json::from_str(r#"{"run_id":"old-run","generation":3}"#).unwrap();
    assert!(delivery.activity_id.is_empty());
}

#[tokio::test]
async fn enqueue_preserves_closed_pool_error_with_context() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://fixture:queue-secret@127.0.0.1/durable")
        .unwrap();
    pool.close().await;
    let mut queue = Queue::from_pool(pool, "diagnostic-test", 1);
    let error = queue
        .push_at(
            Delivery {
                run_id: uuid::Uuid::new_v4().to_string(),
                generation: 1,
                activity_id: "step".into(),
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "queue enqueue failed");
    assert!(error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<apalis_postgres::Error>(),
            Some(apalis_postgres::Error::Database(sqlx::Error::PoolClosed))
        ) || matches!(
            cause.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::PoolClosed)
        )
    }));
}

struct FailedDelivery;
#[async_trait]
impl DeliveryHandler for FailedDelivery {
    async fn execute(&self, _: Delivery, _: CancellationToken) -> anyhow::Result<()> {
        Err(anyhow::Error::new(std::io::Error::other(
            "postgresql://worker:private-password@database/journal",
        ))
        .context("checkpoint write failed"))
    }
}

#[tokio::test]
async fn delivery_preserves_sources_but_redacts_persisted_display_and_debug() {
    let error = deliver(
        Delivery {
            run_id: uuid::Uuid::new_v4().to_string(),
            generation: 7,
            activity_id: "step".into(),
        },
        Data::new(HandlerData {
            handler: Arc::new(FailedDelivery),
            shutdown: CancellationToken::new(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "durable delivery failed");
    for rendered in [
        format!("{error:?}"),
        format!("{error:#?}"),
        format!("{error:#}"),
    ] {
        assert!(!rendered.contains("private-password"));
        assert!(!rendered.contains("postgresql://"));
    }
    let mut source = error.source();
    let mut root_preserved = false;
    let mut context_preserved = false;
    while let Some(cause) = source {
        root_preserved |= cause.to_string().contains("private-password");
        context_preserved |= cause.to_string() == "checkpoint write failed";
        source = cause.source();
    }
    assert!(root_preserved);
    assert!(context_preserved);
}

use std::time::Duration;

use super::*;

struct Handler(tokio::sync::mpsc::Sender<Delivery>);

#[async_trait]
impl DeliveryHandler for Handler {
    async fn execute(&self, delivery: Delivery, _: CancellationToken) -> anyhow::Result<()> {
        self.0.send(delivery).await?;
        Ok(())
    }
}

#[tokio::test]
async fn delivery_survives_pool_restart() {
    let url = crate::test_postgres::database().await;
    let queue_name = format!("durable-test-{}", uuid::Uuid::new_v4());
    let mut queue = Queue::connect(&url, &queue_name, 1).await.unwrap();
    queue
        .push(Delivery {
            run_id: "opaque-run".into(),
            generation: 4,
            activity_id: "test".into(),
        })
        .await
        .unwrap();
    drop(queue);

    let queue = Queue::connect(&url, &queue_name, 1).await.unwrap();
    let mut producer = queue.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let stop = CancellationToken::new();
    let worker_stop = stop.clone();
    let worker =
        tokio::spawn(async move { queue.run(Arc::new(Handler(tx)), worker_stop, 1).await });
    let result = tokio::time::timeout(Duration::from_secs(15), rx.recv()).await;
    // Exercise a second enqueue after the consumer has gone idle, not
    // just the initial fetch during worker registration.
    tokio::time::sleep(Duration::from_secs(6)).await;
    producer
        .push(Delivery {
            run_id: "late-run".into(),
            generation: 5,
            activity_id: "test".into(),
        })
        .await
        .unwrap();
    let late = tokio::time::timeout(Duration::from_secs(15), rx.recv()).await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let delivery = result.unwrap().unwrap();
    assert_eq!(delivery.run_id, "opaque-run");
    assert_eq!(delivery.generation, 4);
    let delivery = late.unwrap().unwrap();
    assert_eq!(delivery.run_id, "late-run");
    assert_eq!(delivery.generation, 5);
}

#[tokio::test]
async fn scheduled_delivery_does_not_block_immediate_work() {
    let url = crate::test_postgres::database().await;
    let queue = Queue::connect(&url, &format!("scheduled-{}", uuid::Uuid::new_v4()), 1)
        .await
        .unwrap();
    let mut producer = queue.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(3);
    let stop = CancellationToken::new();
    let worker_stop = stop.clone();
    let worker =
        tokio::spawn(async move { queue.run(Arc::new(Handler(tx)), worker_stop, 1).await });
    producer
        .push(Delivery {
            run_id: "ready-probe".into(),
            generation: 0,
            activity_id: "step".into(),
        })
        .await
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.run_id, "ready-probe");
    let due = chrono::Utc::now() + chrono::Duration::seconds(20);
    producer
        .push_at(
            Delivery {
                run_id: "scheduled".into(),
                generation: 0,
                activity_id: "step".into(),
            },
            due,
        )
        .await
        .unwrap();
    producer
        .push(Delivery {
            run_id: "immediate".into(),
            generation: 0,
            activity_id: "step".into(),
        })
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let immediate_at = chrono::Utc::now();
    let second = tokio::time::timeout(Duration::from_secs(35), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let completed_at = chrono::Utc::now();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.run_id, "immediate");
    assert!(immediate_at < due);
    assert_eq!(second.run_id, "scheduled");
    assert!(completed_at >= due);
}

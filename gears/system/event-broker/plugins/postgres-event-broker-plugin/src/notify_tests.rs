use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::Partition;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use crate::notify::{HeadsToNotify, NOTIFY_BACKOFF_FIRST, Notifier};

#[tokio::test]
async fn a_failed_notify_pass_sleeps_and_retries() {
    // Nothing listens on port 1: every `begin` fails within the acquire timeout.
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://127.0.0.1:1/none")
        .expect("a lazy pool is built without connecting");
    let heads = Arc::new(HeadsToNotify::default());
    let topic = Uuid::from_u128(7);
    let partition = Partition::new(3);
    heads.offer(topic, partition, 42);
    let mut notifier = Notifier::new(Arc::clone(&heads), pool);
    let cancel = CancellationToken::new();

    let first = notifier.execute(&cancel).await;
    assert!(
        matches!(first, Ok(Directive::Sleep(d, ())) if d == NOTIFY_BACKOFF_FIRST),
        "{first:?}"
    );

    let second = notifier.execute(&cancel).await;
    assert!(
        matches!(second, Ok(Directive::Sleep(d, ())) if d == NOTIFY_BACKOFF_FIRST * 2),
        "{second:?}"
    );

    assert_eq!(
        heads.take(),
        HashMap::from([((topic, partition), 42)]),
        "a failed pass keeps its heads for the next one"
    );
}

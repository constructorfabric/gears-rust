use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{Partition, Role};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use crate::maintenance::extender::Extender;
use crate::maintenance::prioritizer::{Due, Key, Taken};
use crate::maintenance::registry::Urgency;
use crate::settings::PostgresSettings;
use crate::shared::Shared;

/// Nothing listens on port 1, so every connection is refused.
const DOWN: &str = "postgres://u:p@127.0.0.1:1/db";

fn key() -> Key {
    (Uuid::from_u128(1), Partition::new(0))
}

/// An extender over a database that is down.
fn extender() -> (Extender, Arc<Shared>) {
    let serde_json::Value::Object(map) = serde_json::json!({ "url": DOWN }) else {
        unreachable!("an object literal");
    };
    let shared = Shared::new(
        PostgresSettings::parse(&map).expect("settings"),
        Role::Ingest,
    );
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy(DOWN)
        .expect("a lazy pool");
    (Extender::new(Arc::clone(&shared), pool), shared)
}

#[tokio::test]
async fn an_empty_queue_idles() {
    let (mut extender, _) = extender();
    assert_eq!(
        extender
            .execute(&CancellationToken::new())
            .await
            .map_err(|e| e.to_string()),
        Ok(Directive::idle())
    );
}

#[tokio::test]
async fn a_database_that_is_down_backs_the_worker_off_and_keeps_the_key_uncounted() {
    let (mut extender, shared) = extender();
    let queue = &shared.maintenance.extend;
    queue.offer(key(), Urgency::Urgent);
    assert_eq!(
        extender
            .execute(&CancellationToken::new())
            .await
            .map_err(|e| e.to_string()),
        Err("extender: the database is unavailable: \
             pool timed out while waiting for an open connection"
            .to_owned())
    );
    let Taken::Lease(lease) = queue.take() else {
        panic!("expected the key back");
    };
    assert_eq!((lease.key(), lease.attempt()), (key(), 0));
}

#[tokio::test]
async fn a_stopping_worker_puts_the_key_back() {
    let (mut extender, shared) = extender();
    shared.maintenance.extend.offer(key(), Urgency::Urgent);
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(
        extender.execute(&stop).await.map_err(|e| e.to_string()),
        Ok(Directive::idle())
    );
    assert_eq!(shared.maintenance.extend.peek(), Due::Now);
}

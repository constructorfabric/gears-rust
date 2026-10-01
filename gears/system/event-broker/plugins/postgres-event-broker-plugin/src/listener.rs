//! The `LISTEN` connection (`docs/DESIGN.md` §3.1, §4.7): heads committed by
//! any instance reach this instance's watches and waiting reads.
//!
//! It runs on a connection of its own, outside the pool. After a reconnect -
//! sqlx's own (`try_recv` answering `Ok(None)`) or this task's - notifications
//! may have been lost, so it publishes every partition's current head again;
//! [`LocalHeads`](event_broker_sdk::backend::support::LocalHeads) passes on
//! only those above what a watcher already has.

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{Head, TopicPartition};
use event_broker_sdk::{GtsInstanceId, Sequence};
use sqlx::postgres::{PgListener, PgNotification};
use tokio_util::sync::CancellationToken;

use crate::shared::Shared;
use crate::{names, startup};

const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub fn spawn(shared: Arc<Shared>, stop: CancellationToken) {
    tokio::spawn(async move {
        let mut backoff = BACKOFF_FIRST;
        loop {
            match listen(&shared).await {
                Ok(mut listener) => {
                    backoff = BACKOFF_FIRST;
                    republish(&shared).await;
                    if serve(&shared, &mut listener, &stop).await {
                        return;
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "postgres event-broker LISTEN connection failed; reconnecting");
                }
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(startup::jittered(backoff)) => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    });
}

async fn listen(shared: &Shared) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect(&shared.settings.url).await?;
    listener.listen(names::HEAD_CHANNEL).await?;
    Ok(listener)
}

/// Serves notifications until the connection fails (`false`) or the backend is
/// stopped (`true`).
async fn serve(shared: &Shared, listener: &mut PgListener, stop: &CancellationToken) -> bool {
    loop {
        tokio::select! {
            () = stop.cancelled() => return true,
            received = listener.try_recv() => match received {
                Ok(Some(notification)) => publish(shared, &notification).await,
                // sqlx reconnected underneath: whatever arrived meanwhile is lost.
                Ok(None) => republish(shared).await,
                Err(err) => {
                    tracing::warn!(%err, "postgres event-broker LISTEN connection lost");
                    return false;
                }
            },
        }
    }
}

async fn publish(shared: &Shared, notification: &PgNotification) {
    let Some((topic, partition, sequence)) = names::parse_head_payload(notification.payload())
    else {
        tracing::warn!("postgres event-broker: a head notification does not parse");
        return;
    };
    match shared.lookup(topic, "watch").await {
        Ok(Some(known)) => shared.heads.publish(Head {
            at: TopicPartition::new(known.id, partition),
            sequence: Sequence::assigned(sequence),
        }),
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(%err, "postgres event-broker: a notified topic could not be resolved");
        }
    }
}

async fn republish(shared: &Shared) {
    let Ok(pool) = shared.pool() else {
        return;
    };
    let rows: Result<Vec<(String, i32, i64)>, _> = sqlx::query_as(
        "SELECT l.topic_id, p.partition, p.last_sequence FROM evbk__partition p JOIN evbk__log l USING (topic_uuid)",
    )
    .fetch_all(pool)
    .await;
    match rows {
        Ok(rows) => {
            for (topic, partition, sequence) in rows {
                if let (Ok(topic), Some(partition)) = (
                    GtsInstanceId::try_new(&topic),
                    names::partition_from_column(partition),
                ) {
                    shared.heads.publish(Head {
                        at: TopicPartition::new(topic, partition),
                        sequence: Sequence::assigned(sequence),
                    });
                }
            }
        }
        Err(err) => tracing::warn!(%err, "postgres event-broker: current heads could not be read"),
    }
}

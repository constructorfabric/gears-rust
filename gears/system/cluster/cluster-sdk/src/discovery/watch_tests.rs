// Created: 2026-06-04 by Constructor Tech
use std::collections::HashMap;
use std::time::SystemTime;

use super::{ServiceInstance, ServiceWatch, ServiceWatchEvent, TopologyChange};
use crate::discovery::types::InstanceState;
use crate::error::{ClusterError, ProviderErrorKind};

fn instance(id: &str) -> ServiceInstance {
    ServiceInstance {
        instance_id: id.to_owned(),
        address: "10.0.0.1:9000".to_owned(),
        metadata: HashMap::new(),
        state: InstanceState::Enabled,
        registered_at: SystemTime::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn delivers_events_in_order_then_ends_on_sender_drop() {
    let (tx, mut watch) = ServiceWatch::channel(8);
    assert!(
        tx.send(ServiceWatchEvent::Change(TopologyChange::Joined(instance(
            "i-1"
        ))))
        .await
        .is_ok()
    );
    assert!(
        tx.send(ServiceWatchEvent::Change(TopologyChange::Left {
            instance_id: "i-1".to_owned(),
        }))
        .await
        .is_ok()
    );
    assert!(tx.send(ServiceWatchEvent::Reset).await.is_ok());
    drop(tx);

    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Change(TopologyChange::Joined(i))) if i.instance_id == "i-1"
    ));
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Change(TopologyChange::Left { instance_id })) if instance_id == "i-1"
    ));
    assert!(matches!(watch.recv().await, Some(ServiceWatchEvent::Reset)));
    assert!(watch.recv().await.is_none());
}

#[tokio::test]
async fn lagged_and_closed_events_are_delivered_verbatim() {
    let (tx, mut watch) = ServiceWatch::channel(4);
    assert!(
        tx.send(ServiceWatchEvent::Lagged { dropped: 7 })
            .await
            .is_ok()
    );
    assert!(
        tx.send(ServiceWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            message: "bad credentials".to_owned(),
        }))
        .await
        .is_ok()
    );

    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Lagged { dropped: 7 })
    ));
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            ..
        }))
    ));
}

#[tokio::test]
async fn send_errors_after_watch_dropped() {
    let (tx, watch) = ServiceWatch::channel(1);
    drop(watch);
    assert!(tx.send(ServiceWatchEvent::Reset).await.is_err());
}

#[tokio::test]
async fn try_send_does_not_block_on_a_full_buffer() {
    // The shutdown-revocation path awaits the watch task, so its terminal
    // send must be non-blocking even against a live consumer that has
    // stopped draining — otherwise `ClusterHandle::stop` could hang.
    let (tx, _watch) = ServiceWatch::channel(1);
    assert!(tx.try_send(ServiceWatchEvent::Reset).is_ok());
    // The buffer is full and the consumer (`_watch`) is still alive, so the
    // next `try_send` reports `Full` rather than awaiting space.
    assert!(matches!(
        tx.try_send(ServiceWatchEvent::Closed(ClusterError::Shutdown)),
        Err(super::ServiceWatchTrySendError::Full)
    ));
}

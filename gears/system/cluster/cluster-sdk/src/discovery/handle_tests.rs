// Created: 2026-06-11 by Constructor Tech
use std::collections::HashMap;

use super::{ServiceHandle, ServiceRequest};
use crate::discovery::InstanceState;
use crate::error::{ClusterError, ProviderErrorKind};

#[tokio::test]
async fn update_metadata_round_trips_backend_ok() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::UpdateMetadata {
            metadata,
            responder,
        }) = rx.recv().await
        else {
            panic!("an update-metadata request must arrive");
        };
        assert_eq!(metadata.get("region").map(String::as_str), Some("us-east"));
        responder.respond(Ok(()));
    });

    let mut metadata = HashMap::new();
    metadata.insert("region".to_owned(), "us-east".to_owned());
    assert!(handle.update_metadata(metadata).await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn set_state_is_repeatable_and_round_trips_each_call() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        // Drain to disabled.
        let Some(ServiceRequest::SetState {
            state: InstanceState::Disabled,
            responder,
        }) = rx.recv().await
        else {
            panic!("a set-state(Disabled) request must arrive");
        };
        responder.respond(Ok(()));
        // Re-enable.
        let Some(ServiceRequest::SetState {
            state: InstanceState::Enabled,
            responder,
        }) = rx.recv().await
        else {
            panic!("a set-state(Enabled) request must arrive");
        };
        responder.respond(Ok(()));
    });

    assert!(handle.set_state(InstanceState::Disabled).await.is_ok());
    assert!(handle.set_state(InstanceState::Enabled).await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn set_state_propagates_backend_error() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::SetState { responder, .. }) = rx.recv().await else {
            panic!("a set-state request must arrive");
        };
        responder.respond(Err(ClusterError::Provider {
            kind: ProviderErrorKind::Timeout,
            message: "deadline exceeded".to_owned(),
        }));
    });

    assert!(matches!(
        handle.set_state(InstanceState::Disabled).await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::Timeout,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn update_metadata_after_backend_gone_surfaces_error() {
    let (rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    // Backend torn down before the consumer updates: a repeatable mutation
    // cannot be confirmed, so it must surface (not narrow to Ok).
    drop(rx);
    assert!(matches!(
        handle.update_metadata(HashMap::new()).await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
}

#[tokio::test]
async fn set_state_errors_when_backend_drops_responder() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::SetState { responder, .. }) = rx.recv().await else {
            panic!("a set-state request must arrive");
        };
        drop(responder);
    });

    assert!(matches!(
        handle.set_state(InstanceState::Enabled).await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn deregister_round_trips_backend_result() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::Deregister { responder }) = rx.recv().await else {
            panic!("a deregister request must arrive");
        };
        responder.respond(Ok(()));
    });

    assert!(handle.deregister().await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn deregister_propagates_backend_error() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::Deregister { responder }) = rx.recv().await else {
            panic!("a deregister request must arrive");
        };
        responder.respond(Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: "lost mid-deregister".to_owned(),
        }));
    });

    assert!(matches!(
        handle.deregister().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn deregister_after_backend_gone_is_best_effort_ok() {
    let (rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    // Backend torn down (e.g. cluster shutdown) before the consumer
    // deregisters — the §3.7 best-effort Ok narrowing applies to the
    // consuming op.
    drop(rx);
    assert!(handle.deregister().await.is_ok());
}

#[tokio::test]
async fn deregister_errors_when_backend_drops_responder_without_reply() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(ServiceRequest::Deregister { responder }) = rx.recv().await else {
            panic!("a deregister request must arrive");
        };
        drop(responder);
    });

    assert!(matches!(
        handle.deregister().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn dropping_handle_performs_no_io_and_does_not_deregister() {
    let (mut rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
    assert_eq!(handle.instance_id(), "i-1");
    // Dropping the handle must not send any command and must not block.
    drop(handle);
    assert!(rx.recv().await.is_none());
}

// Created: 2026-06-11 by Constructor Tech
use std::time::Duration;

use super::{LockGuard, LockRequest};
use crate::error::{ClusterError, ProviderErrorKind};

#[tokio::test]
async fn renew_round_trips_backend_ok() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(LockRequest::Renew { new_ttl, responder }) = rx.recv().await else {
            panic!("a renew request must arrive");
        };
        assert_eq!(new_ttl, Duration::from_secs(5));
        responder.respond(Ok(()));
    });

    assert!(guard.renew(Duration::from_secs(5)).await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn renew_is_repeatable_and_surfaces_backend_expired() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        // First renewal succeeds.
        let Some(LockRequest::Renew { responder, .. }) = rx.recv().await else {
            panic!("first renew request must arrive");
        };
        responder.respond(Ok(()));
        // Second renewal is rejected — the TTL had elapsed.
        let Some(LockRequest::Renew { responder, .. }) = rx.recv().await else {
            panic!("second renew request must arrive");
        };
        responder.respond(Err(ClusterError::LockExpired {
            name: "rate-limit".to_owned(),
        }));
    });

    assert!(guard.renew(Duration::from_secs(1)).await.is_ok());
    assert!(matches!(
        guard.renew(Duration::from_secs(1)).await,
        Err(ClusterError::LockExpired { .. })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn renew_after_backend_gone_reports_expired() {
    let (rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    // Backend torn down before the consumer renews: nothing maintains the
    // claim, so a renewal cannot keep it.
    drop(rx);
    assert!(matches!(
        guard.renew(Duration::from_secs(5)).await,
        Err(ClusterError::LockExpired { name }) if name == "rate-limit"
    ));
}

#[tokio::test]
async fn renew_errors_when_backend_drops_responder() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(LockRequest::Renew { responder, .. }) = rx.recv().await else {
            panic!("a renew request must arrive");
        };
        drop(responder);
    });

    assert!(matches!(
        guard.renew(Duration::from_secs(5)).await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn release_round_trips_backend_result() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(LockRequest::Release { responder }) = rx.recv().await else {
            panic!("a release request must arrive");
        };
        responder.respond(Ok(()));
    });

    assert!(guard.release().await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn release_propagates_backend_error() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(LockRequest::Release { responder }) = rx.recv().await else {
            panic!("a release request must arrive");
        };
        responder.respond(Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: "lost mid-release".to_owned(),
        }));
    });

    assert!(matches!(
        guard.release().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn release_after_backend_gone_is_best_effort_ok() {
    let (rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    // Backend torn down (e.g. cluster shutdown) before the consumer releases.
    drop(rx);
    assert!(guard.release().await.is_ok());
}

#[tokio::test]
async fn release_errors_when_backend_drops_responder_without_reply() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    let backend = tokio::spawn(async move {
        let Some(LockRequest::Release { responder }) = rx.recv().await else {
            panic!("a release request must arrive");
        };
        drop(responder);
    });

    assert!(matches!(
        guard.release().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn dropping_guard_performs_no_io_and_does_not_release() {
    let (mut rx, guard) = LockGuard::channel("rate-limit".to_owned(), 1);
    // Dropping the guard must not send any command and must not block.
    drop(guard);
    assert!(rx.recv().await.is_none());
}

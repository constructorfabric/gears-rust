#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(feature = "integration", any(feature = "pg", feature = "mysql")))]

//! Native (PG/MySQL) advisory-lock integration tests for the single-session model.
//!
//! These require Docker (testcontainers) and the `integration` feature. They exercise the public
//! API only (`connect_db` -> `Db::lock` / `try_lock`), which routes through the dedicated
//! single-connection lock session.

mod common;

use std::time::Duration;

use toolkit_db::{ConnectOpts, LockConfig, connect_db};

/// Short keepalive so the "session stays warm across an idle gap" assertion is quick.
fn opts_with_short_keepalive() -> ConnectOpts {
    ConnectOpts {
        lock_keepalive: Some(Duration::from_millis(200)),
        ..Default::default()
    }
}

fn uniq(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{prefix}_{nanos}")
}

/// One session holds MANY distinct keys at once — a held lock does not pin the single connection.
async fn one_session_holds_many_keys(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    let id = uniq("many");

    let mut guards = Vec::new();
    for i in 0..8 {
        let guard = db
            .lock("test_gear", &format!("{id}_{i}"))
            .await
            .unwrap_or_else(|e| panic!("key {i} should acquire on the shared session: {e:?}"));
        guards.push(guard);
    }

    // All eight keys are held simultaneously by the one lock session.
    for guard in guards {
        guard.release().await.expect("release");
    }
}

/// Contention on the same key reports `AlreadyHeld`; releasing allows immediate re-acquire.
async fn contention_and_reacquire(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    let key = uniq("contend");

    let guard = db.lock("test_gear", &key).await.expect("first acquire");

    // A second acquire of the same key on the same session is contended (non-blocking).
    let config = LockConfig {
        max_wait: Some(Duration::from_millis(200)),
        // One retry after the first attempt = two attempts total.
        max_retries: Some(1),
        ..Default::default()
    };
    let contended = db
        .try_lock("test_gear", &key, config)
        .await
        .expect("try_lock ok");
    assert!(contended.is_none(), "same key must be contended while held");

    guard.release().await.expect("release");

    // After release the key is immediately re-acquirable.
    let again = db.lock("test_gear", &key).await.expect("reacquire");
    again.release().await.expect("release again");
}

/// The keepalive ping keeps the single session alive across an idle gap longer than the interval.
async fn keepalive_keeps_session_warm(url: &str) {
    let db = connect_db(url, opts_with_short_keepalive())
        .await
        .expect("connect");
    let key = uniq("keepalive");

    let guard = db.lock("test_gear", &key).await.expect("acquire");

    // Idle for several keepalive intervals; the keepalive ping must keep the session (and lock)
    // alive across the gap.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Release must SUCCEED on the still-warm session. A `NotHeld` here would mean the session was
    // reaped during the idle gap despite the keepalive — exactly the regression this test guards
    // against — so accepting it would make the test unable to fail on its own subject.
    guard
        .release()
        .await
        .expect("keepalive should keep the session warm across the idle gap; release must succeed");
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn postgres_single_session_model() -> anyhow::Result<()> {
    let db = common::bring_up_postgres().await?;
    one_session_holds_many_keys(&db.url).await;
    contention_and_reacquire(&db.url).await;
    keepalive_keeps_session_warm(&db.url).await;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_single_session_model() -> anyhow::Result<()> {
    let db = common::bring_up_mysql().await?;
    one_session_holds_many_keys(&db.url).await;
    contention_and_reacquire(&db.url).await;
    keepalive_keeps_session_warm(&db.url).await;
    Ok(())
}

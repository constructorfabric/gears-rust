#![cfg(all(feature = "integration", feature = "outbox"))]
//! Integration tests for `AsyncProducer` (outbox feature).
//! These tests require a real modkit-db database and are skipped in CI unless
//! `--features integration,outbox` is passed with a valid DB environment.
//!
//! For now, this file is a placeholder — the outbox integration tests will
//! be expanded once the event-broker impl crate and DB harness are available.

#[test]
fn placeholder_async_producer() {
    // Placeholder: full outbox integration tests are in the impl crate test suite.
    // This file gates the async producer integration tests behind the `outbox`
    // feature and `integration` feature so they don't run in regular CI.
}

// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Retry policy of the service, driven through a scripted fake transport (no
//! network, no sleeping beyond a millisecond of backoff): which operations
//! retry, on what, how often, and what the final error is.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion};
use uuid::Uuid;

use super::*;
use crate::config::{RetryConfig, VaultCredStorePluginConfig, VaultToken};
use crate::domain::test_ext::PluginErrorExt as _;
use crate::domain::transport::{HttpMethod, TransportError, VaultRequest, VaultResponse};

type Reply = Result<VaultResponse, TransportError>;

/// Answers with the scripted replies in order and records every request. An
/// exhausted script answers with a transport failure, so an unexpected extra
/// attempt shows up as a wrong call count, not a hang.
struct FakeTransport {
    script: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<VaultRequest>>,
}

impl FakeTransport {
    fn new(script: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn requests(&self) -> Vec<VaultRequest> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn methods(&self) -> Vec<HttpMethod> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|r| r.method)
            .collect()
    }
}

#[async_trait]
impl VaultTransport for FakeTransport {
    async fn send(&self, request: VaultRequest) -> Reply {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(TransportError::Other))
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "script entries are `Reply` values, most of them successful responses"
)]
fn reply(status: u16, body: &str) -> Reply {
    Ok(VaultResponse {
        status,
        body: body.to_owned(),
        retry_after: None,
    })
}

fn service(fake: &Arc<FakeTransport>, max_attempts: u32) -> Service {
    let cfg = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("t")),
        retry: RetryConfig {
            max_attempts,
            base_delay_ms: 1,
        },
        ..VaultCredStorePluginConfig::default()
    };
    Service::new(Arc::clone(fake) as Arc<dyn VaultTransport>, &cfg)
}

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn vv(s: &str) -> ValueVersion {
    ValueVersion::new(s)
}

const READ_OK: &str = r#"{"data":{"data":{"value":"aGk="},"metadata":{"destroyed":false}}}"#;
const PUT_OK: &str = r#"{"data":{"version":4}}"#;
const META: &str = r#"{"data":{"versions":{"1":{"destroyed":false},"2":{"destroyed":false},"3":{"destroyed":false}}}}"#;

// -- get --------------------------------------------------------------------------

#[tokio::test]
async fn get_retries_transport_failures_until_success() {
    let fake = FakeTransport::new(vec![
        Err(TransportError::Timeout),
        Err(TransportError::Connect),
        reply(200, READ_OK),
    ]);
    let got = service(&fake, 3)
        .get_value(&key(), &vv("1"))
        .await
        .expect("third attempt succeeds")
        .expect("value");
    assert_eq!(got.as_bytes(), b"hi");
    assert_eq!(fake.calls(), 3);
}

#[tokio::test]
async fn get_retries_transient_statuses() {
    for status in [500, 502, 503, 408, 429] {
        let fake = FakeTransport::new(vec![reply(status, ""), reply(200, READ_OK)]);
        let got = service(&fake, 3).get_value(&key(), &vv("1")).await;
        assert!(got.expect("recovers").is_some(), "status {status}");
        assert_eq!(fake.calls(), 2, "status {status}");
    }
}

#[tokio::test]
async fn get_retries_a_token_that_could_not_be_loaded() {
    let fake = FakeTransport::new(vec![Err(TransportError::Credentials), reply(200, READ_OK)]);
    let got = service(&fake, 3).get_value(&key(), &vv("1")).await;
    assert!(got.expect("recovers").is_some());
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn get_gives_up_after_max_attempts_with_service_unavailable() {
    let fake = FakeTransport::new(vec![
        Err(TransportError::Timeout),
        Err(TransportError::Timeout),
        Err(TransportError::Timeout),
        reply(200, READ_OK),
    ]);
    let err = service(&fake, 3)
        .get_value(&key(), &vv("1"))
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(fake.calls(), 3, "exactly max_attempts, no more");
}

#[tokio::test]
async fn max_attempts_one_disables_retries() {
    let fake = FakeTransport::new(vec![reply(503, ""), reply(200, READ_OK)]);
    let err = service(&fake, 1)
        .get_value(&key(), &vv("1"))
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn get_does_not_retry_definite_answers() {
    for (status, body) in [
        (404, r#"{"errors":[]}"#),
        (400, r#"{"errors":["bad"]}"#),
        (403, r#"{"errors":["permission denied"]}"#),
        (401, ""),
    ] {
        let fake = FakeTransport::new(vec![reply(status, body), reply(200, READ_OK)]);
        drop(service(&fake, 3).get_value(&key(), &vv("1")).await);
        assert_eq!(fake.calls(), 1, "status {status}");
    }
}

#[tokio::test]
async fn final_transient_status_keeps_the_retry_after_hint() {
    let fake = FakeTransport::new(vec![
        reply(503, ""),
        Ok(VaultResponse {
            status: 503,
            body: String::new(),
            retry_after: Some(Duration::from_secs(12)),
        }),
    ]);
    let err = service(&fake, 2)
        .get_value(&key(), &vv("1"))
        .await
        .unwrap_err();
    assert_eq!(err.retry_after().map(|d| d.as_secs()), Some(12));
}

#[tokio::test]
async fn forbidden_is_not_retried_and_is_credentials_rejected() {
    let fake = FakeTransport::new(vec![reply(403, r#"{"errors":["permission denied"]}"#)]);
    let err = service(&fake, 3)
        .get_value(&key(), &vv("1"))
        .await
        .unwrap_err();
    assert!(err.is_credentials_rejected());
    assert_eq!(fake.calls(), 1);
}

// -- put -------------------------------------------------------------------------

#[tokio::test]
async fn put_is_never_retried_on_the_limit_call() {
    for first in [
        Err(TransportError::Timeout),
        Err(TransportError::Connect),
        Err(TransportError::Other),
        reply(500, ""),
        reply(503, ""),
        reply(429, ""),
    ] {
        let fake = FakeTransport::new(vec![first, reply(204, ""), reply(200, PUT_OK)]);
        let err = service(&fake, 5)
            .put_value(&key(), SecretValue::from("v"))
            .await
            .unwrap_err();
        assert!(err.is_unavailable());
        assert_eq!(
            fake.calls(),
            1,
            "the limit call is sent at most once and no data write follows"
        );
    }
}

#[tokio::test]
async fn put_is_never_retried_on_the_data_write() {
    for second in [
        Err(TransportError::Timeout),
        Err(TransportError::Connect),
        Err(TransportError::Other),
        reply(500, ""),
        reply(503, ""),
        reply(429, ""),
    ] {
        let fake = FakeTransport::new(vec![reply(204, ""), second, reply(200, PUT_OK)]);
        let err = service(&fake, 5)
            .put_value(&key(), SecretValue::from("v"))
            .await
            .unwrap_err();
        assert!(err.is_unavailable());
        assert_eq!(fake.calls(), 2, "limit call once, data write once");
    }
}

#[tokio::test]
async fn put_forbidden_on_the_limit_call_is_credentials_rejected_without_a_write() {
    let fake = FakeTransport::new(vec![
        reply(403, r#"{"errors":["permission denied"]}"#),
        reply(200, PUT_OK),
    ]);
    let err = service(&fake, 5)
        .put_value(&key(), SecretValue::from("v"))
        .await
        .unwrap_err();
    assert!(err.is_credentials_rejected());
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn put_sets_the_limit_before_writing_the_data() {
    let fake = FakeTransport::new(vec![reply(204, ""), reply(200, PUT_OK)]);
    let k = key();
    let version = service(&fake, 5)
        .put_value(&k, SecretValue::from("v"))
        .await
        .expect("ok");
    assert_eq!(version, vv("4"));
    let seen = fake.requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].method, HttpMethod::Post);
    assert!(seen[0].url.contains("/metadata/"), "{}", seen[0].url);
    assert_eq!(
        seen[0].json_body.as_deref(),
        Some(r#"{"max_versions":100}"#)
    );
    assert_eq!(seen[1].method, HttpMethod::Post);
    assert!(seen[1].url.contains("/data/"), "{}", seen[1].url);
}

// -- delete_key ------------------------------------------------------------------

#[tokio::test]
async fn delete_key_retries_transient_failures() {
    let fake = FakeTransport::new(vec![
        reply(503, ""),
        Err(TransportError::Timeout),
        reply(204, ""),
    ]);
    service(&fake, 3)
        .delete_key_value(&key())
        .await
        .expect("ok");
    assert_eq!(fake.calls(), 3);
    assert!(fake.methods().iter().all(|m| *m == HttpMethod::Delete));
}

#[tokio::test]
async fn delete_key_gives_up_after_max_attempts() {
    let fake = FakeTransport::new(vec![reply(502, ""), reply(502, ""), reply(502, "")]);
    let err = service(&fake, 3)
        .delete_key_value(&key())
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(fake.calls(), 3);
}

// -- destroy ---------------------------------------------------------------------

#[tokio::test]
async fn destroy_below_retries_the_metadata_read_then_the_destroy_call() {
    let fake = FakeTransport::new(vec![
        reply(503, ""),
        reply(200, META),
        Err(TransportError::Connect),
        reply(204, ""),
    ]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("3")))
        .await
        .expect("ok");
    assert_eq!(
        fake.methods(),
        vec![
            HttpMethod::Get,
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Post
        ]
    );
}

#[tokio::test]
async fn destroy_attempts_are_counted_per_call_not_per_operation() {
    // Two failed metadata reads then success, then two failed destroys then
    // success: each call has its own budget of three attempts.
    let fake = FakeTransport::new(vec![
        reply(500, ""),
        reply(500, ""),
        reply(200, META),
        reply(500, ""),
        reply(500, ""),
        reply(204, ""),
    ]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("3")))
        .await
        .expect("ok");
    assert_eq!(fake.calls(), 6);
}

#[tokio::test]
async fn destroy_exactly_retries_only_the_destroy_call() {
    let fake = FakeTransport::new(vec![reply(503, ""), reply(204, "")]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Exactly(vv("2")))
        .await
        .expect("ok");
    assert_eq!(fake.methods(), vec![HttpMethod::Post, HttpMethod::Post]);
}

#[tokio::test]
async fn destroy_below_selects_versions_from_the_metadata_map() {
    let fake = FakeTransport::new(vec![reply(200, META), reply(204, "")]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("3")))
        .await
        .expect("ok");
    let seen = fake.seen.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(seen[1].json_body.as_deref(), Some(r#"{"versions":[1,2]}"#));
}

#[tokio::test]
async fn destroy_below_with_an_empty_selection_stops_after_the_read() {
    let fake = FakeTransport::new(vec![reply(200, META), reply(204, "")]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("1")))
        .await
        .expect("ok");
    // `Below(1)`: nothing is older than version 1, so not even the read.
    assert_eq!(fake.calls(), 0);

    let fake = FakeTransport::new(vec![reply(200, META), reply(204, "")]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("2")))
        .await
        .expect("ok");
    // Version 1 is older than 2: one read, one destroy.
    assert_eq!(fake.calls(), 2);

    let fake = FakeTransport::new(vec![reply(
        200,
        r#"{"data":{"versions":{"5":{"destroyed":false}}}}"#,
    )]);
    service(&fake, 3)
        .destroy_value(&key(), &DestroySelector::Below(vv("5")))
        .await
        .expect("ok");
    // Nothing below 5 is left: the read is the only call.
    assert_eq!(fake.calls(), 1);
}

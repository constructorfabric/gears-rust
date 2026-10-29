//! The per-attempt bound of [`Transport::exchange`], against a registry that accepts
//! the connection and never answers.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::net::TcpListener;
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_http::{HttpClient, TransportSecurity};

use super::{Credential, Transport};

const ATTEMPT: Duration = Duration::from_millis(100);

/// A listener that accepts every connection and holds it open unanswered; the count
/// is how many connections it accepted.
async fn silent_registry() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base_url = format!("http://{}", listener.local_addr().expect("address"));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((connection, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(connection);
        }
    });
    (base_url, accepted)
}

/// Each attempt of a hung exchange ends at the configured timeout and is retried; the
/// HTTP client's own timeout here is an hour, far past the test's 10s backstop, so only
/// the exchange's bound can end an attempt in time.
#[tokio::test]
async fn a_hung_attempt_times_out_and_is_retried() {
    let (base_url, accepted) = silent_registry().await;
    let config = ClientConfig::new(base_url)
        .with_timeout(ATTEMPT)
        .with_retry(RetryConfig {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            multiplier: 1.0,
        });
    let http = HttpClient::builder()
        .retry(None)
        .transport(TransportSecurity::AllowInsecureHttp)
        .timeout(Duration::from_secs(3600))
        .build()
        .expect("an HTTP client");
    let transport = Transport::with_http_client(http, "/registry", config);
    let url = transport.url("/entities");

    let exchange = transport.exchange("list_entities", Credential::Bearer(None), |http| {
        Ok(http.get(&url))
    });
    let error = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .expect("bounded by its attempts")
        .err()
        .expect("a silent registry never answers");

    assert!(
        matches!(error, TransportError::Timeout(after) if after == ATTEMPT),
        "{error:?}"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "one connection per attempt"
    );
}

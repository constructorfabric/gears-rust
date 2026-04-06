use super::*;
use crate::config::DEFAULT_USER_AGENT;

impl HttpClientBuilder {
    /// Build an `HttpClient` with a custom inner service replacing the
    /// hyper connector. The full middleware stack (Retry, Concurrency,
    /// Buffer, etc.) is applied on top.
    fn build_with_inner_service(self, inner: InnerService) -> crate::HttpClient {
        let mut boxed_service = inner;

        if let Some(ref retry_config) = self.config.retry {
            let retry_layer =
                RetryLayer::with_total_timeout(retry_config.clone(), self.config.total_timeout);
            let retry_service = ServiceBuilder::new()
                .layer(retry_layer)
                .service(boxed_service);
            boxed_service = retry_service.boxed_clone();
        }

        if let Some(rate_limit) = self.config.rate_limit
            && rate_limit.max_concurrent_requests < usize::MAX
        {
            let limited_service = ServiceBuilder::new()
                .layer(LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(
                    rate_limit.max_concurrent_requests,
                ))
                .service(boxed_service);
            let limited_service = limited_service.map_err(map_load_shed_error);
            boxed_service = limited_service.boxed_clone();
        }

        let buffer_capacity = self.config.buffer_capacity.max(1);
        let buffered_service: crate::client::BufferedService =
            Buffer::new(boxed_service, buffer_capacity);

        crate::HttpClient {
            service: buffered_service,
            max_body_size: self.config.max_body_size,
            transport_security: self.config.transport,
        }
    }
}

#[test]
fn test_builder_default() {
    let builder = HttpClientBuilder::new();
    assert_eq!(builder.config.request_timeout, Duration::from_secs(30));
    assert_eq!(builder.config.user_agent, DEFAULT_USER_AGENT);
    assert!(builder.config.retry.is_some());
    assert_eq!(builder.config.buffer_capacity, 1024);
}

#[test]
fn test_builder_with_config() {
    let config = HttpClientConfig::minimal();
    let builder = HttpClientBuilder::with_config(config);
    assert_eq!(builder.config.request_timeout, Duration::from_secs(10));
}

#[test]
fn test_builder_timeout() {
    let builder = HttpClientBuilder::new().timeout(Duration::from_secs(60));
    assert_eq!(builder.config.request_timeout, Duration::from_secs(60));
}

#[test]
fn test_builder_user_agent() {
    let builder = HttpClientBuilder::new().user_agent("custom/1.0");
    assert_eq!(builder.config.user_agent, "custom/1.0");
}

#[test]
fn test_builder_retry() {
    let builder = HttpClientBuilder::new().retry(None);
    assert!(builder.config.retry.is_none());
}

#[test]
fn test_builder_max_body_size() {
    let builder = HttpClientBuilder::new().max_body_size(1024);
    assert_eq!(builder.config.max_body_size, 1024);
}

#[test]
fn test_builder_transport_security() {
    let builder = HttpClientBuilder::new().transport(TransportSecurity::TlsOnly);
    assert_eq!(builder.config.transport, TransportSecurity::TlsOnly);

    let builder = HttpClientBuilder::new().deny_insecure_http();
    assert_eq!(builder.config.transport, TransportSecurity::TlsOnly);

    let builder = HttpClientBuilder::new();
    assert_eq!(
        builder.config.transport,
        TransportSecurity::AllowInsecureHttp
    );
}

#[test]
fn test_builder_otel() {
    let builder = HttpClientBuilder::new().with_otel();
    assert!(builder.config.otel);
}

#[test]
fn test_builder_buffer_capacity() {
    let builder = HttpClientBuilder::new().buffer_capacity(512);
    assert_eq!(builder.config.buffer_capacity, 512);
}

#[test]
fn test_builder_buffer_capacity_zero_clamped() {
    let builder = HttpClientBuilder::new().buffer_capacity(0);
    assert_eq!(
        builder.config.buffer_capacity, 1,
        "buffer_capacity=0 should be clamped to 1"
    );
}

#[tokio::test]
async fn test_builder_buffer_capacity_zero_in_config_clamped() {
    let config = HttpClientConfig {
        buffer_capacity: 0,
        ..Default::default()
    };
    let result = HttpClientBuilder::with_config(config).build();
    assert!(
        result.is_ok(),
        "build() should succeed with capacity clamped to 1"
    );
}

#[tokio::test]
async fn test_builder_build_with_otel() {
    let client = HttpClientBuilder::new().with_otel().build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_builder_with_auth_layer() {
    let client = HttpClientBuilder::new().with_auth_layer(|svc| svc).build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_builder_build() {
    let client = HttpClientBuilder::new().build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_builder_build_with_deny_insecure_http() {
    let client = HttpClientBuilder::new().deny_insecure_http().build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_builder_build_with_sse_config() {
    use crate::config::HttpClientConfig;
    let config = HttpClientConfig::sse();
    let client = HttpClientBuilder::with_config(config).build();
    assert!(client.is_ok(), "SSE config should build successfully");
}

#[tokio::test]
async fn test_builder_build_invalid_user_agent() {
    let client = HttpClientBuilder::new()
        .user_agent("invalid\x00agent")
        .build();
    assert!(client.is_err());
}

#[tokio::test]
async fn test_builder_default_uses_webpki_roots() {
    let builder = HttpClientBuilder::new();
    assert_eq!(builder.config.tls_roots, TlsRootConfig::WebPki);
    let client = builder.build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_builder_native_roots() {
    let config = HttpClientConfig {
        tls_roots: TlsRootConfig::Native,
        ..Default::default()
    };
    let result = HttpClientBuilder::with_config(config).build();

    match &result {
        Ok(_) => {}
        Err(HttpError::Tls(err)) => {
            let msg = err.to_string();
            assert!(
                msg.contains("native root") || msg.contains("certificate"),
                "TLS error should mention certificates: {msg}"
            );
        }
        Err(other) => {
            panic!("Unexpected error type: {other:?}");
        }
    }
}

#[tokio::test]
async fn test_builder_webpki_roots_https_only() {
    let config = HttpClientConfig {
        tls_roots: TlsRootConfig::WebPki,
        transport: TransportSecurity::TlsOnly,
        ..Default::default()
    };
    let client = HttpClientBuilder::with_config(config).build();
    assert!(client.is_ok());
}

#[tokio::test]
async fn test_http2_enabled_for_all_configurations() {
    let client = HttpClientBuilder::new().build();
    assert!(
        client.is_ok(),
        "WebPki + AllowInsecureHttp should build with HTTP/2 enabled"
    );

    let client = HttpClientBuilder::new()
        .transport(TransportSecurity::TlsOnly)
        .build();
    assert!(
        client.is_ok(),
        "WebPki + TlsOnly should build with HTTP/2 enabled"
    );

    let config = HttpClientConfig {
        tls_roots: TlsRootConfig::Native,
        transport: TransportSecurity::AllowInsecureHttp,
        ..Default::default()
    };
    let client = HttpClientBuilder::with_config(config).build();
    assert!(
        client.is_ok(),
        "Native + AllowInsecureHttp should build with HTTP/2 enabled"
    );

    let config = HttpClientConfig {
        tls_roots: TlsRootConfig::Native,
        transport: TransportSecurity::TlsOnly,
        ..Default::default()
    };
    let client = HttpClientBuilder::with_config(config).build();
    assert!(
        client.is_ok(),
        "Native + TlsOnly should build with HTTP/2 enabled"
    );
}

#[tokio::test]
async fn test_load_shedding_returns_overloaded_error() {
    use bytes::Bytes;
    use http::{Request, Response};
    use http_body_util::Full;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tower::Service;
    use tower::ServiceExt;

    #[derive(Clone)]
    struct SlotHoldingService {
        active: Arc<AtomicUsize>,
    }

    impl Service<Request<Full<Bytes>>> for SlotHoldingService {
        type Response = Response<Full<Bytes>>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request<Full<Bytes>>) -> Self::Future {
            self.active.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    let active = Arc::new(AtomicUsize::new(0));

    let service = tower::ServiceBuilder::new()
        .layer(LoadShedLayer::new())
        .layer(ConcurrencyLimitLayer::new(1))
        .service(SlotHoldingService {
            active: active.clone(),
        });

    let service = service.map_err(map_load_shed_error);

    let req1 = Request::builder()
        .uri("http://test")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let mut svc1 = service.clone();

    let svc1_ready = svc1.ready().await.unwrap();
    let _pending_fut = svc1_ready.call(req1);

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        active.load(Ordering::SeqCst),
        1,
        "First request should be active"
    );

    let req2 = Request::builder()
        .uri("http://test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let mut svc2 = service.clone();

    let result = tokio::time::timeout(Duration::from_millis(100), async {
        match svc2.ready().await {
            Ok(ready_svc) => ready_svc.call(req2).await,
            Err(e) => Err(e),
        }
    })
    .await;

    assert!(result.is_ok(), "Request should not hang");
    let err = result.unwrap().unwrap_err();
    assert!(
        matches!(err, HttpError::Overloaded),
        "Expected Overloaded error, got: {err:?}"
    );
}

#[test]
fn test_map_tower_error_preserves_overloaded() {
    let http_err = HttpError::Overloaded;
    let boxed: tower::BoxError = Box::new(http_err);
    let result = map_tower_error(boxed, Duration::from_secs(30));

    assert!(
        matches!(result, HttpError::Overloaded),
        "Should preserve HttpError::Overloaded, got: {result:?}"
    );
}

#[test]
fn test_map_tower_error_preserves_service_closed() {
    let http_err = HttpError::ServiceClosed;
    let boxed: tower::BoxError = Box::new(http_err);
    let result = map_tower_error(boxed, Duration::from_secs(30));

    assert!(
        matches!(result, HttpError::ServiceClosed),
        "Should preserve HttpError::ServiceClosed, got: {result:?}"
    );
}

#[test]
fn test_map_tower_error_preserves_timeout_attempt() {
    let original_duration = Duration::from_secs(5);
    let http_err = HttpError::Timeout(original_duration);
    let boxed: tower::BoxError = Box::new(http_err);
    let result = map_tower_error(boxed, Duration::from_secs(30));

    match result {
        HttpError::Timeout(d) => {
            assert_eq!(
                d, original_duration,
                "Should preserve original timeout duration"
            );
        }
        other => panic!("Should preserve HttpError::Timeout, got: {other:?}"),
    }
}

#[test]
fn test_map_tower_error_wraps_unknown_as_transport() {
    let other_err: tower::BoxError = Box::new(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "connection refused",
    ));
    let result = map_tower_error(other_err, Duration::from_secs(30));

    assert!(
        matches!(result, HttpError::Transport(_)),
        "Should wrap unknown errors as Transport, got: {result:?}"
    );
}

#[tokio::test]
async fn test_cancellation_propagates_through_full_stack() {
    use crate::response::ResponseBody;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use tower::Service;

    #[derive(Clone)]
    struct PendingService {
        completed: Arc<AtomicBool>,
        drop_notifier: Arc<tokio::sync::Notify>,
        started_notifier: Arc<tokio::sync::Notify>,
    }

    struct FutureGuard {
        completed: Arc<AtomicBool>,
        drop_notifier: Arc<tokio::sync::Notify>,
    }

    impl Drop for FutureGuard {
        fn drop(&mut self) {
            if !self.completed.load(Ordering::SeqCst) {
                self.drop_notifier.notify_one();
            }
        }
    }

    impl Service<http::Request<Full<Bytes>>> for PendingService {
        type Response = http::Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<Full<Bytes>>) -> Self::Future {
            let completed = self.completed.clone();
            let drop_notifier = self.drop_notifier.clone();
            let started_notifier = self.started_notifier.clone();
            Box::pin(async move {
                let _guard = FutureGuard {
                    completed: completed.clone(),
                    drop_notifier,
                };
                started_notifier.notify_one();
                std::future::pending::<()>().await;
                completed.store(true, Ordering::SeqCst);
                unreachable!()
            })
        }
    }

    let inner_completed = Arc::new(AtomicBool::new(false));
    let drop_notifier = Arc::new(tokio::sync::Notify::new());
    let started_notifier = Arc::new(tokio::sync::Notify::new());

    let inner = PendingService {
        completed: inner_completed.clone(),
        drop_notifier: drop_notifier.clone(),
        started_notifier: started_notifier.clone(),
    };

    let client = HttpClientBuilder::new()
        .timeout(Duration::from_secs(30))
        .retry(None)
        .build_with_inner_service(inner.boxed_clone());

    let send_handle = tokio::spawn({
        let client = client.clone();
        async move { client.get("http://fake/slow").send().await }
    });

    started_notifier.notified().await;
    send_handle.abort();

    tokio::time::timeout(Duration::from_secs(5), drop_notifier.notified())
        .await
        .expect(
            "Inner service future should have been dropped within 5s - \
             the full modkit-http stack must propagate cancellation",
        );

    assert!(
        !inner_completed.load(Ordering::SeqCst),
        "Inner service future should NOT have completed"
    );
}

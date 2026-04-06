use super::*;
use crate::config::IDEMPOTENCY_KEY_HEADER;
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;

/// Helper to create a boxed `ResponseBody` from bytes
fn make_response_body(data: &[u8]) -> ResponseBody {
    let body = Full::new(Bytes::from(data.to_vec()));
    body.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
        .boxed()
}

#[tokio::test]
async fn test_retry_layer_successful_request() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct CountingService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for CountingService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(make_response_body(b""))
                    .unwrap();
                Ok(response)
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = CountingService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig::default();
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 1); // Should only call once on success
}

/// Test: POST request with 500 is NOT retried and returns Ok(Response).
/// With new semantics: 500 for non-idempotent method passes through as Ok(Response).
#[tokio::test]
async fn test_retry_layer_post_not_retried_on_5xx() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct ServerErrorService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for ServerErrorService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                // Return Ok(Response) with 500 status - POST won't retry
                Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(make_response_body(b"Internal Server Error"))
                    .unwrap())
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = ServerErrorService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    // New semantics: returns Ok(Response) with 500 status, NOT Err
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(*call_count.lock().unwrap(), 1); // POST should NOT be retried on 500
}

/// Test: GET request with 500 is retried (idempotent method).
/// Returns Ok(Response) with final status after retries exhaust or success.
#[tokio::test]
async fn test_retry_layer_get_retried_on_5xx() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FailThenSucceedService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for FailThenSucceedService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 3 {
                    // Return 500 - will trigger retry for GET
                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(make_response_body(b"Internal Server Error"))
                        .unwrap())
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = FailThenSucceedService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().status(), StatusCode::OK);
    assert_eq!(*call_count.lock().unwrap(), 3); // GET should retry on 500
}

/// Test: 429 is always retried (POST included), returns Ok(Response).
#[tokio::test]
async fn test_retry_layer_always_retries_429() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct RateLimitThenSucceedService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for RateLimitThenSucceedService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 2 {
                    // Return 429 - triggers retry for all methods
                    Ok(Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .body(make_response_body(b"Rate limited"))
                        .unwrap())
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = RateLimitThenSucceedService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    // 429 should be retried even for POST
    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().status(), StatusCode::OK);
    assert_eq!(*call_count.lock().unwrap(), 2); // POST retries on 429
}

#[tokio::test]
async fn test_retry_layer_retries_transport_errors() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FailThenSucceedService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for FailThenSucceedService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 3 {
                    Err(HttpError::Transport(Box::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "connection reset",
                    ))))
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = FailThenSucceedService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 3); // Should retry until success
}

/// Test: POST request is NOT retried on transport errors (by default, for safety)
#[tokio::test]
async fn test_retry_layer_post_not_retried_on_transport_error() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct TransportErrorService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for TransportErrorService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                Err(HttpError::Transport(Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection reset",
                ))))
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = TransportErrorService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    // POST without idempotency key should NOT be retried on transport error
    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_err()); // Should return error, not retry
    assert_eq!(*call_count.lock().unwrap(), 1); // Only one attempt
}

/// Test: POST request WITH idempotency key IS retried on transport errors
#[tokio::test]
async fn test_retry_layer_post_with_idempotency_key_retried() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FailThenSucceedService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for FailThenSucceedService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 3 {
                    Err(HttpError::Transport(Box::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "connection reset",
                    ))))
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = FailThenSucceedService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    // POST WITH idempotency key should be retried on transport error
    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .header(IDEMPOTENCY_KEY_HEADER, "unique-key-123")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok()); // Should succeed after retries
    assert_eq!(*call_count.lock().unwrap(), 3); // Should retry until success
}

#[tokio::test]
async fn test_retry_layer_does_not_retry_json_errors() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct JsonErrorService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for JsonErrorService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                // Simulate a JSON parse error (non-retryable)
                let err: serde_json::Error =
                    serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
                Err(HttpError::Json(err))
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = JsonErrorService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig::default();
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_err());
    assert_eq!(*call_count.lock().unwrap(), 1); // Should NOT retry JSON errors
}

#[test]
fn test_calculate_backoff_no_jitter() {
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: 2.0,
        jitter: false,
    };

    let backoff0 = calculate_backoff(&backoff, 0);
    assert_eq!(backoff0, Duration::from_millis(100));

    let backoff1 = calculate_backoff(&backoff, 1);
    assert_eq!(backoff1, Duration::from_millis(200));

    let backoff2 = calculate_backoff(&backoff, 2);
    assert_eq!(backoff2, Duration::from_millis(400));

    // Should cap at max
    let backoff_capped = calculate_backoff(&backoff, 10);
    assert_eq!(backoff_capped, Duration::from_secs(10));
}

#[test]
fn test_calculate_backoff_with_jitter() {
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: 2.0,
        jitter: true,
    };

    let backoff0 = calculate_backoff(&backoff, 0);
    // With jitter, should be between 100ms and 125ms
    assert!(backoff0 >= Duration::from_millis(100));
    assert!(backoff0 <= Duration::from_millis(125));
}

#[test]
fn test_calculate_backoff_with_nan_multiplier() {
    // NaN multiplier should default to 1.0, not panic
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: f64::NAN,
        jitter: false,
    };

    // Should not panic, NaN multiplier falls back to 1.0
    let result = calculate_backoff(&backoff, 0);
    assert_eq!(result, Duration::from_millis(100));

    let result1 = calculate_backoff(&backoff, 1);
    // With multiplier = 1.0, backoff stays at initial value
    assert_eq!(result1, Duration::from_millis(100));
}

#[test]
fn test_calculate_backoff_with_infinity_multiplier() {
    // Infinity multiplier should default to 1.0, not panic
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: f64::INFINITY,
        jitter: false,
    };

    // Should not panic
    let result = calculate_backoff(&backoff, 0);
    assert_eq!(result, Duration::from_millis(100));
}

#[test]
fn test_calculate_backoff_with_negative_multiplier() {
    // Negative multiplier should default to 1.0, not panic
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: -2.0,
        jitter: false,
    };

    // Should not panic, negative multiplier falls back to 1.0
    let result = calculate_backoff(&backoff, 0);
    assert_eq!(result, Duration::from_millis(100));
}

#[test]
fn test_calculate_backoff_with_huge_attempt() {
    // Large attempt number should not overflow or panic
    let backoff = ExponentialBackoff {
        initial: Duration::from_millis(100),
        max: Duration::from_secs(10),
        multiplier: 2.0,
        jitter: false,
    };

    // usize::MAX should be clamped to i32::MAX internally
    let result = calculate_backoff(&backoff, usize::MAX);
    // Should return max since 2^(i32::MAX) is way beyond max
    assert_eq!(result, Duration::from_secs(10));
}

/// Test: Retry-After header in response is used for backoff timing.
#[tokio::test]
async fn test_retry_layer_uses_retry_after_header() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct RetryAfterService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for RetryAfterService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 2 {
                    // Return 429 with Retry-After header (50ms)
                    Ok(Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .header(http::header::RETRY_AFTER, "0")
                        .body(make_response_body(b"Rate limited"))
                        .unwrap())
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = RetryAfterService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff {
            initial: Duration::from_secs(10), // Long backoff that would fail test
            jitter: false,
            ..ExponentialBackoff::default()
        },
        ignore_retry_after: false, // Use Retry-After header
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let start = std::time::Instant::now();
    let result = retry_service.call(req).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 2);

    // Should have used Retry-After: 0 (immediate), not 10s backoff
    assert!(
        elapsed < Duration::from_secs(1),
        "Expected quick retry using Retry-After, but took {elapsed:?}",
    );
}

/// Test: Retry-After header is ignored when config says to ignore it.
#[tokio::test]
async fn test_retry_layer_ignores_retry_after_when_configured() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct RetryAfterService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for RetryAfterService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 2 {
                    // Return 429 with Retry-After: 10s (should be ignored)
                    Ok(Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .header(http::header::RETRY_AFTER, "10")
                        .body(make_response_body(b"Rate limited"))
                        .unwrap())
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = RetryAfterService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(), // Fast backoff (1ms initial, no jitter)
        ignore_retry_after: true,            // Ignore Retry-After header
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let start = std::time::Instant::now();
    let result = retry_service.call(req).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 2);

    // Should have used 1ms backoff, not 10s Retry-After
    assert!(
        elapsed < Duration::from_secs(1),
        "Expected quick retry using backoff policy (1ms), but took {elapsed:?}",
    );
}

/// Regression test: a large Retry-After value (e.g. from a malicious upstream)
/// must be clamped to `config.backoff.max` so the client doesn't stall.
#[tokio::test]
async fn test_retry_after_clamped_to_backoff_max() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct LargeRetryAfterService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for LargeRetryAfterService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                if *c < 2 {
                    // Return 429 with absurdly large Retry-After (1 hour)
                    Ok(Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .header(http::header::RETRY_AFTER, "3600")
                        .body(make_response_body(b"Rate limited"))
                        .unwrap())
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = LargeRetryAfterService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(50), // Clamp ceiling
            jitter: false,
            ..ExponentialBackoff::default()
        },
        ignore_retry_after: false, // Use (and clamp) Retry-After
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let start = std::time::Instant::now();
    let result = retry_service.call(req).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 2);

    // Without clamping, the client would sleep for 3600s.
    // With clamping to backoff.max (50ms), the retry should be near-instant.
    assert!(
        elapsed < Duration::from_secs(1),
        "Retry-After should be clamped to backoff.max (50ms), but took {elapsed:?}",
    );
}

#[tokio::test]
async fn test_retry_attempt_header_added_on_retry() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct HeaderCapturingService {
        call_count: Arc<Mutex<usize>>,
        captured_headers: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl Service<Request<Full<Bytes>>> for HeaderCapturingService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            let captured_headers = self.captured_headers.clone();

            // Capture the X-Retry-Attempt header value
            let retry_header = req
                .headers()
                .get(RETRY_ATTEMPT_HEADER)
                .map(|v| v.to_str().unwrap_or("invalid").to_owned());

            Box::pin(async move {
                let mut c = count.lock().unwrap();
                *c += 1;
                captured_headers.lock().unwrap().push(retry_header);

                if *c < 3 {
                    // Fail with transport error (always retried)
                    Err(HttpError::Transport(Box::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "connection reset",
                    ))))
                } else {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(make_response_body(b""))
                        .unwrap())
                }
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let captured_headers = Arc::new(Mutex::new(Vec::new()));
    let service = HeaderCapturingService {
        call_count: call_count.clone(),
        captured_headers: captured_headers.clone(),
    };

    let retry_config = RetryConfig {
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;
    assert!(result.is_ok());
    assert_eq!(*call_count.lock().unwrap(), 3);

    // Verify captured headers
    let headers = captured_headers.lock().unwrap();
    assert_eq!(headers.len(), 3);
    // First call (attempt 0): no header
    assert_eq!(headers[0], None);
    // Second call (attempt 1): header = "1"
    assert_eq!(headers[1], Some("1".to_owned()));
    // Third call (attempt 2): header = "2"
    assert_eq!(headers[2], Some("2".to_owned()));
}

/// Test: Retries exhausted returns Ok(Response) with final status, not Err.
#[tokio::test]
async fn test_retry_layer_exhausted_returns_ok_with_status() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct AlwaysFailService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for AlwaysFailService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                // Always return 500
                Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(make_response_body(b"error"))
                    .unwrap())
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = AlwaysFailService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        max_retries: 2,
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;

    // Retries exhausted: returns Ok(Response) with 500 status
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // 1 initial + 2 retries = 3 calls
    assert_eq!(*call_count.lock().unwrap(), 3);
}

/// Test: Non-retryable status (404) passes through immediately.
#[tokio::test]
async fn test_retry_layer_non_retryable_status_passes_through() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct NotFoundService {
        call_count: Arc<Mutex<usize>>,
    }

    impl Service<Request<Full<Bytes>>> for NotFoundService {
        type Response = Response<ResponseBody>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            let count = self.call_count.clone();
            Box::pin(async move {
                *count.lock().unwrap() += 1;
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(make_response_body(b"not found"))
                    .unwrap())
            })
        }
    }

    let call_count = Arc::new(Mutex::new(0));
    let service = NotFoundService {
        call_count: call_count.clone(),
    };

    let retry_config = RetryConfig {
        max_retries: 3,
        backoff: ExponentialBackoff::fast(),
        ..RetryConfig::default()
    };
    let layer = RetryLayer::new(retry_config);
    let mut retry_service = layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = retry_service.call(req).await;

    // 404 is not retryable - passes through immediately
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Only 1 call (no retries)
    assert_eq!(*call_count.lock().unwrap(), 1);
}

use super::*;

#[test]
fn test_default_retry_config() {
    let cfg = RpcRetryConfig::default();
    assert_eq!(cfg.max_retries, 3);
    assert_eq!(cfg.base_backoff, Duration::from_millis(100));
    assert_eq!(cfg.max_backoff, Duration::from_secs(5));
}

#[test]
fn test_retry_config_from_grpc_config() {
    let grpc_cfg = crate::client::GrpcClientConfig::new("test").with_max_retries(5);
    let retry_cfg = RpcRetryConfig::from(&grpc_cfg);

    assert_eq!(retry_cfg.max_retries, 5);
    assert_eq!(retry_cfg.base_backoff, grpc_cfg.base_backoff);
    assert_eq!(retry_cfg.max_backoff, grpc_cfg.max_backoff);
}

#[test]
fn test_retry_config_builder() {
    let cfg = RpcRetryConfig::new(10)
        .with_base_backoff(Duration::from_millis(200))
        .with_max_backoff(Duration::from_secs(10));

    assert_eq!(cfg.max_retries, 10);
    assert_eq!(cfg.base_backoff, Duration::from_millis(200));
    assert_eq!(cfg.max_backoff, Duration::from_secs(10));
}

#[tokio::test]
async fn test_call_with_retry_succeeds_first_attempt() {
    struct MockClient;

    let mut client = MockClient;
    let cfg = Arc::new(RpcRetryConfig::default());

    let result = call_with_retry(
        &mut client,
        cfg,
        "test_request".to_owned(),
        |_c, req| async move { Ok::<_, Status>(format!("response: {req}")) },
        "test.op",
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "response: test_request");
}

#[tokio::test]
async fn test_call_with_retry_non_retryable_error() {
    struct MockClient;

    let mut client = MockClient;
    let cfg = Arc::new(RpcRetryConfig::new(3));

    let result = call_with_retry(
        &mut client,
        cfg,
        (),
        |_c, _req| async move { Err::<String, _>(Status::invalid_argument("bad request")) },
        "test.op",
    )
    .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::InvalidArgument);
}

#[tokio::test]
async fn test_call_with_retry_retries_on_unavailable() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockClient {
        call_count: Arc<AtomicU32>,
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let mut client = MockClient {
        call_count: call_count.clone(),
    };

    let cfg = Arc::new(
        RpcRetryConfig::new(3)
            .with_base_backoff(Duration::from_millis(1))
            .with_max_backoff(Duration::from_millis(10)),
    );

    let result = call_with_retry(
        &mut client,
        cfg,
        (),
        |c, _req| {
            let count = c.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if count < 3 {
                    Err(Status::unavailable("temporarily unavailable"))
                } else {
                    Ok("success".to_owned())
                }
            }
        },
        "test.op",
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "success");
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_call_with_retry_gives_up_after_max_retries() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct MockClient {
        call_count: Arc<AtomicU32>,
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let mut client = MockClient {
        call_count: call_count.clone(),
    };

    let cfg = Arc::new(
        RpcRetryConfig::new(2)
            .with_base_backoff(Duration::from_millis(1))
            .with_max_backoff(Duration::from_millis(10)),
    );

    let result = call_with_retry(
        &mut client,
        cfg,
        (),
        |c, _req| {
            c.call_count.fetch_add(1, Ordering::SeqCst);
            async move { Err::<String, _>(Status::unavailable("always unavailable")) }
        },
        "test.op",
    )
    .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code(), Code::Unavailable);
    // Initial attempt + 2 retries = 3 total calls
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_call_with_retry_respects_max_backoff() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    struct MockClient {
        call_count: Arc<AtomicU32>,
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let mut client = MockClient {
        call_count: call_count.clone(),
    };

    // Set base_backoff high enough that without max_backoff cap,
    // total time would be much longer
    let cfg = Arc::new(
        RpcRetryConfig::new(2)
            .with_base_backoff(Duration::from_millis(100))
            .with_max_backoff(Duration::from_millis(50)),
    );

    let start = Instant::now();
    _ = call_with_retry(
        &mut client,
        cfg,
        (),
        |c, _req| {
            c.call_count.fetch_add(1, Ordering::SeqCst);
            async move { Err::<String, _>(Status::unavailable("unavailable")) }
        },
        "test.op",
    )
    .await;
    let elapsed = start.elapsed();

    // With max_backoff of 50ms and 2 retries, total backoff should be ~100ms max
    // (50ms + 50ms, since both attempts would hit the cap)
    // Without cap: 100ms + 200ms = 300ms
    assert!(
        elapsed < Duration::from_millis(200),
        "Backoff should be capped; elapsed: {elapsed:?}"
    );
}

// Created: 2026-04-07 by Constructor Tech
#[test]
fn parse_alias_and_suffix() {
    let path = "/oagw/v1/proxy/api.openai.com/v1/chat/completions";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "api.openai.com");
    assert_eq!(suffix, "/v1/chat/completions");
}

#[test]
fn parse_alias_with_port() {
    let path = "/oagw/v1/proxy/host:8443/path";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "host:8443");
    assert_eq!(suffix, "/path");
}

#[test]
fn parse_alias_no_suffix() {
    let path = "/oagw/v1/proxy/api.openai.com";
    let prefix = "/oagw/v1/proxy/";
    let remaining = path.strip_prefix(prefix).unwrap();
    let (alias, suffix) = match remaining.find('/') {
        Some(pos) => (&remaining[..pos], &remaining[pos..]),
        None => (remaining, ""),
    };
    assert_eq!(alias, "api.openai.com");
    assert_eq!(suffix, "");
}

#[test]
fn parse_query_params() {
    let query = "version=2&model=gpt-4";
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0], ("version".into(), "2".into()));
    assert_eq!(params[1], ("model".into(), "gpt-4".into()));
}

#[test]
fn percent_encoded_param_name_decoded() {
    let query = "my%20key=value";
    let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], ("my key".into(), "value".into()));
}

mod cancelled_uploads {
    use futures_util::StreamExt as _;
    use std::sync::Arc;
    use std::time::Duration;

    use tower::ServiceExt as _;

    use super::super::client_gone;
    use crate::infra::proxy::service::tests::RequestRecorder;
    use crate::test_support::AppHarness;

    fn io_error(kind: std::io::ErrorKind) -> axum::Error {
        axum::Error::new(std::io::Error::new(kind, "test"))
    }

    #[test]
    fn a_closed_or_reset_connection_is_the_client_gone() {
        use std::io::ErrorKind;
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
        ] {
            assert!(client_gone(&io_error(kind)), "{kind:?}");
        }
        assert!(!client_gone(&io_error(ErrorKind::InvalidData)));
        assert!(!client_gone(&axum::Error::new("bad chunk")));
    }

    /// An HTTP/2 stream the client cancelled is the client gone; a protocol
    /// error is not. `h2` must be the version hyper uses, or the downcast in
    /// `client_gone` never matches.
    #[test]
    fn a_cancelled_http2_stream_is_the_client_gone() {
        let h2_error = |reason| axum::Error::new(h2::Error::from(reason));
        assert!(client_gone(&h2_error(h2::Reason::CANCEL)));
        assert!(!client_gone(&h2_error(h2::Reason::PROTOCOL_ERROR)));
    }

    async fn harness() -> (AppHarness, Arc<RequestRecorder>) {
        let metrics = Arc::new(RequestRecorder::default());
        let h = AppHarness::builder()
            .with_metrics(metrics.clone())
            .build()
            .await;
        (h, metrics)
    }

    /// A client gone mid-upload is counted as `Cancelled`, not as a 400.
    #[tokio::test]
    async fn a_client_gone_mid_upload_is_counted_as_cancelled() {
        let (h, metrics) = harness().await;
        let body = axum::body::Body::from_stream(futures_util::stream::iter([
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial ")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "client went away",
            )),
        ]));
        let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
            .body(body)
            .unwrap();
        let resp = h.router().clone().oneshot(request).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );
    }

    /// A handler dropped while it reads the body (the gateway's request
    /// timeout, a shutdown) is counted as `Cancelled`.
    #[tokio::test]
    async fn a_handler_dropped_mid_upload_is_counted_as_cancelled() {
        let (h, metrics) = harness().await;
        let body = axum::body::Body::from_stream(
            futures_util::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from_static(
                b"partial ",
            ))])
            .chain(futures_util::stream::pending()),
        );
        let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
            .body(body)
            .unwrap();
        let timed_out = tokio::time::timeout(
            Duration::from_millis(50),
            h.router().clone().oneshot(request),
        )
        .await;
        assert!(timed_out.is_err());
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );
    }
}

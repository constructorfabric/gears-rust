use super::*;

const URI: &str = "/alias/v1/upload";

fn stream(items: Vec<Result<&'static [u8], BoxError>>) -> BodyStream {
    Box::pin(futures_util::stream::iter(
        items.into_iter().map(|i| i.map(Bytes::from_static)),
    ))
}

async fn buffer(
    items: Vec<Result<&'static [u8], BoxError>>,
    max_body: usize,
    declared: Option<u64>,
) -> (Result<Bytes, DomainError>, Arc<StreamedBodyState>) {
    let state = Arc::new(StreamedBodyState::default());
    let read = buffer_body_stream(stream(items), max_body, declared, Arc::clone(&state), URI).await;
    (read, state)
}

fn field_reason(err: &DomainError) -> (&'static str, &'static str) {
    match err {
        DomainError::Validation { field, reason, .. } => (field, reason),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_body_of_the_declared_length_is_read_whole() {
    for declared in [Some(11), None] {
        let (read, state) = buffer(vec![Ok(b"hello "), Ok(b""), Ok(b"world")], 64, declared).await;
        assert_eq!(read.unwrap(), "hello world");
        assert!(state.finished());
        assert!(state.finished_at().is_some());
    }
}

// The upstream's share of the deadline is measured from the end of the
// upload, so it is stamped when the body ends.
#[tokio::test(start_paused = true)]
async fn the_end_of_the_body_is_stamped() {
    let state = Arc::new(StreamedBodyState::default());
    let before = tokio::time::Instant::now();
    let body = stream(vec![Ok(b"hello")]).chain(futures_util::stream::once(async {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        Ok(Bytes::from_static(b" world"))
    }));
    let read = buffer_body_stream(Box::pin(body), 64, None, Arc::clone(&state), URI).await;
    assert_eq!(read.unwrap(), "hello world");
    let at = state.finished_at().unwrap();
    assert_eq!(at - before, std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn a_body_of_another_length_than_declared_is_invalid_content_length() {
    for (items, declared) in [
        (vec![Ok(&b"hello"[..])], 11),
        (vec![Ok(&b"hello "[..]), Ok(b"world"), Ok(b"!")], 11),
        (vec![Ok(&b"x"[..])], 0),
    ] {
        let (read, state) = buffer(items, 64, Some(declared)).await;
        let err = read.unwrap_err();
        assert_eq!(
            field_reason(&err),
            ("content-length", field::INVALID_CONTENT_LENGTH)
        );
        assert!(!state.finished());
        assert!(state.finished_at().is_none());
    }
}

#[tokio::test]
async fn a_body_over_the_limit_is_payload_too_large() {
    let (read, _) = buffer(vec![Ok(b"0123456789"), Ok(b"0123456789")], 15, None).await;
    assert!(
        matches!(read, Err(DomainError::PayloadTooLarge { .. })),
        "{read:?}"
    );
}

// A chunk past both the limit and the declared length is the size limit's
// 413, not a length mismatch; a later chunk past the limit also undoes the
// end the declared length marked.
#[tokio::test]
async fn a_chunk_over_the_limit_and_the_declared_length_is_payload_too_large() {
    const CHUNK: &[u8; 65] = &[b'x'; 65];
    for (items, max_body) in [
        (vec![Ok(&CHUNK[..])], 64),
        (vec![Ok(&b"hello world"[..]), Ok(b"!!!!!")], 15),
    ] {
        let (read, state) = buffer(items, max_body, Some(11)).await;
        assert!(
            matches!(read, Err(DomainError::PayloadTooLarge { .. })),
            "{read:?}"
        );
        assert!(!state.finished());
        assert!(state.finished_at().is_none());
    }
}

#[tokio::test]
async fn a_failed_stream_is_body_read_failed() {
    let (read, state) = buffer(vec![Ok(b"part"), Err("bad chunk".into())], 64, Some(11)).await;
    let err = read.unwrap_err();
    assert_eq!(field_reason(&err), ("body", field::BODY_READ_FAILED));
    assert!(!state.client_gone());
}

#[tokio::test]
async fn a_client_gone_mid_upload_is_marked() {
    let gone: BoxError = Box::new(axum::Error::new(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "client went away",
    )));
    let (read, state) = buffer(vec![Ok(b"part"), Err(client_read_error(gone))], 64, None).await;
    assert_eq!(
        field_reason(&read.unwrap_err()),
        ("body", field::BODY_READ_FAILED)
    );
    assert!(state.client_gone());
}

/// The same error from an SDK caller's own stream is not the client gone:
/// that caller still receives the response.
#[tokio::test]
async fn an_sdk_stream_error_is_never_the_client_gone() {
    for kind in [
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::UnexpectedEof,
    ] {
        let err: BoxError = Box::new(std::io::Error::new(kind, "source failed"));
        let (read, state) = buffer(vec![Ok(b"part"), Err(err)], 64, None).await;
        assert_eq!(
            field_reason(&read.unwrap_err()),
            ("body", field::BODY_READ_FAILED)
        );
        assert!(!state.client_gone(), "{kind:?}");
    }
}

/// Only a read error that means the client went away is marked.
#[test]
fn only_a_client_disconnect_is_marked() {
    let marked =
        |err: axum::Error| has_source::<ClientGone>(client_read_error(Box::new(err)).as_ref());
    assert!(marked(io_error(std::io::ErrorKind::ConnectionReset)));
    assert!(!marked(io_error(std::io::ErrorKind::InvalidData)));
    assert!(!marked(axum::Error::new("bad chunk")));
}

/// A limit enforced before the gateway (the API gateway's
/// `RequestBodyLimitLayer`) is reported as our own 413.
#[tokio::test]
async fn a_length_limit_below_the_gateway_is_payload_too_large() {
    use http_body_util::BodyExt as _;
    let limited = http_body_util::Limited::new(
        http_body_util::Full::new(Bytes::from_static(b"0123456789")),
        4,
    );
    let err = limited.collect().await.unwrap_err();
    let err: BoxError = Box::new(axum::Error::new(err));
    let (read, _) = buffer(vec![Err(err)], 64, None).await;
    assert!(
        matches!(read, Err(DomainError::PayloadTooLarge { .. })),
        "{read:?}"
    );
}

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

#[tokio::test]
async fn a_failure_after_bytes_went_upstream_says_so() {
    let detail = |err: DomainError| match err {
        DomainError::Validation { detail, .. } | DomainError::PayloadTooLarge { detail, .. } => {
            detail
        }
        other => panic!("unexpected {other:?}"),
    };
    let (read, state) = buffer(vec![Ok(b"part"), Err("bad chunk".into())], 64, None).await;
    assert!(!detail(read.unwrap_err()).contains("may have received"));
    state.handed_to_engine();
    let err = state.failure(64, URI).expect("the body failed");
    assert!(detail(err).contains("may have received part of the request"));
}

#[tokio::test]
async fn the_buffer_for_a_declared_length_starts_small() {
    // A huge declared length allocates nothing up front beyond 64 KiB; the
    // short body is then a length mismatch.
    let (read, _) = buffer(vec![Ok(b"tiny")], usize::MAX, Some(u64::MAX)).await;
    assert_eq!(
        field_reason(&read.unwrap_err()),
        ("content-length", field::INVALID_CONTENT_LENGTH)
    );
}

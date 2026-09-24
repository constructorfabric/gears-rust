#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};

use super::length_guard;

fn ok_stream(chunks: Vec<&'static [u8]>) -> BoxStream<'static, io::Result<Bytes>> {
    Box::pin(stream::iter(
        chunks.into_iter().map(|c| Ok(Bytes::from_static(c))),
    ))
}

/// Collects every `Ok` chunk (concatenated) plus, if the stream ends on an
/// `Err`, that error's kind. A stream that both yields data and then errors
/// (the overlong case) reports both.
///
/// Only re-polls after an `Err` (to confirm nothing follows it) -- `unfold`
/// (which [`length_guard`] is built on) panics if polled again after it has
/// already returned `Poll::Ready(None)`, so a clean end must not be re-polled.
async fn drain(mut s: BoxStream<'static, io::Result<Bytes>>) -> (Vec<u8>, Option<io::ErrorKind>) {
    let mut collected = Vec::new();
    let mut err_kind = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(e) => {
                err_kind = Some(e.kind());
                // Nothing must be yielded after an error.
                assert!(
                    s.next().await.is_none(),
                    "the stream must not yield anything after its terminal error"
                );
                break;
            }
        }
    }
    (collected, err_kind)
}

#[tokio::test]
async fn exact_length_completes_without_error() {
    let inner = ok_stream(vec![b"hello", b" ", b"world"]);
    let (collected, err) = drain(length_guard(inner, 11)).await;
    assert_eq!(collected, b"hello world");
    assert_eq!(
        err, None,
        "a stream of exactly the expected length must not error"
    );
}

#[tokio::test]
async fn shorter_than_expected_yields_unexpected_eof_at_the_end() {
    let inner = ok_stream(vec![b"hello"]);
    let (collected, err) = drain(length_guard(inner, 10)).await;
    assert_eq!(
        collected, b"hello",
        "the bytes that did arrive must still be yielded"
    );
    assert_eq!(
        err,
        Some(io::ErrorKind::UnexpectedEof),
        "a stream ending short of expected_len must surface UnexpectedEof"
    );
}

#[tokio::test]
async fn longer_than_expected_errors_and_withholds_the_excess() {
    let inner = ok_stream(vec![b"hello", b"world"]); // 10 bytes total
    let (collected, err) = drain(length_guard(inner, 7)).await;
    assert_eq!(
        collected, b"hellowo",
        "only the bytes within expected_len may ever be yielded"
    );
    assert_eq!(
        err,
        Some(io::ErrorKind::InvalidData),
        "exceeding expected_len must surface InvalidData, never a silently longer body"
    );
}

/// A chunk that lands exactly on the boundary (no partial truncation needed)
/// followed by more data must still error on the very next poll, with no
/// empty `Ok` in between.
#[tokio::test]
async fn longer_than_expected_with_chunk_boundary_aligned_errors_immediately() {
    let inner = ok_stream(vec![b"hello", b"world"]); // 5 + 5
    let (collected, err) = drain(length_guard(inner, 5)).await;
    assert_eq!(collected, b"hello");
    assert_eq!(err, Some(io::ErrorKind::InvalidData));
}

#[tokio::test]
async fn empty_stream_with_zero_expected_len_is_ok() {
    let inner = ok_stream(vec![]);
    let (collected, err) = drain(length_guard(inner, 0)).await;
    assert!(collected.is_empty());
    assert_eq!(
        err, None,
        "an empty stream matching expected_len == 0 must not error"
    );
}

#[tokio::test]
async fn empty_stream_with_nonzero_expected_len_errors() {
    let inner = ok_stream(vec![]);
    let (collected, err) = drain(length_guard(inner, 3)).await;
    assert!(collected.is_empty());
    assert_eq!(err, Some(io::ErrorKind::UnexpectedEof));
}

/// A read error from `inner` must pass through unchanged and end the stream,
/// regardless of how many (correct-looking) bytes preceded it.
#[tokio::test]
async fn inner_read_error_passes_through() {
    let inner: BoxStream<'static, io::Result<Bytes>> = Box::pin(stream::iter(vec![
        Ok(Bytes::from_static(b"partial")),
        Err(io::Error::new(io::ErrorKind::TimedOut, "boom")),
    ]));
    let (collected, err) = drain(length_guard(inner, 100)).await;
    assert_eq!(collected, b"partial");
    assert_eq!(err, Some(io::ErrorKind::TimedOut));
}

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};

use super::hashing_length_guard;
use crate::infra::content::hash;

fn ok_stream(chunks: Vec<&'static [u8]>) -> BoxStream<'static, io::Result<Bytes>> {
    Box::pin(stream::iter(
        chunks.into_iter().map(|c| Ok(Bytes::from_static(c))),
    ))
}

/// Collects every `Ok` chunk (concatenated) plus, if the stream ends on an
/// `Err`, that error's kind — mirrors `length_guard_tests::drain`.
async fn drain(mut s: BoxStream<'static, io::Result<Bytes>>) -> (Vec<u8>, Option<io::ErrorKind>) {
    let mut collected = Vec::new();
    let mut err_kind = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(e) => {
                err_kind = Some(e.kind());
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
async fn exact_length_publishes_the_correct_digest() {
    let inner = ok_stream(vec![b"hello", b" ", b"world"]);
    let (guarded, slot) = hashing_length_guard(inner, 11);
    let (collected, err) = drain(guarded).await;
    assert_eq!(collected, b"hello world");
    assert_eq!(err, None);

    let digest = slot
        .lock()
        .unwrap()
        .take()
        .expect("digest must be published once the stream completes at exactly expected_len");
    assert_eq!(
        digest.to_vec(),
        hash::sha256(b"hello world"),
        "the published digest must equal a plain whole-buffer sha256 of the same bytes"
    );
}

#[tokio::test]
async fn shorter_than_expected_errors_and_never_publishes_a_digest() {
    let inner = ok_stream(vec![b"hello"]);
    let (guarded, slot) = hashing_length_guard(inner, 10);
    let (collected, err) = drain(guarded).await;
    assert_eq!(collected, b"hello");
    assert_eq!(err, Some(io::ErrorKind::UnexpectedEof));
    assert!(
        slot.lock().unwrap().is_none(),
        "an undersized stream must never publish a digest \u{2014} the part must not be treated as fully uploaded"
    );
}

#[tokio::test]
async fn longer_than_expected_errors_and_never_publishes_a_digest() {
    let inner = ok_stream(vec![b"hello", b"world"]); // 10 bytes total
    let (guarded, slot) = hashing_length_guard(inner, 7);
    let (collected, err) = drain(guarded).await;
    assert_eq!(collected, b"hellowo");
    assert_eq!(err, Some(io::ErrorKind::InvalidData));
    assert!(
        slot.lock().unwrap().is_none(),
        "an oversized stream must never publish a digest \u{2014} the part must not be treated as fully uploaded"
    );
}

#[tokio::test]
async fn empty_stream_with_zero_expected_len_publishes_the_empty_digest() {
    let inner = ok_stream(vec![]);
    let (guarded, slot) = hashing_length_guard(inner, 0);
    let (collected, err) = drain(guarded).await;
    assert!(collected.is_empty());
    assert_eq!(err, None);
    assert_eq!(
        slot.lock().unwrap().take().unwrap().to_vec(),
        hash::sha256(b"")
    );
}

#[tokio::test]
async fn multi_chunk_stream_hashes_incrementally_to_the_same_digest_as_a_whole_buffer_hash() {
    let chunks: Vec<&'static [u8]> = vec![b"the ", b"quick ", b"brown ", b"fox"];
    let whole: Vec<u8> = chunks.concat();
    let inner = ok_stream(chunks);
    let (guarded, slot) = hashing_length_guard(inner, whole.len() as u64);
    let (collected, err) = drain(guarded).await;
    assert_eq!(collected, whole);
    assert_eq!(err, None);
    assert_eq!(
        slot.lock().unwrap().take().unwrap().to_vec(),
        hash::sha256(&whole)
    );
}

/// The regression this module's doc comment describes: a consumer that
/// stops polling the instant the boundary chunk is yielded (never asking
/// for a trailing `None`, exactly what an HTTP client sending a fixed
/// `Content-Length` body may do) must already see the digest published —
/// waiting for one more poll would mean it never publishes at all against
/// such a consumer.
#[tokio::test]
async fn digest_is_available_immediately_after_the_boundary_chunk_without_a_trailing_poll() {
    let inner = ok_stream(vec![b"hello", b"world"]); // exactly 10 bytes
    let (mut guarded, slot) = hashing_length_guard(inner, 10);

    let first = guarded.next().await.expect("first chunk").expect("ok");
    assert_eq!(first, Bytes::from_static(b"hello"));
    assert!(
        slot.lock().unwrap().is_none(),
        "must not publish before expected_len is actually reached"
    );

    let second = guarded.next().await.expect("second chunk").expect("ok");
    assert_eq!(second, Bytes::from_static(b"world"));
    // No further `.next()` call here -- this is the point: the digest must
    // already be published without ever driving the stream to a trailing
    // `None`.
    assert_eq!(
        slot.lock().unwrap().take().unwrap().to_vec(),
        hash::sha256(b"helloworld")
    );
}

/// A chunk that lands exactly on the boundary followed by *more* data (a
/// caller that keeps polling past where an HTTP client under fixed
/// `Content-Length` framing would have stopped) must still end up rejected,
/// with the speculatively-published digest revoked back to `None` -- never
/// left in a state where a caller could observe a "published" digest for a
/// part that turned out to be oversized.
#[tokio::test]
async fn oversize_discovered_after_the_boundary_revokes_the_published_digest() {
    let inner = ok_stream(vec![b"hello", b"world"]);
    let (guarded, slot) = hashing_length_guard(inner, 5);
    let (collected, err) = drain(guarded).await;
    assert_eq!(
        collected, b"hello",
        "no bytes past the boundary may ever be yielded"
    );
    assert_eq!(err, Some(io::ErrorKind::InvalidData));
    assert!(
        slot.lock().unwrap().is_none(),
        "a digest published speculatively at the boundary must be revoked once more data turns up"
    );
}

#[tokio::test]
async fn inner_read_error_passes_through_and_never_publishes_a_digest() {
    let inner: BoxStream<'static, io::Result<Bytes>> = Box::pin(stream::iter(vec![
        Ok(Bytes::from_static(b"partial")),
        Err(io::Error::new(io::ErrorKind::TimedOut, "boom")),
    ]));
    let (guarded, slot) = hashing_length_guard(inner, 100);
    let (collected, err) = drain(guarded).await;
    assert_eq!(collected, b"partial");
    assert_eq!(err, Some(io::ErrorKind::TimedOut));
    assert!(slot.lock().unwrap().is_none());
}

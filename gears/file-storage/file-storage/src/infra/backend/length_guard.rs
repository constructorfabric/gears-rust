//! Enforces that a byte stream yields exactly the length its caller already
//! committed to (e.g. an already-sent `Content-Length` header) before it ends
//! successfully.
//!
//! [`S3Backend`](super::S3Backend)'s `get_stream`/`get_range_stream` compare
//! `expected_len` against a response's `Content-Length` header when the
//! upstream response carries one, but that check is skippable: a
//! chunked-transfer-encoded response (no `Content-Length` at all) forwards
//! its raw `bytes_stream()` with no length verification whatsoever, so a
//! truncated or grown object served that way would otherwise reach the
//! sidecar's client as an apparently successful response under an already-set
//! (and now wrong) `Content-Length`. [`length_guard`] closes that
//! unconditionally, regardless of whether the upstream response carried a
//! `Content-Length` -- the header check remains a cheap up-front rejection
//! (refuses before a single byte is streamed when it fires at all), this
//! guard is what actually holds for every response shape.

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::io;

/// Wraps `inner` so it yields exactly `expected_len` bytes total before
/// ending, erroring instead of ever completing successfully at a different
/// length.
///
/// - Fewer bytes than `expected_len` (`inner` ends before that many bytes were
///   seen): the stream's final item is
///   `Err(io::Error::new(io::ErrorKind::UnexpectedEof, _))` in place of the
///   `None` that would otherwise end it cleanly.
/// - More bytes than `expected_len`: the chunk that would cross the boundary
///   is truncated to the boundary -- so the stream never yields a single byte
///   past `expected_len` -- and the *next* poll yields
///   `Err(io::Error::new(io::ErrorKind::InvalidData, _))`. A chunk that lands
///   exactly on the boundary with nothing left over yields that error
///   immediately instead of an empty `Ok`.
/// - A read error from `inner` is passed through unchanged and ends the
///   stream (no further polls).
///
/// `expected_len == 0` with an `inner` that ends immediately yields no items
/// at all -- a correct, empty stream, not an error.
pub fn length_guard(
    inner: BoxStream<'static, io::Result<Bytes>>,
    expected_len: u64,
) -> BoxStream<'static, io::Result<Bytes>> {
    /// Internal `unfold` state. `Pending` holds an error already decided on a
    /// previous poll (the overlong-chunk case truncates and yields data
    /// *this* poll, then must still surface the error on the *next* one).
    enum State {
        Reading {
            inner: BoxStream<'static, io::Result<Bytes>>,
            seen: u64,
        },
        Pending(io::Error),
        Done,
    }

    let stream = futures::stream::unfold(
        State::Reading { inner, seen: 0 },
        move |state| async move {
            match state {
                State::Done => None,
                State::Pending(e) => Some((Err(e), State::Done)),
                State::Reading { mut inner, seen } => match inner.next().await {
                    None => {
                        if seen == expected_len {
                            None
                        } else {
                            let e = io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                format!(
                                    "stream ended {} byte(s) short of the expected {expected_len} byte(s)",
                                    expected_len - seen
                                ),
                            );
                            Some((Err(e), State::Done))
                        }
                    }
                    Some(Err(e)) => Some((Err(e), State::Done)),
                    Some(Ok(chunk)) => {
                        let n = chunk.len() as u64;
                        let new_seen = seen + n;
                        if new_seen <= expected_len {
                            Some((
                                Ok(chunk),
                                State::Reading {
                                    inner,
                                    seen: new_seen,
                                },
                            ))
                        } else {
                            let over_err = io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "stream exceeded the expected {expected_len} byte(s) (at least {new_seen} byte(s) seen)"
                                ),
                            );
                            // `allowed` bytes of this chunk are still within
                            // the promised length; the rest must never reach
                            // the caller.
                            let allowed = usize::try_from(expected_len - seen).unwrap_or(0);
                            if allowed == 0 {
                                Some((Err(over_err), State::Done))
                            } else {
                                let truncated = chunk.slice(0..allowed);
                                Some((Ok(truncated), State::Pending(over_err)))
                            }
                        }
                    }
                },
            }
        },
    );
    Box::pin(stream)
}

#[cfg(test)]
#[path = "length_guard_tests.rs"]
mod length_guard_tests;

//! Wraps a byte stream so it both enforces [`length_guard`](super::length_guard)'s
//! exact-length contract (see that module's doc comment for the precise
//! truncation/mismatch semantics — identical here) and accumulates the
//! SHA-256 digest of the bytes it forwards, publishing the finished digest
//! the moment the stream has yielded exactly `expected_len` bytes.
//!
//! Combining the two in one pass matters for
//! [`S3Backend::upload_part_stream`](super::S3Backend): the part's SHA-256
//! (this gear's own hash, not S3's MD5-based `ETag`) must be known without
//! ever buffering the whole part just to compute it separately — hashing
//! happens on exactly the same pass that streams the part's bytes to S3 as
//! the request body.
//!
//! ## Why the digest publishes *at* the boundary, not after a trailing `None`
//! [`length_guard`] (the plain, non-hashing sibling this mirrors) only
//! disambiguates "clean end" from "more data follows" on the poll *after*
//! the chunk that reaches `expected_len` — appropriate there, since its
//! caller (`axum::body::Body::from_stream`, for a response body) always
//! drives a stream all the way to a `None` poll. A **request** body handed
//! to `reqwest::Body::wrap_stream` under a fixed `Content-Length` is not
//! guaranteed to get that extra poll: once the HTTP client has written
//! exactly `Content-Length` bytes, it may consider the body fully sent and
//! never poll for a terminating `None` at all — confirmed empirically
//! against `s3s-fs` (the digest never published if publication waited for
//! that extra poll, even though the `UploadPart` itself succeeded). So the
//! digest here is published *speculatively* the instant the running count
//! reaches `expected_len`, and only **revoked** (slot cleared back to
//! `None`, stream errors) if a caller keeps polling past that point and
//! more data turns up — the "oversize discovered only on the very next
//! poll" case `length_guard`'s own boundary-aligned test exercises. A
//! consumer that (like `reqwest`) stops polling right at the boundary keeps
//! the published digest; one that (like this module's own tests) keeps
//! draining the stream to `None` still gets the same acceptance, and one
//! that turns out to have more data still gets the same rejection
//! `length_guard` would give, just with the digest slot cleaned back up
//! first.

use std::io;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::infra::content::hash::{Hasher, digest_to_array};

/// Where [`hashing_length_guard`] publishes the finished digest once its
/// stream has yielded exactly `expected_len` bytes. Stays `None` (or is
/// cleared back to `None`) on every error path (mismatched length, an
/// upstream read failure, or oversize data discovered after the digest was
/// already speculatively published) — so a caller must only trust a
/// populated digest once it has *also* observed the wrapped stream's
/// consumer (e.g. the HTTP client that sent it as a request body) succeed
/// end-to-end.
pub type DigestSlot = Arc<Mutex<Option<[u8; 32]>>>;

/// See the module doc comment. `inner` must yield exactly `expected_len`
/// bytes total before ending, or the returned stream errors exactly like
/// [`length_guard`](super::length_guard) — see its doc comment for the
/// undersized/oversized/exact-boundary cases, all identical here (modulo
/// *when* the digest is published — see this module's own doc comment).
pub fn hashing_length_guard(
    inner: BoxStream<'static, io::Result<Bytes>>,
    expected_len: u64,
) -> (BoxStream<'static, io::Result<Bytes>>, DigestSlot) {
    /// Internal `unfold` state — mirrors [`length_guard`](super::length_guard)'s
    /// own, plus a running hasher carried alongside the inner stream while
    /// still below `expected_len`.
    enum State {
        Reading {
            inner: BoxStream<'static, io::Result<Bytes>>,
            hasher: Hasher,
        },
        /// Exactly `expected_len` bytes have been seen and the digest was
        /// already speculatively published into the shared slot. Still
        /// watching `inner` in case a caller keeps polling past the
        /// boundary and more data turns up — see the module doc comment.
        AtBoundary {
            inner: BoxStream<'static, io::Result<Bytes>>,
        },
        Pending(io::Error),
        Done,
    }

    let digest_slot: DigestSlot = Arc::new(Mutex::new(None));
    let out_slot = Arc::clone(&digest_slot);

    let stream = futures::stream::unfold(
        State::Reading {
            inner,
            hasher: Hasher::new(),
        },
        move |state| {
            let digest_slot = Arc::clone(&digest_slot);
            async move {
                match state {
                    State::Done => None,
                    State::Pending(e) => Some((Err(e), State::Done)),
                    State::AtBoundary { mut inner } => match inner.next().await {
                        // The common case for a caller that stops driving
                        // the stream right at the boundary (e.g. an HTTP
                        // client under a fixed Content-Length): nothing more
                        // to do, the digest already published stands.
                        None => None,
                        // The stream misbehaved after exhausting its
                        // promised length -- revoke the speculative digest
                        // so a caller can never observe a "published" digest
                        // for a part that turned out to be oversized.
                        Some(Err(e)) => {
                            if let Ok(mut slot) = digest_slot.lock() {
                                *slot = None;
                            }
                            Some((Err(e), State::Done))
                        }
                        Some(Ok(_chunk)) => {
                            if let Ok(mut slot) = digest_slot.lock() {
                                *slot = None;
                            }
                            let over_err = io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "stream exceeded the expected {expected_len} byte(s) after \
                                     already reaching it exactly"
                                ),
                            );
                            Some((Err(over_err), State::Done))
                        }
                    },
                    State::Reading {
                        mut inner,
                        mut hasher,
                    } => match inner.next().await {
                        None => {
                            let seen = hasher.len();
                            if seen == expected_len {
                                let digest = digest_to_array(hasher.finalize());
                                if let Ok(mut slot) = digest_slot.lock() {
                                    *slot = Some(digest);
                                }
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
                            let seen = hasher.len();
                            let n = chunk.len() as u64;
                            let new_seen = seen + n;
                            match new_seen.cmp(&expected_len) {
                                std::cmp::Ordering::Less => {
                                    hasher.update(&chunk);
                                    Some((Ok(chunk), State::Reading { inner, hasher }))
                                }
                                std::cmp::Ordering::Equal => {
                                    // Publish speculatively right now -- see
                                    // the module doc comment for why this
                                    // cannot wait for a subsequent poll.
                                    hasher.update(&chunk);
                                    let digest = digest_to_array(hasher.finalize());
                                    if let Ok(mut slot) = digest_slot.lock() {
                                        *slot = Some(digest);
                                    }
                                    Some((Ok(chunk), State::AtBoundary { inner }))
                                }
                                std::cmp::Ordering::Greater => {
                                    let over_err = io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        format!(
                                            "stream exceeded the expected {expected_len} byte(s) (at least {new_seen} byte(s) seen)"
                                        ),
                                    );
                                    // `allowed` bytes of this chunk are still
                                    // within the promised length; the rest
                                    // must never reach the caller (or the
                                    // hasher).
                                    let allowed = usize::try_from(expected_len - seen).unwrap_or(0);
                                    if allowed == 0 {
                                        Some((Err(over_err), State::Done))
                                    } else {
                                        let truncated = chunk.slice(0..allowed);
                                        hasher.update(&truncated);
                                        Some((Ok(truncated), State::Pending(over_err)))
                                    }
                                }
                            }
                        }
                    },
                }
            }
        },
    );
    (Box::pin(stream), out_slot)
}

#[cfg(test)]
#[path = "hashing_length_guard_tests.rs"]
mod hashing_length_guard_tests;

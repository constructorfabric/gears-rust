//! Streamed request bodies: the size limit, the declared length, and what
//! a failure on the caller's side of the body means for the request.

use std::error::Error as StdError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt as _;
use oagw_sdk::body::{BodyStream, BoxError};
use oagw_sdk::field;

use crate::domain::error::DomainError;

/// Largest initial buffer for a body read whole.
const INITIAL_BUFFER_CAPACITY: usize = 64 * 1024;

/// What happened on our side of a streamed request body.
#[derive(Default)]
pub(super) struct StreamedBodyState {
    /// Total bytes when the limit was breached.
    exceeded: OnceLock<usize>,
    /// Bytes seen when the body turned out longer or shorter than its
    /// declared length, and that length.
    mismatched: OnceLock<(usize, u64)>,
    /// Why the caller's body stream failed.
    failed: OnceLock<String>,
    /// The stream failed because the REST client went away.
    client_gone: AtomicBool,
    /// The caller's body stream ended cleanly, or yielded all its declared
    /// bytes.
    finished: AtomicBool,
    /// When the body first counted as finished: the upstream's share of the
    /// request deadline starts there.
    finished_at: OnceLock<tokio::time::Instant>,
    /// A byte of the body went to the engine: the upstream may have
    /// received the head and part of the body.
    handed: AtomicBool,
}

impl StreamedBodyState {
    pub(super) fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// When the caller's body finished, if it did.
    pub(super) fn finished_at(&self) -> Option<tokio::time::Instant> {
        self.finished()
            .then(|| self.finished_at.get().copied())
            .flatten()
    }

    fn mark_finished(&self) {
        let _first = self.finished_at.set(tokio::time::Instant::now());
        self.finished.store(true, Ordering::Release);
    }

    /// Record that a chunk of the body went to the engine.
    pub(super) fn handed_to_engine(&self) {
        self.handed.store(true, Ordering::Release);
    }

    /// The REST client went away mid-upload: nobody gets the answer or the
    /// error, and the request counts as cancelled. Only the REST handler,
    /// which owns the client connection, marks it (see
    /// [`client_read_error`]); an SDK caller's stream error never does, as
    /// that caller still receives the response.
    pub(super) fn client_gone(&self) -> bool {
        self.client_gone.load(Ordering::Acquire)
    }

    /// The error the request fails with, if the body failed on our side.
    ///
    /// The upstream never gets a complete request then, but once a byte
    /// went to the engine it may have received the head and part of the
    /// body; the detail says so.
    pub(super) fn failure(&self, max_body: usize, instance_uri: &str) -> Option<DomainError> {
        let partly_sent = if self.handed.load(Ordering::Acquire) {
            "; the upstream may have received part of the request"
        } else {
            ""
        };
        if let Some(total) = self.exceeded.get() {
            return Some(DomainError::PayloadTooLarge {
                detail: format!(
                    "streaming request body of {total} bytes exceeds maximum of {max_body} \
                     bytes{partly_sent}"
                ),
                instance: instance_uri.to_string(),
            });
        }
        // The caller's stream failed (the client aborted its upload, an SDK
        // stream returned an error): the caller's fault.
        if let Some(reason) = self.failed.get() {
            tracing::debug!(reason, "reading the streamed request body failed");
            return Some(DomainError::Validation {
                field: "body",
                reason: field::BODY_READ_FAILED,
                detail: format!("failed to read the request body{partly_sent}"),
                instance: instance_uri.to_string(),
            });
        }
        self.mismatched.get().map(|&(seen, declared)| {
            let detail = if seen as u64 > declared {
                format!(
                    "request body is longer than its Content-Length of {declared} \
                     bytes{partly_sent}"
                )
            } else {
                format!(
                    "request body ended after {seen} of its Content-Length of {declared} \
                     bytes{partly_sent}"
                )
            };
            DomainError::Validation {
                field: "content-length",
                reason: field::INVALID_CONTENT_LENGTH,
                detail,
                instance: instance_uri.to_string(),
            }
        })
    }

    fn stream_failed(&self, err: &(dyn StdError + 'static), total: usize) {
        // A body limit enforced before the gateway (the API gateway's
        // `RequestBodyLimitLayer`) is the same 413 as ours.
        if has_source::<http_body_util::LengthLimitError>(err) {
            let _ = self.exceeded.set(total);
            return;
        }
        if has_source::<ClientGone>(err) {
            self.client_gone.store(true, Ordering::Release);
        }
        let _ = self.failed.set(format!("body stream read error: {err}"));
    }
}

/// Enforce `max_body` and the `declared` length on a streamed request body,
/// recording failures and the clean end in `state`. Empty chunks are
/// dropped. A breach or a stream error ends the stream with an error, which
/// aborts the upstream request without completing its framing. A stream
/// that ends short of `declared` is recorded here; the engine's sized body
/// fails the request.
pub(super) fn limit_body_stream(
    stream: BodyStream,
    max_body: usize,
    declared: Option<u64>,
    state: Arc<StreamedBodyState>,
) -> impl futures_util::Stream<Item = Result<Bytes, BoxError>> + Send {
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_at_end = Arc::clone(&seen);
    let end = Arc::clone(&state);
    let limited = stream.filter_map(move |chunk| {
        let total = seen.load(Ordering::Relaxed);
        let item = match chunk {
            Ok(bytes) if bytes.is_empty() => None,
            Ok(bytes) => {
                let total = total.saturating_add(bytes.len());
                seen.store(total, Ordering::Relaxed);
                // The size limit first: a chunk past both it and the
                // declared length is a 413, not a length mismatch.
                if total > max_body {
                    tracing::warn!(
                        total_bytes = total,
                        max_body,
                        "streaming body exceeded max size, aborting"
                    );
                    let _ = state.exceeded.set(total);
                    state.finished.store(false, Ordering::Release);
                    Some(Err("request body exceeds the configured limit".into()))
                } else if let Some(len) = declared
                    && total as u64 > len
                {
                    let _ = state.mismatched.set((total, len));
                    state.finished.store(false, Ordering::Release);
                    Some(Err("request body exceeds its declared length".into()))
                } else {
                    // All declared bytes are in: the stream may stay open
                    // (the engine's sized body stops reading here).
                    if declared == Some(total as u64) {
                        state.mark_finished();
                    }
                    Some(Ok(bytes))
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "body stream chunk error");
                state.stream_failed(e.as_ref(), total);
                Some(Err(e))
            }
        };
        std::future::ready(item)
    });
    // Polled only once the caller's stream has ended. After an error item
    // the body state reports the failure first, whatever this says.
    let end = futures_util::stream::once(async move {
        let total = seen_at_end.load(Ordering::Relaxed);
        if let Some(len) = declared
            && (total as u64) < len
        {
            let _ = end.mismatched.set((total, len));
            return;
        }
        end.mark_finished();
    })
    .filter_map(|()| std::future::ready(None));
    limited.chain(end)
}

/// Read a streamed request body whole, for a stage that needs its bytes,
/// with the limits and errors of [`limit_body_stream`].
pub(super) async fn buffer_body_stream(
    stream: BodyStream,
    max_body: usize,
    declared: Option<u64>,
    state: Arc<StreamedBodyState>,
    instance_uri: &str,
) -> Result<Bytes, DomainError> {
    let limited = limit_body_stream(stream, max_body, declared, Arc::clone(&state));
    let mut limited = std::pin::pin!(limited);
    // The declared length is the client's word: start small and grow.
    let capacity = declared.map_or(0, |len| {
        usize::try_from(len)
            .unwrap_or(0)
            .min(max_body)
            .min(INITIAL_BUFFER_CAPACITY)
    });
    let mut buf = BytesMut::with_capacity(capacity);
    while let Some(chunk) = limited.next().await {
        match chunk {
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    match state.failure(max_body, instance_uri) {
        Some(err) => Err(err),
        None => Ok(buf.freeze()),
    }
}

/// A REST client's body read failed because its connection went away.
///
/// Only the REST handler wraps errors in it: there the failed read is the
/// client connection itself. The same error kinds from an SDK caller's
/// stream (an `UnexpectedEof` from its own source) say nothing about the
/// caller, who still receives the response.
#[derive(Debug)]
struct ClientGone(BoxError);

impl std::fmt::Display for ClientGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl StdError for ClientGone {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.0.as_ref())
    }
}

/// The error a failed read of the REST client's request body passes on:
/// marked as the client gone when the connection closed or reset, or the
/// HTTP/2 stream was cancelled.
pub(crate) fn client_read_error(err: BoxError) -> BoxError {
    if client_gone(err.as_ref()) {
        Box::new(ClientGone(err))
    } else {
        err
    }
}

/// Whether a failed body read means the client went away mid-upload: the
/// connection closed or reset, or the HTTP/2 stream cancelled.
fn client_gone(err: &(dyn StdError + 'static)) -> bool {
    let mut source: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(cause) = source {
        if let Some(e) = cause.downcast_ref::<hyper::Error>()
            && (e.is_incomplete_message() || e.is_canceled())
        {
            return true;
        }
        if let Some(e) = cause.downcast_ref::<h2::Error>()
            && (e.reason() == Some(h2::Reason::CANCEL) || e.is_go_away() || e.is_io())
        {
            return true;
        }
        if let Some(e) = cause.downcast_ref::<std::io::Error>()
            && matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            )
        {
            return true;
        }
        source = cause.source();
    }
    false
}

fn has_source<T: StdError + 'static>(err: &(dyn StdError + 'static)) -> bool {
    let mut source = Some(err);
    while let Some(cause) = source {
        if cause.is::<T>() {
            return true;
        }
        source = cause.source();
    }
    false
}

#[cfg(test)]
#[path = "request_body_tests.rs"]
mod request_body_tests;

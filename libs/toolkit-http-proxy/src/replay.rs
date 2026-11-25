//! A bounded copy of a streamed request body, kept so the engine can resend
//! the request (see [`replay_tee`]).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt as _;
use parking_lot::Mutex;

use crate::body::{BoxError, ProxyBody};
use crate::error::{ErrorKind, ProxyError};

/// Wrap a streamed `body` so the bytes it yields are also kept, up to
/// `limit` bytes, for a resend. Attach the returned [`ReplayTee`] to the
/// request's extensions.
///
/// The body streams as before; nothing waits for the copy. If the engine
/// resends the request (see [`crate::ProxyClient::send`]), the resend sends
/// the copy and then the rest of the caller's body, which the first attempt
/// had not read yet. The rest is read from the same body, so any length
/// check in it (e.g. [`crate::sized_stream_body`]) still covers the whole
/// body. Once the body has yielded more than `limit` bytes, or a trailers
/// frame, or an error, the copy is dropped and its memory freed: the request
/// is no longer resendable. It is also dropped when `send` returns, and,
/// before the upload starts, whenever `send` cannot resend from it: on
/// HTTP/1.1 unless the connection is reused and the method idempotent, for
/// a request declaring `Trailer`, an empty body or an attached
/// [`crate::ReplayableBody`], and on a second attempt.
#[must_use]
pub fn replay_tee(body: ProxyBody, limit: usize) -> (ProxyBody, ReplayTee) {
    let shared = Arc::new(Mutex::new(Shared {
        body: Some(body),
        copy: Some(Vec::new()),
        ended: false,
        limit,
    }));
    let tee = Tee {
        shared: Arc::clone(&shared),
    };
    (tee.boxed_unsync(), ReplayTee { shared })
}

/// Request extension: the copy a [`replay_tee`] body keeps for a resend.
/// Cloning shares the copy.
#[derive(Clone)]
pub struct ReplayTee {
    shared: Arc<Mutex<Shared>>,
}

impl ReplayTee {
    /// Memory the copy currently holds, in bytes (its buffer's capacity).
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.shared.lock().copy.as_ref().map_or(0, Vec::capacity)
    }

    /// Whether a resend is still possible: the copy holds every byte the
    /// body has yielded so far.
    #[must_use]
    pub fn is_replayable(&self) -> bool {
        let shared = self.shared.lock();
        shared.copy.is_some() && shared.body.is_some()
    }

    /// The body for a resend: the copy followed by the rest of the caller's
    /// body, which the first attempt's body gives up (it fails if polled
    /// again). `None` once the copy is gone.
    pub(crate) fn resume(&self) -> Option<ProxyBody> {
        let mut shared = self.shared.lock();
        let copy = shared.copy.take()?;
        let body = shared.body.take()?;
        let rest = (!shared.ended).then_some(body);
        Some(
            Resumed {
                copy: (!copy.is_empty()).then(|| Bytes::from(copy)),
                rest,
            }
            .boxed_unsync(),
        )
    }

    /// No resend will happen: free the copy and stop copying.
    pub(crate) fn release(&self) {
        self.shared.lock().copy = None;
    }
}

impl std::fmt::Debug for ReplayTee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayTee")
            .field("retained_bytes", &self.retained_bytes())
            .finish_non_exhaustive()
    }
}

struct Shared {
    /// The caller's body; taken by a resend.
    body: Option<ProxyBody>,
    /// What the body yielded so far; `None` once it cannot be complete.
    /// Grown by hand so its capacity never exceeds `limit`.
    copy: Option<Vec<u8>>,
    /// The body returned its end.
    ended: bool,
    limit: usize,
}

impl Shared {
    fn record(&mut self, frame: &Frame<Bytes>) {
        let Some(copy) = self.copy.as_mut() else {
            return;
        };
        match frame.data_ref() {
            Some(data) if copy.len().saturating_add(data.len()) <= self.limit => {
                let needed = copy.len() + data.len();
                if needed > copy.capacity() {
                    // Double as `Vec` would, but never past the limit.
                    let target = copy
                        .capacity()
                        .saturating_mul(2)
                        .max(needed)
                        .min(self.limit);
                    copy.reserve_exact(target - copy.len());
                }
                copy.extend_from_slice(data);
            }
            // Over the limit, or trailers the copy cannot carry.
            _ => self.copy = None,
        }
    }
}

/// The body the first attempt sends: the caller's, copied as it goes.
struct Tee {
    shared: Arc<Mutex<Shared>>,
}

impl Body for Tee {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let mut shared = self.shared.lock();
        let Some(body) = shared.body.as_mut() else {
            // A resend took the rest: this attempt must not end cleanly.
            return Poll::Ready(Some(Err(Box::new(ProxyError::new(
                ErrorKind::Write,
                "request body moved to a resend",
            )))));
        };
        let frame = ready!(Pin::new(body).poll_frame(cx));
        match &frame {
            Some(Ok(frame)) => shared.record(frame),
            Some(Err(_)) => shared.copy = None,
            None => shared.ended = true,
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        // Not ended once taken: the attempt must fail, not end short.
        self.shared
            .lock()
            .body
            .as_ref()
            .is_some_and(ProxyBody::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.shared
            .lock()
            .body
            .as_ref()
            .map_or_else(SizeHint::default, ProxyBody::size_hint)
    }
}

/// A resend's body: the copy, then the rest of the caller's body.
struct Resumed {
    copy: Option<Bytes>,
    /// `None` when the first attempt read the body to its end.
    rest: Option<ProxyBody>,
}

impl Body for Resumed {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        if let Some(copy) = self.copy.take() {
            return Poll::Ready(Some(Ok(Frame::data(copy))));
        }
        match self.rest.as_mut() {
            Some(rest) => Pin::new(rest).poll_frame(cx),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.copy.is_none() && self.rest.as_ref().is_none_or(ProxyBody::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        let copied = self.copy.as_ref().map_or(0, |c| c.len() as u64);
        let rest = self
            .rest
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), ProxyBody::size_hint);
        let mut hint = SizeHint::new();
        hint.set_lower(rest.lower().saturating_add(copied));
        if let Some(upper) = rest.upper() {
            hint.set_upper(upper.saturating_add(copied));
        }
        hint
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod replay_tests;

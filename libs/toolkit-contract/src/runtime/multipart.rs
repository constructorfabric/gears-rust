//! `multipart/mixed` reader used by streaming clients.
//!
//! Translates a byte stream into a stream of typed items, one JSON item per
//! body part. This is streaming *response* framing only — `multipart/form-data`
//! requests, mixed per-part content types and `Content-Disposition` are all out
//! of scope, and every part is assumed to carry a JSON document decodable as
//! `T`.
//!
//! ### Framing is length-driven, not parse-driven
//!
//! A part's extent is decided by its MIME headers, never by where its JSON
//! payload happens to become syntactically complete. Two modes, in this
//! precedence:
//!
//! 1. **`Content-Length: N` present** — consume exactly `N` body bytes, decode,
//!    then expect the delimiter. Exact, one part per part, no lookahead, so a
//!    part is yielded the instant its own bytes have arrived — the reader never
//!    runs a part behind a live stream.
//! 2. **Absent** — strict RFC 2046 delimiter scan. Correct, but a part is only
//!    emitted once the *next* delimiter (or the close delimiter) arrives, so a
//!    live producer that writes part `N+1`'s delimiter only when item `N+1`
//!    exists leaves the reader one part behind.
//!
//! `Content-Length` on a body part is unusual but well-formed: RFC 2046 §5.1
//! makes the *delimiter* the authoritative boundary mechanism, so a strict
//! parser that ignores the header still reads the stream correctly. The header
//! is a fast path, not a protocol fork.
//!
//! ### A missing close delimiter is a truncation
//!
//! A complete multipart body ends with the `--<boundary>--` close delimiter
//! (RFC 2046). A graceful end of the byte stream that did **not** see it is a
//! truncation — a proxy idle-timeout, an LB half-close, or a rolling deploy
//! that closed the connection mid-body — and the streaming client surfaces it
//! as a [`TransportError::Framing`] end-of-stream error, exactly as the SSE
//! path errors on a close without a terminal `event: done`.
//!
//! This is reported by the transport rather than left to the consumer because
//! on the public path the consumer *cannot* detect it: `send_streaming` /
//! `open_streaming` return a boxed stream that erases the concrete
//! [`MultipartStream`], so [`MultipartStream::saw_close_delimiter`] is
//! unreachable there, and a generic item type carries no terminal marker of its
//! own. (`saw_close_delimiter` remains available to a caller holding the
//! concrete stream, as diagnostic detail about the framing.) A partially
//! buffered part at end of stream is discarded rather than surfaced, mirroring
//! how the SSE parser discards an unterminated trailing event.
//!
//! An *aborted* body — the peer's byte stream erroring, which is what a
//! truncated chunked encoding looks like — is a different thing and is
//! surfaced as [`TransportError::Network`]. That is reserved for a genuine
//! transport fault (a server item that would not serialize at all).
//!
//! A post-open **domain** failure is not an abort: the framer sends it as a
//! typed **error part** — one `application/problem+json` part whose body is an
//! RFC 9457 [`Problem`] — followed by the close delimiter. This reader decodes
//! such a part into [`TransportError::Problem`] (which the generated client
//! recovers as a typed `CanonicalError`), so a mid-stream domain error arrives
//! as a *typed* `Err` item, not a truncation. An error part is **terminal**: the
//! reader stops after it and treats it as a clean end, so a non-conforming peer
//! cannot smuggle further data items past a reported error.
//!
//! Accumulated buffers are bounded by [`MAX_ACCUMULATED_BYTES`] for the same
//! reason the SSE parser bounds its own: otherwise a peer that streams an
//! unterminated construct grows the buffer without limit for the lifetime of a
//! self-healing, indefinitely-reconnecting client.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use futures_core::Stream;
use serde::de::DeserializeOwned;

use toolkit_canonical_errors::Problem;

use crate::ir::binding::StreamFraming;
use crate::runtime::sse::StreamActivity;
use crate::runtime::transport_error::TransportError;

/// Maximum bytes the reader accumulates for a single not-yet-complete
/// construct — the preamble, a part's header block, a length-less part body, or
/// a delimiter — before treating the peer as protocol-violating and
/// terminating the stream with [`TransportError::Framing`]. Same value and same
/// rationale as the SSE parser's own guard.
pub const MAX_ACCUMULATED_BYTES: usize = 16 * 1024 * 1024;

const CRLF: &[u8] = b"\r\n";
const CLOSE_MARKER: &[u8] = b"--";
const CONTENT_LENGTH: &[u8] = b"content-length";
const CONTENT_TYPE: &[u8] = b"content-type";
/// Media type marking a part as a typed **error part**: its body is an RFC 9457
/// [`Problem`], not a `T`. The server framer emits exactly this token (see
/// `toolkit::http::multipart`); matched case-insensitively and ignoring any
/// `;`-parameters.
const PROBLEM_MEDIA_TYPE: &[u8] = b"application/problem+json";

/// Adapter that lifts a `Display`-only error into an
/// `Error + Send + Sync + 'static` so it can be boxed into
/// [`TransportError::Network`] without losing the original message.
///
/// Twin of the same adapter in [`crate::runtime::sse`]; duplicated rather than
/// shared because the SSE parser is deliberately not touched by this work.
#[derive(Debug)]
struct DisplayError(String);
impl std::fmt::Display for DisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DisplayError {}

/// A framing error message that keeps its underlying cause reachable via
/// [`Error::source`], so a caller can see the `Utf8Error`/`ParseIntError`
/// behind a parse failure rather than a flattened string (#4740). `Display` is
/// the human message; `source()` is the real error.
#[derive(Debug)]
struct SourcedError {
    message: String,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}
impl std::fmt::Display for SourcedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SourcedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// Extract the `boundary` parameter from a `Content-Type` header value.
///
/// The media type is matched case-insensitively and the boundary may be quoted
/// (`boundary="abc"`) or bare (`boundary=abc`). Parameters are split on `;`,
/// which is safe because RFC 2046 excludes `;` from the legal boundary
/// characters, so no legal boundary can contain one even when quoted.
///
/// # Errors
/// [`TransportError::Framing`] when the media type is not `multipart/mixed`,
/// when no `boundary` parameter is present, or when the parameter is empty or
/// an unterminated quoted string.
pub fn boundary_from_content_type(value: &str) -> Result<String, TransportError> {
    let mut params = value.split(';');
    let media_type = params.next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case(StreamFraming::MultipartMixed.media_type()) {
        return Err(framing_error(format!(
            "expected media type `{}`, got `{media_type}`",
            StreamFraming::MultipartMixed.media_type()
        )));
    }

    for param in params {
        let Some((name, raw)) = param.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let raw = raw.trim();
        let boundary = if let Some(inner) = raw.strip_prefix('"') {
            inner.strip_suffix('"').ok_or_else(|| {
                framing_error(format!(
                    "`boundary` parameter is not a closed quoted string: `{raw}`"
                ))
            })?
        } else {
            raw
        };
        if boundary.is_empty() {
            return Err(framing_error("`boundary` parameter is empty"));
        }
        return Ok(boundary.to_owned());
    }

    Err(framing_error(format!(
        "`{}` response carries no `boundary` parameter: `{value}`",
        StreamFraming::MultipartMixed.media_type()
    )))
}

fn framing_error(message: impl Into<String>) -> TransportError {
    TransportError::framing(StreamFraming::MultipartMixed, DisplayError(message.into()))
}

/// A framing error whose `source()` is `source` and whose `Display` is
/// `message`, so the underlying parse error stays on the chain (#4740).
fn framing_error_sourced(
    message: impl Into<String>,
    source: impl std::error::Error + Send + Sync + 'static,
) -> TransportError {
    TransportError::framing(
        StreamFraming::MultipartMixed,
        SourcedError {
            message: message.into(),
            source: Box::new(source),
        },
    )
}

/// Render an untrusted header value for an error message: truncate to a short
/// fixed length and escape control characters, so a hostile peer cannot inject
/// bare CR/LF or up to `MAX_ACCUMULATED_BYTES` of arbitrary bytes into a string
/// that is then propagated and logged (#4740).
fn display_value(text: &str) -> String {
    const MAX_CHARS: usize = 64;
    let mut out = String::new();
    let mut truncated = false;
    for (i, ch) in text.chars().enumerate() {
        if i >= MAX_CHARS {
            truncated = true;
            break;
        }
        if ch.is_control() {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    if truncated {
        out.push_str("...");
    }
    out
}

/// Parse a `multipart/mixed` byte stream into a stream of typed items.
///
/// `body` is typically the byte-stream view of
/// `toolkit_http::HttpResponse::into_body()` (adapted via
/// [`crate::runtime::http::body_to_byte_stream`]); `boundary` is the value
/// returned by [`boundary_from_content_type`] — the bare token, with no
/// leading dashes. Errors from the inner stream are surfaced as
/// [`TransportError::Network`].
pub fn parse_multipart_stream<T, S, E>(body: S, boundary: &str) -> MultipartStream<T, S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    E: std::fmt::Display,
    T: DeserializeOwned + 'static,
{
    let mut dash_boundary = Vec::with_capacity(CLOSE_MARKER.len() + boundary.len());
    dash_boundary.extend_from_slice(CLOSE_MARKER);
    dash_boundary.extend_from_slice(boundary.as_bytes());

    let mut crlf_dash_boundary = Vec::with_capacity(CRLF.len() + dash_boundary.len());
    crlf_dash_boundary.extend_from_slice(CRLF);
    crlf_dash_boundary.extend_from_slice(&dash_boundary);

    MultipartStream {
        inner: body,
        buf: BytesMut::with_capacity(4 * 1024),
        scan_from: 0,
        state: State::Preamble,
        pending: VecDeque::new(),
        dash_boundary,
        crlf_dash_boundary,
        deferred_decode_error: None,
        done: false,
        saw_close: false,
        activity: StreamActivity::new(),
        _marker: std::marker::PhantomData,
    }
}

/// What a part's `Content-Type` says its body is, and therefore how the reader
/// decodes it: an ordinary data item (`T`) or a typed error part (a [`Problem`],
/// `application/problem+json`). Selected in [`MultipartStream::step_headers`] and
/// carried through the body states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PartKind {
    /// A normal `application/json` data part; the body decodes as `T`.
    #[default]
    Data,
    /// An `application/problem+json` error part; the body decodes as a
    /// [`Problem`] and is surfaced as a *terminal* typed `Err` item.
    Error,
}

/// Where the reader is in the `multipart/mixed` grammar.
///
/// The plan's single `Delimiter` state is realised as the pair
/// `Delimiter` (expects the `CRLF--<boundary>` token that closes a part) and
/// `BoundaryTail` (having consumed a boundary token, decides between `CRLF`
/// for another part and `--` for the close delimiter). Splitting them lets
/// `Preamble` reuse the tail logic, since the very first delimiter has no
/// leading `CRLF`.
#[derive(Debug, Clone, Copy)]
enum State {
    /// Discard bytes until the first `--<boundary>`.
    Preamble,
    /// A boundary token has just been consumed: `CRLF` starts another part,
    /// `--` is the close delimiter.
    BoundaryTail,
    /// Accumulating a part's header block, up to its terminating empty line.
    Headers {
        /// `Content-Length` seen so far for this part, if any.
        content_length: Option<usize>,
        /// Header-block bytes consumed so far, for the accumulation guard —
        /// header lines leave `buf` as they are parsed, so `buf.len()` alone
        /// would not bound a peer streaming an endless run of tiny headers.
        consumed: usize,
        /// What this part's `Content-Type` has said so far.
        kind: PartKind,
    },
    /// Reading a part body. `Some(n)` is the exact `Content-Length` byte count;
    /// `None` means scan for the next delimiter.
    Body {
        /// Exact remaining body length, when the part declared one.
        remaining: Option<usize>,
        /// Carried from [`State::Headers`]: how to decode this body.
        kind: PartKind,
    },
    /// Expecting the `CRLF--<boundary>` token that closes the part just read.
    Delimiter,
    /// Terminal.
    Closed,
}

/// Outcome of one step of the state machine.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Progress was made — step again.
    Progress,
    /// Not enough buffered bytes to make progress — poll the wire.
    NeedMore,
    /// Terminal state reached; stop stepping.
    Stop,
}

/// Stream yielded by [`parse_multipart_stream`].
pub struct MultipartStream<T, S> {
    inner: S,
    buf: BytesMut,
    /// Prefix length of `buf`, in bytes, already confirmed to contain no match
    /// for whatever the current state is scanning for. Lets [`find_from`]
    /// resume instead of rescanning the whole buffer on every poll — without
    /// this, a single long construct delivered over many small chunks costs
    /// `O(total_length²)` instead of `O(total_length)`. Reset to 0 whenever
    /// bytes are consumed or the state changes what it scans for.
    scan_from: usize,
    state: State,
    pending: VecDeque<Result<T, TransportError>>,
    /// `--<boundary>`, precomputed.
    dash_boundary: Vec<u8>,
    /// `CRLF--<boundary>`, precomputed.
    crlf_dash_boundary: Vec<u8>,
    /// A sized part's body that would not decode, held until the bytes that
    /// follow reveal *why*. A well-framed part with a malformed payload is a
    /// [`TransportError::Serialization`] fault; a part whose declared length
    /// disagrees with its delimiter is a [`TransportError::Framing`] fault —
    /// and from the body bytes alone the two are indistinguishable, because a
    /// wrong length hands the decoder the wrong bytes. Deciding at the
    /// delimiter keeps the *successful* decode on the fast path, which is what
    /// the latency property depends on: a part that decodes is yielded from
    /// its own bytes, and only a part that does not wait for its delimiter.
    ///
    /// Only ever set while in [`State::Delimiter`].
    deferred_decode_error: Option<serde_json::Error>,
    done: bool,
    saw_close: bool,
    activity: StreamActivity,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T, S> MultipartStream<T, S> {
    /// Returns a clone of the shared wire-activity counter, bumped on every
    /// wire chunk — including chunks that dispatch no item, such as a part
    /// header block arriving on its own. The streaming client snapshots it
    /// around an idle-timeout wait so the idle deadline stays *idle* rather
    /// than merely *quiet*. Mirrors
    /// [`crate::runtime::sse::SseStream::activity_handle`].
    #[must_use]
    pub fn activity_handle(&self) -> StreamActivity {
        self.activity.clone()
    }

    /// Whether the stream reached a graceful end — the peer sent the closing
    /// `--<boundary>--` delimiter, **or** a terminal error part ended it (an
    /// error part is a complete terminator, so it counts as a clean end).
    ///
    /// Analogous to [`crate::runtime::sse::SseStream::saw_done_event`]: the
    /// streaming client treats a graceful end *without* either as a truncation
    /// and surfaces a [`TransportError::Framing`] error (see the module docs).
    /// This accessor exposes the same framing fact to a caller holding the
    /// concrete stream as diagnostic detail. Only meaningful once the stream has
    /// yielded `None`.
    #[must_use]
    pub fn saw_close_delimiter(&self) -> bool {
        self.saw_close
    }
}

// `inner` is bounded by `Unpin` at construction; the rest of the fields are
// trivially `Unpin`. Implement `Unpin` unconditionally so callers can poll
// `Pin<&mut MultipartStream<...>>` without pinning the type itself.
impl<T, S: Unpin> Unpin for MultipartStream<T, S> {}

impl<T, S> MultipartStream<T, S>
where
    T: DeserializeOwned + 'static,
{
    /// Run the state machine over the currently buffered bytes, stopping as
    /// soon as an item is ready.
    ///
    /// Stopping at the first `pending` item bounds the work done in one
    /// `poll_next`: one wire chunk carrying many parts no longer runs every
    /// `serde_json::from_slice` before yielding, and no longer pushes every
    /// decoded item into the unbounded `pending` queue at once. The remaining
    /// buffered bytes are drained on the next poll (`poll_next` drains before it
    /// polls the wire), so no chunk is needed to make progress.
    fn drain_buffer(&mut self) {
        while self.pending.is_empty() && matches!(self.step(), Step::Progress) {}
        if matches!(self.state, State::Closed) {
            self.done = true;
        }
    }

    fn step(&mut self) -> Step {
        match self.state {
            State::Preamble => self.step_preamble(),
            State::BoundaryTail => self.step_boundary_tail(),
            State::Headers {
                content_length,
                consumed,
                kind,
            } => self.step_headers(content_length, consumed, kind),
            State::Body { remaining, kind } => match remaining {
                Some(n) => self.step_sized_body(n, kind),
                None => self.step_scanned_body(kind),
            },
            State::Delimiter => self.step_delimiter(),
            State::Closed => Step::Stop,
        }
    }

    fn step_preamble(&mut self) -> Step {
        let mut scan = self.scan_from;
        let hit = find_from(&self.buf, &self.dash_boundary, &mut scan);
        self.scan_from = scan;
        match hit {
            Some(at) => {
                self.consume(at + self.dash_boundary.len());
                self.state = State::BoundaryTail;
                Step::Progress
            }
            None => self.need_more("multipart preamble"),
        }
    }

    fn step_boundary_tail(&mut self) -> Step {
        if self.buf.len() >= CLOSE_MARKER.len() {
            if self.buf.starts_with(CLOSE_MARKER) {
                self.consume(CLOSE_MARKER.len());
                self.saw_close = true;
                self.state = State::Closed;
                return Step::Stop;
            }
        } else if CLOSE_MARKER.starts_with(&self.buf) {
            // Could still turn into the close delimiter once more bytes land.
            return self.need_more("multipart boundary delimiter");
        }

        let mut scan = self.scan_from;
        let hit = find_from(&self.buf, CRLF, &mut scan);
        self.scan_from = scan;
        match hit {
            Some(at) => {
                // RFC 2046 permits transport padding (linear whitespace)
                // between the boundary and its CRLF; anything else means the
                // boundary token was a prefix of some longer token.
                if self.buf[..at].iter().any(|b| !matches!(b, b' ' | b'\t')) {
                    return self.fail("boundary delimiter is followed by neither `CRLF` nor `--`");
                }
                self.consume(at + CRLF.len());
                self.state = State::Headers {
                    content_length: None,
                    consumed: 0,
                    kind: PartKind::Data,
                };
                Step::Progress
            }
            None => self.need_more("multipart boundary delimiter"),
        }
    }

    fn step_headers(
        &mut self,
        content_length: Option<usize>,
        consumed: usize,
        kind: PartKind,
    ) -> Step {
        let mut scan = self.scan_from;
        let hit = find_from(&self.buf, CRLF, &mut scan);
        self.scan_from = scan;
        let Some(at) = hit else {
            return self.need_more("multipart part headers");
        };

        let line = self.buf.split_to(at + CRLF.len());
        self.scan_from = 0;
        let consumed = consumed + line.len();

        // An empty line terminates the header block. A part with no headers at
        // all is legal MIME and lands here on the first iteration.
        if at == 0 {
            self.state = State::Body {
                remaining: content_length,
                kind,
            };
            return Step::Progress;
        }

        if consumed > MAX_ACCUMULATED_BYTES {
            return self.fail(format!(
                "multipart part headers exceed maximum accumulated size ({MAX_ACCUMULATED_BYTES} bytes); aborting stream"
            ));
        }

        // `Content-Length` sizes the body; `Content-Type` is read only to spot a
        // typed error part (`application/problem+json`). Every other part header
        // is deliberately ignored.
        let content_length = match header_value(&line[..at], CONTENT_LENGTH) {
            None => content_length,
            Some(raw) => match parse_content_length(raw) {
                Ok(n) => Some(n),
                Err(err) => return self.fail_with(err),
            },
        };
        let kind = if is_problem_content_type(header_value(&line[..at], CONTENT_TYPE)) {
            PartKind::Error
        } else {
            kind
        };

        self.state = State::Headers {
            content_length,
            consumed,
            kind,
        };
        Step::Progress
    }

    fn step_sized_body(&mut self, len: usize, kind: PartKind) -> Step {
        if self.buf.len() < len {
            return self.need_more("multipart part body");
        }
        let body = self.buf.split_to(len);
        self.scan_from = 0;
        self.state = State::Delimiter;
        // The delimiter has not been proven yet, so a decode failure here may
        // be the length's fault rather than the payload's — defer it.
        match Self::decode_body(&body, kind) {
            Ok(item) => {
                self.pending.push_back(item);
                self.finish_if_error(kind);
            }
            Err(e) => self.deferred_decode_error = Some(e),
        }
        Step::Progress
    }

    fn step_scanned_body(&mut self, kind: PartKind) -> Step {
        let mut scan = self.scan_from;
        let hit = find_from(&self.buf, &self.crlf_dash_boundary, &mut scan);
        self.scan_from = scan;
        match hit {
            Some(at) => {
                let body = self.buf.split_to(at);
                self.scan_from = 0;
                // `buf` now starts with the delimiter the scan found, so the
                // body's extent is already authoritative and a decode failure
                // can only be the payload's fault.
                self.state = State::Delimiter;
                self.decode_now(&body, kind)
            }
            None => self.need_more("multipart part body"),
        }
    }

    /// An error part is **terminal** (#4740 F5): once a typed error item is
    /// queued, close the stream so a non-conforming peer's parts after it are
    /// never surfaced as data. The framer emits the close delimiter after an
    /// error part anyway, but the reader must not rely on the peer to stop.
    ///
    /// The error part is itself a graceful, complete terminator, so it counts as
    /// a clean end (`saw_close`) — otherwise the streaming client would treat the
    /// unread close delimiter as a truncation and append a spurious framing error
    /// after the typed one.
    fn finish_if_error(&mut self, kind: PartKind) {
        if kind == PartKind::Error {
            self.state = State::Closed;
            self.saw_close = true;
        }
    }

    fn step_delimiter(&mut self) -> Step {
        // RFC 2046 makes the CRLF part of the delimiter, so it is normally
        // present; tolerate a producer that omits it before the close
        // delimiter, since the boundary token alone is unambiguous.
        let skip = if self.buf.starts_with(CRLF) {
            CRLF.len()
        } else if self.buf.len() < CRLF.len() && CRLF.starts_with(&self.buf) {
            return self.need_more("multipart part delimiter");
        } else {
            0
        };

        let outcome = {
            let avail = &self.buf[skip..];
            if avail.len() < self.dash_boundary.len() {
                if self.dash_boundary.starts_with(avail) {
                    DelimiterCheck::NeedMore
                } else {
                    DelimiterCheck::Overrun
                }
            } else if avail.starts_with(&self.dash_boundary) {
                DelimiterCheck::Matched
            } else {
                DelimiterCheck::Overrun
            }
        };

        match outcome {
            DelimiterCheck::NeedMore => self.need_more("multipart part delimiter"),
            // The bytes right after the part body are not a delimiter, so the
            // part's declared length did not describe the part: either it ran
            // short (leaving payload bytes here) or it ran long (having eaten
            // into the delimiter). Either way the framing is unusable.
            DelimiterCheck::Overrun => {
                // The length is the root cause and subsumes any deferred
                // decode failure it caused, so drop that and report the
                // framing fault.
                self.deferred_decode_error = None;
                self.fail(
                    "part `Content-Length` does not agree with the multipart delimiter that follows it",
                )
            }
            DelimiterCheck::Matched => {
                // The part was framed correctly after all, so a held decode
                // failure really was the payload's fault.
                if let Some(e) = self.deferred_decode_error.take() {
                    return self.fail_serialization(e);
                }
                self.consume(skip + self.dash_boundary.len());
                self.state = State::BoundaryTail;
                Step::Progress
            }
        }
    }

    /// Decode a part body whose extent is already proven by its delimiter.
    ///
    /// A malformed part terminates the stream: unlike a named SSE event, a
    /// part is unambiguously the typed data channel, so a body that will not
    /// decode is real data loss and must not be silently dropped.
    fn decode_now(&mut self, body: &[u8], kind: PartKind) -> Step {
        match Self::decode_body(body, kind) {
            Ok(item) => {
                self.pending.push_back(item);
                self.finish_if_error(kind);
                Step::Progress
            }
            Err(e) => self.fail_serialization(e),
        }
    }

    /// Decode one part body into a pending item. A [`PartKind::Data`] part
    /// decodes as `T` and yields `Ok(item)`; a [`PartKind::Error`] part decodes
    /// as an RFC 9457 [`Problem`] and yields `Err(TransportError::Problem { .. })`.
    ///
    /// The `Err` variant is a *decoded item*, not a stream fault; the caller
    /// pushes it and then closes the stream (`finish_if_error`), because an error
    /// part is terminal. Only a body that will not decode at all is a
    /// `serde_json::Error` here — a genuine serialization fault the caller
    /// surfaces via `fail_serialization` (directly, or deferred to the delimiter
    /// for a sized body).
    fn decode_body(
        body: &[u8],
        kind: PartKind,
    ) -> Result<Result<T, TransportError>, serde_json::Error> {
        match kind {
            PartKind::Error => {
                let problem = serde_json::from_slice::<Problem>(body)?;
                Ok(Err(TransportError::problem(problem)))
            }
            PartKind::Data => Ok(Ok(serde_json::from_slice::<T>(body)?)),
        }
    }

    fn fail_serialization(&mut self, e: serde_json::Error) -> Step {
        self.state = State::Closed;
        self.pending
            .push_back(Err(TransportError::serialization(e)));
        Step::Stop
    }

    fn consume(&mut self, n: usize) {
        self.buf.advance(n);
        self.scan_from = 0;
    }

    /// Not enough bytes to complete the current construct. Trips the
    /// accumulation guard first, so a peer that streams unbounded bytes
    /// without ever completing one terminates the stream instead of growing
    /// `buf` without limit.
    fn need_more(&mut self, what: &str) -> Step {
        if self.buf.len() > MAX_ACCUMULATED_BYTES {
            return self.fail(format!(
                "{what} exceeds maximum accumulated size ({MAX_ACCUMULATED_BYTES} bytes); aborting stream"
            ));
        }
        Step::NeedMore
    }

    fn fail(&mut self, message: impl Into<String>) -> Step {
        self.fail_with(framing_error(message))
    }

    /// Like [`fail`](Self::fail) but for an already-built error, so a caller
    /// that constructed a sourced framing error keeps its `source()` chain.
    fn fail_with(&mut self, err: TransportError) -> Step {
        self.state = State::Closed;
        self.pending.push_back(Err(err));
        Step::Stop
    }
}

/// Result of matching the buffered bytes against a part delimiter.
#[derive(Debug, Clone, Copy)]
enum DelimiterCheck {
    Matched,
    NeedMore,
    Overrun,
}

impl<T, S, E> Stream for MultipartStream<T, S>
where
    T: DeserializeOwned + 'static,
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    E: std::fmt::Display,
{
    type Item = Result<T, TransportError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Some(item) = this.pending.pop_front() {
                return Poll::Ready(Some(item));
            }
            if this.done {
                return Poll::Ready(None);
            }

            // Drain what is already buffered before touching the wire: a
            // previous poll may have left decoded-but-unyielded parts in the
            // buffer (drain_buffer stops at the first item). Only poll the wire
            // when the buffer can produce nothing more on its own.
            this.drain_buffer();
            if !this.pending.is_empty() || this.done {
                continue;
            }

            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    // A graceful end of the byte stream is a clean end of this
                    // stream, with or without the closing `--<boundary>--`.
                    // Whatever partial construct is buffered is discarded
                    // rather than surfaced as a truncation error — see the
                    // module docs on why completion detection is the
                    // consumer's, and why an *aborted* body (the `Err` arm
                    // below) is loud instead.
                    this.done = true;
                    this.state = State::Closed;
                    this.buf.clear();
                    // A held decode failure is the one thing a graceful EOF
                    // does not excuse: the part's `Content-Length` said its
                    // bytes were all there, and they were, and they did not
                    // decode. That is data loss rather than truncation, so it
                    // is surfaced even though the stream is ending cleanly.
                    if let Some(e) = this.deferred_decode_error.take() {
                        this.pending
                            .push_back(Err(TransportError::serialization(e)));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    this.done = true;
                    this.state = State::Closed;
                    // `E: Display` only — wrap in a small Display->Error
                    // adapter so the source chain stays intact through
                    // `TransportError::network`.
                    return Poll::Ready(Some(Err(TransportError::network(DisplayError(
                        e.to_string(),
                    )))));
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    // Any wire chunk counts as activity — even one carrying
                    // only a part header block — so the idle timeout in the
                    // streaming driver can tell "quiet but alive" from "truly
                    // idle". The new bytes are drained at the top of the next
                    // loop iteration, before the wire is polled again.
                    this.activity.bump();
                    this.buf.extend_from_slice(&chunk);
                }
            }
        }
    }
}

/// Find `needle` in `haystack`, resuming from `*scan_from` — the length of a
/// prefix already confirmed to start no match.
///
/// On a miss, `*scan_from` advances to `haystack.len()`, so the next call
/// rescans only the unavoidable `needle.len() - 1` bytes of overlap plus
/// whatever was appended since. On a hit it is left at the match offset; the
/// caller consumes bytes and resets it.
fn find_from(haystack: &[u8], needle: &[u8], scan_from: &mut usize) -> Option<usize> {
    let start = scan_from.saturating_sub(needle.len().saturating_sub(1));
    if needle.is_empty() || haystack.len() < needle.len() {
        *scan_from = haystack.len();
        return None;
    }
    for at in start..=(haystack.len() - needle.len()) {
        if haystack[at..].starts_with(needle) {
            *scan_from = at;
            return Some(at);
        }
    }
    *scan_from = haystack.len();
    None
}

/// Value of the header `name` (given lowercase) on `line`, or `None` when the
/// line is a different header. `line` excludes its terminating `CRLF`.
fn header_value<'l>(line: &'l [u8], name: &[u8]) -> Option<&'l [u8]> {
    let at = line.iter().position(|b| *b == b':')?;
    let (candidate, rest) = line.split_at(at);
    if candidate.len() != name.len()
        || !candidate
            .iter()
            .zip(name)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    {
        return None;
    }
    Some(rest[1..].trim_ascii())
}

/// Whether a part's `Content-Type` header value marks it a typed error part,
/// i.e. its media type is `application/problem+json`. `None` (no `Content-Type`
/// on this line) is not a match. Parameters after `;` are ignored and the media
/// type is compared case-insensitively, per RFC 7231.
fn is_problem_content_type(value: Option<&[u8]>) -> bool {
    let Some(value) = value else { return false };
    let media_type = match value.iter().position(|b| *b == b';') {
        Some(at) => &value[..at],
        None => value,
    }
    .trim_ascii();
    media_type.eq_ignore_ascii_case(PROBLEM_MEDIA_TYPE)
}

/// Parse a part's `Content-Length`, rejecting a value the accumulation guard
/// would not let us buffer anyway. `Err` is the framing error, carrying the
/// underlying `Utf8Error`/`ParseIntError` as its `source()` and echoing the
/// offending value only after truncating and escaping it.
fn parse_content_length(raw: &[u8]) -> Result<usize, TransportError> {
    let text = std::str::from_utf8(raw)
        .map_err(|e| framing_error_sourced("part `Content-Length` is not valid UTF-8", e))?;
    let len: usize = text.parse().map_err(|e| {
        framing_error_sourced(
            format!(
                "part `Content-Length` is not a byte count: `{}`",
                display_value(text)
            ),
            e,
        )
    })?;
    if len > MAX_ACCUMULATED_BYTES {
        return Err(framing_error(format!(
            "part `Content-Length` of {len} exceeds maximum accumulated size ({MAX_ACCUMULATED_BYTES} bytes)"
        )));
    }
    Ok(len)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
#[path = "multipart_tests.rs"]
mod tests;

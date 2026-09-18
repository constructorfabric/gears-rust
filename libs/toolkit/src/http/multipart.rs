//! `multipart/mixed` streaming responses — the server counterpart to
//! `toolkit_contract::runtime::multipart`.
//!
//! Sits beside [`super::sse`] as the second wire framing for a long-lived
//! streaming endpoint: one JSON item per body part, for server-to-server
//! consumers, where SSE serves browser-direct `EventSource`-style ones.
//!
//! This is streaming *response* framing only. `multipart/form-data` requests,
//! mixed per-part content types and `Content-Disposition` are all out of scope,
//! and the `boundary=` parameter is generated here rather than declared by the
//! author.

use axum::body::{Body, Bytes};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use futures_util::StreamExt as _;
use http::HeaderValue;
use http::header::CONTENT_TYPE;
use serde::Serialize;
use toolkit_canonical_errors::Problem;
use toolkit_contract::runtime::multipart::MAX_ACCUMULATED_BYTES;

/// RFC 2046 caps a boundary at 70 characters.
const MAX_BOUNDARY_LEN: usize = 70;

/// Media type of a typed **error part**. A post-open domain error is framed as
/// a normal part whose `Content-Type` is this rather than `application/json`,
/// so the reader can decode its body as an RFC 9457 [`Problem`] and surface a
/// typed `Err`, instead of the whole body aborting. Kept byte-for-byte in sync
/// with the reader's matcher in `toolkit_contract::runtime::multipart`.
const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// Largest serialized item this framer will emit as a single part.
///
/// Bound to the paired reader's accumulation ceiling
/// ([`toolkit_contract::runtime::multipart::MAX_ACCUMULATED_BYTES`]) — the
/// reader is the source of truth. The reader rejects any part whose body
/// exceeds it, so emitting a larger part would produce a stream the toolkit
/// client cannot consume; the framer aborts the body instead (see [`frame`]).
const MAX_PART_BYTES: usize = MAX_ACCUMULATED_BYTES;

/// A `multipart/mixed` streaming response, one JSON part per item.
///
/// Emits, per item: `--<boundary>CRLF`, `Content-Type: application/json`,
/// `Content-Length: <n>`, `CRLF`, the JSON body, `CRLF`; and
/// `--<boundary>--CRLF` on a clean end of the source stream.
///
/// The per-part `Content-Length` is what lets a reader emit each part from that
/// part's own bytes instead of waiting for the following delimiter — so a live
/// stream is never delivered a part behind. It is a fast path, not a protocol
/// fork: RFC 2046 §5.1 makes the delimiter authoritative, so a strict parser
/// that ignores the header still reads the stream correctly.
///
/// # Post-open domain error
///
/// The wrapped stream yields `Result<T, E>`. Once the `200` is on the wire a
/// domain failure can no longer become an HTTP status, so an `Err(e)` is framed
/// as a typed **error part**: one `application/problem+json` part carrying
/// `Into::<Problem>::into(e)`, immediately followed by the close delimiter. The
/// stream therefore ends *cleanly* and the reader surfaces the error as a typed
/// `Err` item, not a truncation — an `Err` is terminal, so any later source
/// items are not framed.
///
/// If the error `Problem` itself would exceed [`MAX_PART_BYTES`], its unbounded
/// fields (`detail`, `context`) are dropped so the typed error — status, type,
/// `error_code` — still reaches the client; only a `Problem` that cannot fit or
/// serialize even trimmed falls back to an abort.
///
/// # Mid-stream serialization failure (genuine transport fault)
///
/// Distinct from the above: an `Ok(item)` that will not *serialize*, or one
/// larger than [`MAX_PART_BYTES`], is not a domain error — it is a value the
/// framer cannot put on the wire at all. There is no status left to send, so
/// the response **body is aborted** and the failure is logged at `error`.
///
/// "Abort" specifically means yielding an error into the body stream, which
/// truncates the chunked encoding and makes the reader report a transport
/// error — *not* ending the stream, which would be a graceful EOF and which
/// the reader treats as a clean end. A silently short stream is worse than a
/// broken connection: the consumer's reopen path already handles an ungraceful
/// close, but it cannot detect a stream that simply stopped early.
pub struct MultipartJsonStream<S> {
    stream: S,
    boundary: String,
    /// Prebuilt `Content-Type`, so rendering the response has no fallible
    /// step — the boundary is validated once, on the way in.
    content_type: HeaderValue,
}

/// A caller-supplied boundary was rejected by
/// [`MultipartJsonStream::with_boundary`].
///
/// Returned rather than silently substituted so the caller decides whether to
/// fall back to a generated boundary ([`MultipartJsonStream::new`]) or surface
/// the failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryError {
    boundary: String,
    reason: &'static str,
}

impl BoundaryError {
    /// Why the boundary was rejected.
    #[must_use]
    pub fn reason(&self) -> &str {
        self.reason
    }
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `escape_debug` so control characters in a rejected value are escaped
        // rather than written raw into the message.
        write!(
            f,
            "invalid multipart/mixed boundary \"{}\": {}",
            self.boundary.escape_debug(),
            self.reason
        )
    }
}

impl std::error::Error for BoundaryError {}

impl<S> MultipartJsonStream<S> {
    /// Wrap a stream of serializable items, with a generated boundary.
    #[must_use]
    pub fn new(stream: S) -> Self {
        Self::with_validated_boundary(stream, generated_boundary())
    }

    /// Wrap a stream of serializable items, with a caller-chosen boundary.
    ///
    /// The boundary must be RFC 2046-legal *and* an HTTP token: 1–70 characters
    /// drawn from `A-Z a-z 0-9 ' + - . _` (see [`validate_boundary`]).
    ///
    /// Returns [`BoundaryError`] if the boundary is rejected, leaving the
    /// fallback decision to the caller — typically [`MultipartJsonStream::new`]
    /// for a generated boundary — rather than silently substituting one, which
    /// would hide the caller's mistake behind a boundary they never chose.
    ///
    /// The boundary must also not occur inside any item's JSON. A generated
    /// boundary makes that impossible in practice; a caller-chosen one makes it
    /// the caller's responsibility.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryError`] when `boundary` is empty, exceeds 70
    /// characters, or contains a character outside the HTTP token set.
    pub fn with_boundary(stream: S, boundary: impl Into<String>) -> Result<Self, BoundaryError> {
        let boundary = boundary.into();
        match validate_boundary(&boundary) {
            Ok(()) => Ok(Self::with_validated_boundary(stream, boundary)),
            Err(reason) => Err(BoundaryError { boundary, reason }),
        }
    }

    fn with_validated_boundary(stream: S, boundary: String) -> Self {
        if let Some(content_type) = content_type_for(&boundary) {
            return Self {
                stream,
                boundary,
                content_type,
            };
        }
        // Unreachable: a validated boundary is an HTTP token, and `new` uses a
        // hex boundary — both are legal header values. If that invariant is ever
        // broken we must NOT emit the old silent fallback (a bare
        // `multipart/mixed` with no `boundary=`), which disagreed with the
        // `--<boundary>` delimiters the framer still writes and produced an
        // unparseable body. Substitute a known-good boundary and its matching
        // header together so the two never desync. Panic-free: `expect`/`unwrap`
        // are denied.
        tracing::error!(
            boundary = ?boundary,
            "validated multipart/mixed boundary did not form a legal header value; substituting a known-good boundary"
        );
        Self {
            stream,
            boundary: FALLBACK_BOUNDARY.to_owned(),
            content_type: HeaderValue::from_static(FALLBACK_CONTENT_TYPE),
        }
    }

    /// The boundary this response will frame its parts with.
    #[must_use]
    pub fn boundary(&self) -> &str {
        &self.boundary
    }
}

// The error bound is `Into<Problem>` by deliberate design, not incidentally: the
// paired reader (`toolkit_contract::runtime::multipart`) decodes an error part
// as an RFC 9457 `Problem`, and `Problem` is the platform's single canonical
// wire error across REST, SSE and gRPC. This framer and that reader are two
// halves of one protocol and must agree on the error envelope, so the framer
// speaks `Problem` rather than being generic over the error format.
impl<S, T, E> IntoResponse for MultipartJsonStream<S>
where
    S: Stream<Item = Result<T, E>> + Send + 'static,
    T: Serialize,
    E: Into<Problem>,
{
    fn into_response(self) -> Response {
        let mut response = Response::new(Body::from_stream(frame(self.stream, self.boundary)));
        response
            .headers_mut()
            .insert(CONTENT_TYPE, self.content_type);
        response
    }
}

/// Where the framer is in the response body.
struct FramerState<S> {
    stream: S,
    boundary: String,
    /// Set once the close delimiter has been written, or once an item failed
    /// to serialize — either way there is nothing further to emit.
    finished: bool,
}

/// Turn a stream of `Result<T, E>` items into the `multipart/mixed` body byte
/// stream. `Ok` items become `application/json` data parts; an `Err` becomes a
/// terminal `application/problem+json` error part (see [`MultipartJsonStream`]).
fn frame<S, T, E>(
    stream: S,
    boundary: String,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static
where
    S: Stream<Item = Result<T, E>> + Send + 'static,
    T: Serialize,
    E: Into<Problem>,
{
    let state = FramerState {
        stream: Box::pin(stream),
        boundary,
        finished: false,
    };

    futures_util::stream::unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        let Some(item) = state.stream.next().await else {
            // Clean end of the source stream: write the close delimiter, then
            // let the body end gracefully on the next poll.
            state.finished = true;
            let close = format!("--{}--\r\n", state.boundary);
            return Some((Ok(Bytes::from(close)), state));
        };
        let value = match item {
            Ok(value) => value,
            Err(e) => {
                // A post-open domain error. The status is long gone, so it
                // cannot be an HTTP error — but it must not be an abort either:
                // deliver it as a typed `application/problem+json` error part
                // followed by the close delimiter, so the reader surfaces
                // `Err(typed)` and a *clean* end. An error is terminal.
                state.finished = true;
                let mut problem: Problem = e.into();
                // A mid-stream failure after the `200` is invisible to the
                // status line; log it so it stays observable server-side.
                tracing::warn!(
                    status = ?problem.status,
                    error_code = ?problem.error_code,
                    "multipart/mixed stream ended with a domain error; framing it as a typed error part"
                );
                let Some(bytes) = encode_error_part_within_limit(&state.boundary, &mut problem)
                else {
                    // Even trimmed, the `Problem` will not fit or will not
                    // serialize — there is nothing well-formed left to frame, so
                    // abort the body rather than emit an unreadable part.
                    tracing::error!(
                        status = ?problem.status,
                        "multipart/mixed error part exceeds the maximum part size even after trimming, or would not serialize; aborting the response body"
                    );
                    return Some((
                        Err(std::io::Error::other(
                            "multipart/mixed error part exceeds the maximum part size even after trimming",
                        )),
                        state,
                    ));
                };
                return Some((Ok(bytes), state));
            }
        };
        match serde_json::to_vec(&value) {
            Ok(json) if json.len() > MAX_PART_BYTES => {
                // The reader rejects any part whose body exceeds its
                // accumulation ceiling, so emitting a larger part would produce
                // a stream the toolkit client cannot consume. The status is long
                // gone, so abort the body — exactly as the serialization-failure
                // arm below does — rather than write an unreadable part.
                tracing::error!(
                    item_type = std::any::type_name::<T>(),
                    item_bytes = json.len(),
                    max_bytes = MAX_PART_BYTES,
                    "multipart/mixed stream item exceeds the maximum part size; aborting the response body"
                );
                state.finished = true;
                Some((
                    Err(std::io::Error::other(format!(
                        "multipart/mixed stream item is {} bytes, exceeding the maximum part size of {MAX_PART_BYTES} bytes",
                        json.len()
                    ))),
                    state,
                ))
            }
            Ok(json) => {
                let bytes = encode_part(&state.boundary, &json);
                Some((Ok(bytes), state))
            }
            Err(e) => {
                // Q5: the status is long gone, so abort the body rather than
                // short it. Yielding an `Err` truncates the chunked encoding,
                // which the reader surfaces as a transport error; returning
                // `None` here would be a graceful EOF and would read as a
                // clean, complete stream.
                tracing::error!(
                    item_type = std::any::type_name::<T>(),
                    error = %e,
                    "failed to serialize a multipart/mixed stream item; aborting the response body"
                );
                state.finished = true;
                Some((
                    Err(std::io::Error::other(format!(
                        "failed to serialize a multipart/mixed stream item: {e}"
                    ))),
                    state,
                ))
            }
        }
    })
}

fn encode_part(boundary: &str, json: &[u8]) -> Bytes {
    let header = format!(
        "--{boundary}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        json.len()
    );
    let mut out = Vec::with_capacity(header.len() + json.len() + 2);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(json);
    out.extend_from_slice(b"\r\n");
    Bytes::from(out)
}

/// Encode a terminal error part — one `application/problem+json` part carrying
/// the serialized [`Problem`] — immediately followed by the close delimiter.
///
/// Emitting the close in the same chunk is what makes an error a *clean* end:
/// the reader decodes the part into a typed `Err`, then sees the closing
/// delimiter and reports a graceful close, never a truncation or a manufactured
/// missing-terminator error.
fn encode_error_part_and_close(boundary: &str, problem_json: &[u8]) -> Bytes {
    let header = format!(
        "--{boundary}\r\nContent-Type: {PROBLEM_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n",
        problem_json.len()
    );
    let close = format!("\r\n--{boundary}--\r\n");
    let mut out = Vec::with_capacity(header.len() + problem_json.len() + close.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(problem_json);
    out.extend_from_slice(close.as_bytes());
    Bytes::from(out)
}

/// Serialize `problem` as a terminal error part (+ close), keeping it within
/// [`MAX_PART_BYTES`].
///
/// If the full `Problem` would exceed the reader's ceiling, its two unbounded
/// fields (`detail`, `context`) are dropped and it is retried, so the *typed*
/// error — `status`, `type`, `error_code`/`error_domain` — still reaches the
/// client rather than degrading to a truncation. Returns `None` only when even
/// the trimmed form will not fit or will not serialize; the caller aborts then.
///
/// This trimming is for a real domain `Err`. An oversized or unserializable
/// `Ok` *data* item is a different case (a value the framer cannot render at
/// all) and is deliberately aborted rather than replaced with a synthetic error.
fn encode_error_part_within_limit(boundary: &str, problem: &mut Problem) -> Option<Bytes> {
    let full = serde_json::to_vec(problem)
        .ok()
        .filter(|json| json.len() <= MAX_PART_BYTES);
    if let Some(json) = full {
        return Some(encode_error_part_and_close(boundary, &json));
    }
    problem.detail = String::new();
    problem.context = serde_json::Value::Null;
    let trimmed = serde_json::to_vec(problem)
        .ok()
        .filter(|json| json.len() <= MAX_PART_BYTES)?;
    Some(encode_error_part_and_close(boundary, &trimmed))
}

fn generated_boundary() -> String {
    // Hex only, so RFC 2046-legal by construction, and wide enough that it
    // cannot collide with item payload bytes in practice.
    uuid::Uuid::now_v7().simple().to_string()
}

/// Build the `Content-Type` for a boundary, or `None` if it does not form a
/// legal header value. A validated boundary (an HTTP token) always yields
/// `Some`; `None` is the unreachable guard handled in
/// [`MultipartJsonStream::with_validated_boundary`].
fn content_type_for(boundary: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!("multipart/mixed; boundary={boundary}")).ok()
}

/// Known-good boundary substituted only if a *validated* boundary somehow fails
/// to form a header value (unreachable). Hex, so it is both RFC 2046-legal and
/// an HTTP token. Kept byte-for-byte consistent with [`FALLBACK_CONTENT_TYPE`]
/// so the emitted `boundary=` always matches the `--<boundary>` delimiters.
const FALLBACK_BOUNDARY: &str = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";
const FALLBACK_CONTENT_TYPE: &str = "multipart/mixed; boundary=0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";

/// Check a boundary against RFC 2046 §5.1.1 **and** the HTTP token grammar
/// (RFC 7230 §3.2.6). `Err` carries a reason suitable for a log line.
///
/// RFC 2046 bchars additionally allow `( ) , / : = ?` and space, but those are
/// HTTP `tspecials`: a boundary containing one would have to be quoted to
/// survive a conformant `Content-Type` parser, yet this crate emits — and
/// reads — the `boundary=` parameter unquoted (see [`with_validated_boundary`]
/// and the client's `;`-splitting reader). Restricting to the intersection of
/// the two grammars keeps the emitted header unambiguous, so we reject those
/// characters rather than produce a `Content-Type` a conformant client would
/// read differently from this crate.
fn validate_boundary(boundary: &str) -> Result<(), &'static str> {
    if boundary.is_empty() {
        return Err("boundary must not be empty");
    }
    if boundary.len() > MAX_BOUNDARY_LEN {
        return Err("boundary exceeds 70 characters");
    }
    // Intersection of RFC 2046 bchars and HTTP token chars:
    //   DIGIT / ALPHA / "'" / "+" / "-" / "." / "_"
    // (space can't appear at all, so no trailing-space check is needed.)
    if !boundary
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"'+-._".contains(&b))
    {
        return Err("boundary contains a character outside the HTTP token set");
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde::Serialize;
    use toolkit_canonical_errors::CanonicalError;

    #[derive(Serialize)]
    struct Item {
        id: u32,
    }

    /// An item whose `Serialize` impl fails, to exercise the abort path — a
    /// value the framer cannot put on the wire at all (distinct from a domain
    /// `Err`, which becomes a typed error part).
    struct Unserializable;
    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("nope"))
        }
    }

    #[tokio::test]
    async fn frames_one_part_per_item_with_a_content_length() {
        let items = futures_util::stream::iter(vec![
            Ok::<_, CanonicalError>(Item { id: 1 }),
            Ok(Item { id: 2 }),
        ]);
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "multipart/mixed; boundary=BOUND"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            "--BOUND\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n{\"id\":1}\r\n\
             --BOUND\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n{\"id\":2}\r\n\
             --BOUND--\r\n"
        );
    }

    #[tokio::test]
    async fn an_empty_stream_is_just_the_close_delimiter() {
        let items = futures_util::stream::iter(Vec::<Result<Item, CanonicalError>>::new());
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "--BOUND--\r\n");
    }

    #[tokio::test]
    async fn an_unserializable_item_aborts_the_body_rather_than_ending_it() {
        // Q5's implementation trap: the body must ERROR, not end. If this ever
        // regresses to ending the stream, `to_bytes` succeeds and the reader
        // sees a clean, complete — but silently short — stream.
        let items = futures_util::stream::iter(vec![Ok::<_, CanonicalError>(Unserializable)]);
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        let result = to_bytes(response.into_body(), usize::MAX).await;
        assert!(
            result.is_err(),
            "the body must abort, not end gracefully; got {:?}",
            result.map(|b| String::from_utf8_lossy(&b).into_owned())
        );
    }

    #[tokio::test]
    async fn an_err_item_becomes_a_typed_error_part_then_a_clean_close() {
        // A domain `Err` is NOT an abort: it is framed as a terminal
        // `application/problem+json` part followed by the close delimiter, so
        // the body ends cleanly and the reader can surface `Err(typed)`.
        let items = futures_util::stream::iter(vec![
            Ok(Item { id: 1 }),
            Err(CanonicalError::internal("mid-stream boom").create()),
            // Anything after the error is not framed — an error is terminal.
            Ok(Item { id: 2 }),
        ]);
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        // The body must END (not abort): `to_bytes` succeeds.
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("an error item ends the body cleanly, it must not abort");
        let text = String::from_utf8(body.to_vec()).unwrap();
        // One data part, then the problem+json error part, then the close —
        // and no second data part (the error is terminal).
        assert!(
            text.starts_with(
                "--BOUND\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n{\"id\":1}\r\n"
            ),
            "expected the data part first; got:\n{text}"
        );
        assert!(
            text.contains("Content-Type: application/problem+json"),
            "expected a problem+json error part; got:\n{text}"
        );
        assert!(
            text.ends_with("--BOUND--\r\n"),
            "expected a clean close; got:\n{text}"
        );
        assert!(
            !text.contains("{\"id\":2}"),
            "an error is terminal; later items must not be framed; got:\n{text}"
        );
    }

    #[tokio::test]
    async fn an_oversized_error_problem_is_trimmed_not_aborted() {
        // A domain error whose `Problem` exceeds `MAX_PART_BYTES` must still
        // reach the client as a typed error part (with its unbounded fields
        // trimmed), NOT degrade to a truncation/abort. Contrast with an oversized
        // `Ok` data item, which does abort.
        let huge = "a".repeat(MAX_PART_BYTES + 4096);
        let items = futures_util::stream::iter(vec![Err::<Item, CanonicalError>(
            CanonicalError::internal(huge).create(),
        )]);
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        // The body must END cleanly (trimmed part + close), not abort.
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("an oversized error Problem must be trimmed and framed, not aborted");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("Content-Type: application/problem+json"),
            "expected a problem+json error part; got {} bytes",
            body.len()
        );
        assert!(
            text.ends_with("--BOUND--\r\n"),
            "expected a clean close; got {} bytes",
            body.len()
        );
        // Trimmed: the ~16 MiB detail was dropped, so the whole body is small.
        assert!(
            body.len() < MAX_PART_BYTES,
            "the oversized detail must be trimmed away; body was {} bytes",
            body.len()
        );
    }

    #[tokio::test]
    async fn an_oversized_item_aborts_the_body_rather_than_emitting_an_unreadable_part() {
        // A part larger than the reader's accumulation ceiling would produce a
        // stream the toolkit client cannot consume, so the framer must abort the
        // body (error), not emit the part. `String` serializes to its own bytes
        // plus two quotes, so this clears `MAX_PART_BYTES`.
        let oversized = "a".repeat(MAX_PART_BYTES);
        let items = futures_util::stream::iter(vec![Ok::<_, CanonicalError>(oversized)]);
        let response = MultipartJsonStream::with_boundary(items, "BOUND")
            .unwrap()
            .into_response();
        let result = to_bytes(response.into_body(), usize::MAX).await;
        assert!(
            result.is_err(),
            "an oversized item must abort the body, not emit an unreadable part; got {:?}",
            result.map(|b| b.len())
        );
    }

    #[test]
    fn the_emitted_content_type_carries_the_framing_boundary() {
        // The invariant #19 is about: the header's `boundary=` must match the
        // `--<boundary>` delimiters the framer writes, never degrade to a
        // boundary-less `multipart/mixed`.
        let framer = MultipartJsonStream::with_boundary(
            futures_util::stream::iter(Vec::<Result<Item, CanonicalError>>::new()),
            "abc123",
        )
        .unwrap();
        let expected = format!("multipart/mixed; boundary={}", framer.boundary());
        let response = framer.into_response();
        assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), &expected);
    }

    #[test]
    fn the_fallback_boundary_and_header_stay_consistent() {
        // The unreachable substitution relies on these two constants agreeing.
        assert_eq!(
            FALLBACK_CONTENT_TYPE,
            format!("multipart/mixed; boundary={FALLBACK_BOUNDARY}")
        );
        assert!(validate_boundary(FALLBACK_BOUNDARY).is_ok());
        assert!(content_type_for(FALLBACK_BOUNDARY).is_some());
    }

    #[test]
    fn a_generated_boundary_is_used_when_none_is_given() {
        let items = futures_util::stream::iter(Vec::<Result<Item, CanonicalError>>::new());
        let framer = MultipartJsonStream::new(items);
        assert!(validate_boundary(framer.boundary()).is_ok());
        assert!(!framer.boundary().is_empty());
    }

    #[test]
    fn an_illegal_boundary_is_rejected_not_silently_substituted() {
        let items = futures_util::stream::iter(Vec::<Result<Item, CanonicalError>>::new());
        // `MultipartJsonStream` isn't `Debug`, so match rather than `unwrap_err`.
        let Err(err) = MultipartJsonStream::with_boundary(items, "not\r\nlegal") else {
            panic!("an illegal boundary must be rejected");
        };
        // The rejected value is escaped in the message, not written raw.
        assert!(
            err.to_string().contains("not\\r\\nlegal"),
            "message should escape control chars: {err}"
        );
        // The caller can still opt into a generated boundary via `new`.
        let framer = MultipartJsonStream::new(futures_util::stream::iter(Vec::<
            Result<Item, CanonicalError>,
        >::new()));
        assert!(validate_boundary(framer.boundary()).is_ok());
    }

    #[test]
    fn boundary_validation_requires_an_rfc_2046_http_token() {
        // Characters in both RFC 2046 bchars and the HTTP token set.
        assert!(validate_boundary("abcABC012'+-._").is_ok());
        assert!(validate_boundary("").is_err());
        assert!(validate_boundary(&"a".repeat(MAX_BOUNDARY_LEN + 1)).is_err());
        // RFC 2046-legal but HTTP tspecials: rejected so the emitted
        // Content-Type can't be misparsed by a conformant client.
        assert!(validate_boundary("has space").is_err());
        assert!(validate_boundary("a/b").is_err());
        assert!(validate_boundary("a:b=c?").is_err());
        assert!(validate_boundary("(paren)").is_err());
        assert!(validate_boundary("comma,d").is_err());
        // Not even RFC 2046-legal.
        assert!(validate_boundary("semi;colon").is_err());
        assert!(validate_boundary("quote\"d").is_err());
    }
}

use bytes::Bytes;
use futures_util::StreamExt;

use crate::body::{Body, BodyStream, BoxError};

// ---------------------------------------------------------------------------
// PartBody
// ---------------------------------------------------------------------------

/// Payload of a single multipart part.
pub enum PartBody {
    /// Fully buffered bytes (text fields, small files).
    Bytes(Bytes),
    /// Streaming bytes (large files, piped data).
    Stream(BodyStream),
}

impl std::fmt::Debug for PartBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartBody::Bytes(b) => write!(f, "PartBody::Bytes({} bytes)", b.len()),
            PartBody::Stream(_) => write!(f, "PartBody::Stream(...)"),
        }
    }
}

impl From<Bytes> for PartBody {
    fn from(b: Bytes) -> Self {
        PartBody::Bytes(b)
    }
}

impl From<Vec<u8>> for PartBody {
    fn from(v: Vec<u8>) -> Self {
        PartBody::Bytes(Bytes::from(v))
    }
}

impl From<String> for PartBody {
    fn from(s: String) -> Self {
        PartBody::Bytes(Bytes::from(s))
    }
}

impl From<&'static str> for PartBody {
    fn from(s: &'static str) -> Self {
        PartBody::Bytes(Bytes::from(s))
    }
}

impl From<BodyStream> for PartBody {
    fn from(s: BodyStream) -> Self {
        PartBody::Stream(s)
    }
}

// ---------------------------------------------------------------------------
// Part
// ---------------------------------------------------------------------------

/// A single part in a `multipart/form-data` body.
pub struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    body: PartBody,
}

impl Part {
    /// Create a text field part.
    pub fn text(name: &str, value: impl Into<String>) -> Self {
        Part {
            name: name.to_owned(),
            filename: None,
            content_type: None,
            body: PartBody::Bytes(Bytes::from(value.into())),
        }
    }

    /// Create a binary part from a buffered payload.
    pub fn bytes(name: &str, data: impl Into<Bytes>) -> Self {
        Part {
            name: name.to_owned(),
            filename: None,
            content_type: None,
            body: PartBody::Bytes(data.into()),
        }
    }

    /// Create a streaming part from an async byte stream.
    pub fn stream(name: &str, stream: BodyStream) -> Self {
        Part {
            name: name.to_owned(),
            filename: None,
            content_type: None,
            body: PartBody::Stream(stream),
        }
    }

    /// Set the filename parameter in Content-Disposition.
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the Content-Type header for this part.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// Field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Filename, if set.
    pub fn get_filename(&self) -> Option<&str> {
        self.filename.as_deref()
    }

    /// Content-Type, if set.
    pub fn get_content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// Returns true if this part has a streaming body.
    pub fn is_streaming(&self) -> bool {
        matches!(self.body, PartBody::Stream(_))
    }
}

impl std::fmt::Debug for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part")
            .field("name", &self.name)
            .field("filename", &self.filename)
            .field("content_type", &self.content_type)
            .field("body", &self.body)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// MultipartError
// ---------------------------------------------------------------------------

/// Errors from multipart body construction.
#[derive(Debug, thiserror::Error)]
pub enum MultipartError {
    /// The provided boundary string violates RFC 2046 constraints.
    #[error("invalid boundary: {reason}")]
    InvalidBoundary { reason: String },
}

// ---------------------------------------------------------------------------
// MultipartBody
// ---------------------------------------------------------------------------

/// Builder for `multipart/form-data` request bodies.
pub struct MultipartBody {
    boundary: String,
    parts: Vec<Part>,
}

impl std::fmt::Debug for MultipartBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultipartBody")
            .field("boundary", &self.boundary)
            .field("parts", &self.parts)
            .finish()
    }
}

impl MultipartBody {
    /// Create a new builder with an auto-generated UUID boundary.
    pub fn new() -> Self {
        MultipartBody {
            boundary: uuid::Uuid::new_v4().simple().to_string(),
            parts: Vec::new(),
        }
    }

    /// Create a builder with a caller-provided boundary.
    ///
    /// Validates RFC 2046 constraints: max 70 chars, allowed characters only.
    pub fn with_boundary(boundary: impl Into<String>) -> Result<Self, MultipartError> {
        let boundary = boundary.into();
        if boundary.is_empty() {
            return Err(MultipartError::InvalidBoundary {
                reason: "boundary must not be empty".into(),
            });
        }
        if boundary.len() > 70 {
            return Err(MultipartError::InvalidBoundary {
                reason: format!("boundary exceeds 70 characters ({})", boundary.len()),
            });
        }
        // RFC 2046 bchars: DIGIT / ALPHA / "'" / "(" / ")" / "+" / "_" /
        //                  "," / "-" / "." / "/" / ":" / "=" / "?" / " "
        for (i, b) in boundary.bytes().enumerate() {
            let allowed = b.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&b);
            if !allowed {
                return Err(MultipartError::InvalidBoundary {
                    reason: format!("illegal character at position {i}"),
                });
            }
        }
        // RFC 2046: boundary must not end with a space.
        if boundary.ends_with(' ') {
            return Err(MultipartError::InvalidBoundary {
                reason: "boundary must not end with a space".into(),
            });
        }
        Ok(MultipartBody {
            boundary,
            parts: Vec::new(),
        })
    }

    /// Append a pre-built part.
    pub fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Shorthand for `self.part(Part::text(name, value))`.
    pub fn text(self, name: &str, value: impl Into<String>) -> Self {
        self.part(Part::text(name, value))
    }

    /// Shorthand for `self.part(Part::bytes(name, data))`.
    pub fn bytes(self, name: &str, data: impl Into<Bytes>) -> Self {
        self.part(Part::bytes(name, data))
    }

    /// Returns the `Content-Type` header value string.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// Returns the `Content-Type` as a pre-parsed `HeaderValue`.
    pub fn content_type_header_value(&self) -> http::HeaderValue {
        // Boundary chars are validated to be ASCII visible, so this cannot panic.
        http::HeaderValue::from_str(&self.content_type())
            .expect("boundary contains only valid header value characters")
    }

    /// Returns true if any part has a streaming body.
    pub fn has_streaming_parts(&self) -> bool {
        self.parts.iter().any(|p| p.is_streaming())
    }

    /// Serialize all parts into a `Body`.
    ///
    /// If all parts are buffered, returns `Body::Bytes` (synchronous).
    /// If any part is streaming, returns `Body::Stream`.
    pub fn into_body(self) -> Body {
        if self.has_streaming_parts() {
            let state = StreamState {
                parts: self.parts,
                boundary: self.boundary,
                idx: 0,
                active_stream: None,
                phase: StreamPhase::Header,
            };
            Body::Stream(Box::pin(futures_util::stream::unfold(state, stream_unfold)))
        } else {
            Body::Bytes(encode_buffered(self.parts, &self.boundary))
        }
    }

    /// Build an `http::Request` with the Content-Type header set.
    pub fn into_request<M, U>(self, method: M, uri: U) -> Result<http::Request<Body>, http::Error>
    where
        http::Method: TryFrom<M>,
        <http::Method as TryFrom<M>>::Error: Into<http::Error>,
        http::Uri: TryFrom<U>,
        <http::Uri as TryFrom<U>>::Error: Into<http::Error>,
    {
        let ct = self.content_type_header_value();
        let body = self.into_body();
        http::Request::builder()
            .method(method)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, ct)
            .body(body)
    }
}

impl Default for MultipartBody {
    fn default() -> Self {
        Self::new()
    }
}

impl From<MultipartBody> for Body {
    fn from(multipart: MultipartBody) -> Self {
        multipart.into_body()
    }
}

// ---------------------------------------------------------------------------
// Private serialization helpers
// ---------------------------------------------------------------------------

/// Strip CR and LF characters to prevent CRLF header injection.
fn sanitize_for_header(value: &str) -> String {
    value.replace(['\r', '\n'], "")
}

/// Escape `"` inside a quoted Content-Disposition parameter and strip CR/LF.
fn escape_quoted_value(value: &str) -> String {
    sanitize_for_header(value).replace('"', "\\\"")
}

fn encode_part_headers(part: &Part, boundary: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"--");
    buf.extend_from_slice(boundary.as_bytes());
    buf.extend_from_slice(b"\r\n");

    // Content-Disposition
    buf.extend_from_slice(b"Content-Disposition: form-data; name=\"");
    buf.extend_from_slice(escape_quoted_value(&part.name).as_bytes());
    buf.push(b'"');

    if let Some(ref filename) = part.filename {
        buf.extend_from_slice(b"; filename=\"");
        buf.extend_from_slice(escape_quoted_value(filename).as_bytes());
        buf.push(b'"');
    }
    buf.extend_from_slice(b"\r\n");

    // Content-Type (optional)
    if let Some(ref ct) = part.content_type {
        buf.extend_from_slice(b"Content-Type: ");
        buf.extend_from_slice(sanitize_for_header(ct).as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    // Blank line separating headers from body
    buf.extend_from_slice(b"\r\n");
    buf
}

fn encode_buffered(parts: Vec<Part>, boundary: &str) -> Bytes {
    let mut buf = Vec::new();
    for part in &parts {
        buf.extend_from_slice(&encode_part_headers(part, boundary));
        match &part.body {
            PartBody::Bytes(b) => buf.extend_from_slice(b),
            PartBody::Stream(_) => unreachable!("encode_buffered called with streaming part"),
        }
        buf.extend_from_slice(b"\r\n");
    }
    buf.extend_from_slice(b"--");
    buf.extend_from_slice(boundary.as_bytes());
    buf.extend_from_slice(b"--\r\n");
    Bytes::from(buf)
}

// ---------------------------------------------------------------------------
// Streaming serialization
// ---------------------------------------------------------------------------

struct StreamState {
    parts: Vec<Part>,
    boundary: String,
    idx: usize,
    active_stream: Option<BodyStream>,
    phase: StreamPhase,
}

enum StreamPhase {
    Header,
    BufferedBody,
    StreamBody,
    TrailingCrlf,
    Close,
    Done,
}

async fn stream_unfold(mut state: StreamState) -> Option<(Result<Bytes, BoxError>, StreamState)> {
    loop {
        match state.phase {
            StreamPhase::Header => {
                if state.idx >= state.parts.len() {
                    state.phase = StreamPhase::Close;
                    continue;
                }
                let headers = encode_part_headers(&state.parts[state.idx], &state.boundary);
                if state.parts[state.idx].is_streaming() {
                    let body = std::mem::replace(
                        &mut state.parts[state.idx].body,
                        PartBody::Bytes(Bytes::new()),
                    );
                    if let PartBody::Stream(s) = body {
                        state.active_stream = Some(s);
                    }
                    state.phase = StreamPhase::StreamBody;
                } else {
                    state.phase = StreamPhase::BufferedBody;
                }
                return Some((Ok(Bytes::from(headers)), state));
            }
            StreamPhase::BufferedBody => {
                let bytes = match &state.parts[state.idx].body {
                    PartBody::Bytes(b) => b.clone(),
                    PartBody::Stream(_) => unreachable!(),
                };
                state.phase = StreamPhase::TrailingCrlf;
                return Some((Ok(bytes), state));
            }
            StreamPhase::StreamBody => {
                let mut stream = state.active_stream.take().expect("active stream missing");
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        state.active_stream = Some(stream);
                        return Some((Ok(chunk), state));
                    }
                    Some(Err(e)) => {
                        state.phase = StreamPhase::Done;
                        return Some((Err(e), state));
                    }
                    None => {
                        state.phase = StreamPhase::TrailingCrlf;
                        continue;
                    }
                }
            }
            StreamPhase::TrailingCrlf => {
                state.idx += 1;
                state.phase = StreamPhase::Header;
                return Some((Ok(Bytes::from_static(b"\r\n")), state));
            }
            StreamPhase::Close => {
                let closing = format!("--{}--\r\n", state.boundary);
                state.phase = StreamPhase::Done;
                return Some((Ok(Bytes::from(closing)), state));
            }
            StreamPhase::Done => {
                return None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "multipart_tests.rs"]
mod multipart_tests;

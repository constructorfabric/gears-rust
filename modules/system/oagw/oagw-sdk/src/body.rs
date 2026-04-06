use std::pin::Pin;

use bytes::Bytes;
use futures_core::Stream;

/// Boxed error type for body stream errors.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A streaming body.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, BoxError>> + Send>>;

/// Unified body type for gateway proxy requests and responses.
///
/// Covers every protocol through a single `proxy_request` call:
/// - `Empty` — no body (GET, HEAD, DELETE)
/// - `Bytes` — buffered body (small JSON payloads, typical API calls)
/// - `Stream` — streaming body (SSE, chunked transfer, large payloads,
///   **and WebSocket messages** serialized as byte chunks)
///
/// # Protocol mapping
///
/// | Protocol  | Request Body          | Response Body          |
/// |-----------|-----------------------|------------------------|
/// | HTTP      | `Body::Bytes`/`Empty` | `Body::Bytes`          |
/// | SSE       | `Body::Bytes`/`Empty` | `Body::Stream`         |
/// | WebSocket | `Body::Stream`        | `Body::Stream`         |
pub enum Body {
    /// No body.
    Empty,
    /// Fully buffered body.
    Bytes(Bytes),
    /// Streaming body (SSE responses, WebSocket messages, chunked transfers).
    Stream(BodyStream),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Body::Empty => write!(f, "Body::Empty"),
            Body::Bytes(b) => write!(f, "Body::Bytes({} bytes)", b.len()),
            Body::Stream(_) => write!(f, "Body::Stream(...)"),
        }
    }
}

impl Body {
    /// Returns `true` if this is an empty body.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Body::Empty)
    }

    /// Consume this body into `Bytes`, buffering a stream if necessary.
    ///
    /// For `Body::Stream`, reads the entire stream into memory. Use with
    /// caution on unbounded streams (SSE, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error if a stream chunk fails.
    pub async fn into_bytes(self) -> Result<Bytes, BoxError> {
        match self {
            Body::Empty => Ok(Bytes::new()),
            Body::Bytes(b) => Ok(b),
            Body::Stream(mut s) => {
                use futures_util::StreamExt;
                let mut buf = Vec::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(Bytes::from(buf))
            }
        }
    }

    /// Extract the inner `BodyStream`, converting other variants as needed.
    ///
    /// - `Empty` → empty stream
    /// - `Bytes` → single-item stream
    /// - `Stream` → pass-through
    pub fn into_stream(self) -> BodyStream {
        match self {
            Body::Empty => Box::pin(futures_util::stream::empty()),
            Body::Bytes(b) => Box::pin(futures_util::stream::once(async { Ok(b) })),
            Body::Stream(s) => s,
        }
    }

    /// Try to extract the inner `Bytes`.
    ///
    /// Returns `Err(self)` if this is not `Body::Bytes`.
    pub fn try_into_bytes(self) -> Result<Bytes, Self> {
        match self {
            Body::Bytes(b) => Ok(b),
            other => Err(other),
        }
    }

    /// Try to extract the inner `BodyStream`.
    ///
    /// Returns `Err(self)` if this is not `Body::Stream`.
    pub fn try_into_stream(self) -> Result<BodyStream, Self> {
        match self {
            Body::Stream(s) => Ok(s),
            other => Err(other),
        }
    }
}

impl From<()> for Body {
    fn from((): ()) -> Self {
        Body::Empty
    }
}

impl From<Bytes> for Body {
    fn from(b: Bytes) -> Self {
        if b.is_empty() {
            Body::Empty
        } else {
            Body::Bytes(b)
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self {
        Bytes::from(v).into()
    }
}

impl From<String> for Body {
    fn from(s: String) -> Self {
        Bytes::from(s).into()
    }
}

impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Bytes::from(s).into()
    }
}

impl From<BodyStream> for Body {
    fn from(s: BodyStream) -> Self {
        Body::Stream(s)
    }
}

#[cfg(test)]
#[path = "body_tests.rs"]
mod body_tests;

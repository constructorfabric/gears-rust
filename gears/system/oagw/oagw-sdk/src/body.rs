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
    ///
    /// As a request body it is sent to the upstream as it is read. Its
    /// length is the request's `Content-Length` header when one is set: the
    /// upstream then gets that `Content-Length` instead of chunked framing,
    /// and a stream that yields more or fewer bytes fails the request (400,
    /// `INVALID_CONTENT_LENGTH`). A declared length over the gateway's body
    /// limit is refused (413) before the stream is read. A stream error
    /// fails the request (400, `BODY_READ_FAILED`), unless the upstream has
    /// already answered: its response is returned then.
    ///
    /// End the stream right after its last byte. With a `Content-Length`
    /// the request completes at that byte without waiting for the end, so
    /// data the stream yields later is not seen; without one the request
    /// waits for the end.
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
mod tests {
    use super::*;

    #[test]
    fn body_empty_is_empty() {
        assert!(Body::Empty.is_empty());
    }

    #[test]
    fn body_bytes_not_empty() {
        let body = Body::from(Bytes::from("hello"));
        assert!(!body.is_empty());
    }

    #[test]
    fn empty_bytes_becomes_empty_body() {
        let body = Body::from(Bytes::new());
        assert!(body.is_empty());
    }

    #[test]
    fn from_string() {
        let body = Body::from("hello".to_string());
        assert!(!body.is_empty());
        assert!(matches!(body, Body::Bytes(_)));
    }

    #[test]
    fn from_static_str() {
        let body = Body::from("hello");
        assert!(matches!(body, Body::Bytes(_)));
    }

    #[test]
    fn from_vec() {
        let body = Body::from(vec![1, 2, 3]);
        assert!(matches!(body, Body::Bytes(_)));
    }

    #[test]
    fn from_unit() {
        let body = Body::from(());
        assert!(body.is_empty());
    }

    #[test]
    fn debug_does_not_leak_content() {
        let body = Body::from(Bytes::from("secret-data"));
        let debug = format!("{body:?}");
        assert!(debug.contains("11 bytes"));
        assert!(!debug.contains("secret-data"));
    }

    #[test]
    fn debug_empty() {
        assert_eq!(format!("{:?}", Body::Empty), "Body::Empty");
    }

    #[test]
    fn debug_stream() {
        let stream: BodyStream = Box::pin(futures_util::stream::empty());
        let body = Body::Stream(stream);
        assert_eq!(format!("{body:?}"), "Body::Stream(...)");
    }

    #[tokio::test]
    async fn into_bytes_from_empty() {
        let bytes = Body::Empty.into_bytes().await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn into_bytes_from_bytes() {
        let original = Bytes::from("hello");
        let bytes = Body::Bytes(original.clone()).into_bytes().await.unwrap();
        assert_eq!(bytes, original);
    }

    #[tokio::test]
    async fn into_bytes_from_stream() {
        let chunks = vec![Ok(Bytes::from("hel")), Ok(Bytes::from("lo"))];
        let stream: BodyStream = Box::pin(futures_util::stream::iter(chunks));
        let bytes = Body::Stream(stream).into_bytes().await.unwrap();
        assert_eq!(bytes, Bytes::from("hello"));
    }

    #[test]
    fn try_into_bytes_succeeds() {
        let body = Body::Bytes(Bytes::from("data"));
        assert_eq!(body.try_into_bytes().unwrap(), Bytes::from("data"));
    }

    #[test]
    fn try_into_bytes_fails_on_empty() {
        let body = Body::Empty;
        assert!(body.try_into_bytes().is_err());
    }

    #[test]
    fn try_into_stream_fails_on_bytes() {
        let body = Body::Bytes(Bytes::from("data"));
        assert!(body.try_into_stream().is_err());
    }
}

use crate::builder::HttpClientBuilder;
use crate::config::TransportSecurity;
use crate::error::HttpError;
use crate::request::RequestBuilder;
use crate::response::ResponseBody;
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use std::future::Future;
use std::pin::Pin;
use tower::Service;
use tower::buffer::Buffer;

/// Type alias for the future type of the inner service
pub type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<Response<ResponseBody>, HttpError>> + Send>>;

/// Type alias for the buffered service
/// Buffer<Req, F> in tower 0.5 where Req is the request type and F is the service future type
pub type BufferedService = Buffer<Request<Full<Bytes>>, ServiceFuture>;

/// HTTP client with tower middleware stack
///
/// This client provides a clean interface over a tower service stack that includes:
/// - Timeout handling
/// - Automatic retries with exponential backoff
/// - User-Agent header injection
/// - Concurrency limiting (optional)
///
/// Use [`HttpClientBuilder`] to construct instances with custom configuration.
///
/// # Thread Safety
///
/// `HttpClient` is `Clone + Send + Sync`. Cloning is cheap (internal channel clone).
/// The client uses `tower::buffer::Buffer` internally, which allows true concurrent
/// access without any mutex serialization. Callers do NOT need to wrap `HttpClient`
/// in `Mutex` or `Arc<Mutex<_>>`.
///
/// # Example
///
/// ```ignore
/// // Just store the client directly - no Mutex needed!
/// struct MyService {
///     http: HttpClient,
/// }
///
/// impl MyService {
///     async fn fetch(&self) -> Result<Data, HttpError> {
///         // reqwest-like API: response has body-reading methods
///         self.http.get("https://example.com/api").await?.json().await
///     }
/// }
/// ```
#[derive(Clone)]
pub struct HttpClient {
    pub(crate) service: BufferedService,
    pub(crate) max_body_size: usize,
    pub(crate) transport_security: TransportSecurity,
}

impl HttpClient {
    /// Create a new HTTP client with default configuration
    ///
    /// # Errors
    /// Returns an error if TLS initialization fails
    pub fn new() -> Result<Self, HttpError> {
        HttpClientBuilder::new().build()
    }

    /// Create a builder for configuring the HTTP client
    #[must_use]
    pub fn builder() -> HttpClientBuilder {
        HttpClientBuilder::new()
    }

    /// Create a GET request builder
    ///
    /// Returns a [`RequestBuilder`] that can be configured with headers
    /// before sending with `.send().await`.
    ///
    /// # URL Requirements
    ///
    /// The URL must be an absolute URI with scheme and authority (host).
    /// Relative URLs like `/path` or `example.com/path` are rejected with
    /// [`HttpError::InvalidUri`].
    ///
    /// Valid examples:
    /// - `https://api.example.com/users`
    /// - `http://localhost:8080/health` (requires [`TransportSecurity::AllowInsecureHttp`])
    ///
    /// # URL Construction
    ///
    /// Query parameters must be encoded into the URL externally (e.g. via `url::Url`):
    ///
    /// ```ignore
    /// use url::Url;
    ///
    /// let mut url = Url::parse("https://api.example.com/search")?;
    /// url.query_pairs_mut().append_pair("q", "rust").append_pair("page", "1");
    ///
    /// let resp = client
    ///     .get(url.as_str())
    ///     .header("authorization", "Bearer token")
    ///     .send()
    ///     .await?;
    /// ```
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Simple GET
    /// let resp = client.get("https://api.example.com/data").send().await?;
    /// ```
    ///
    /// [`HttpError::InvalidUri`]: crate::error::HttpError::InvalidUri
    /// [`TransportSecurity::AllowInsecureHttp`]: crate::config::TransportSecurity::AllowInsecureHttp
    pub fn get(&self, url: &str) -> RequestBuilder {
        RequestBuilder::new(
            self.service.clone(),
            self.max_body_size,
            http::Method::GET,
            url.to_owned(),
            self.transport_security,
        )
    }

    /// Create a POST request builder
    ///
    /// Returns a [`RequestBuilder`] that can be configured with headers,
    /// body (JSON, form, bytes), etc. before sending with `.send().await`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // POST with JSON body
    /// let resp = client
    ///     .post("https://api.example.com/users")
    ///     .json(&NewUser { name: "Alice" })?
    ///     .send()
    ///     .await?;
    ///
    /// // POST with form body
    /// let resp = client
    ///     .post("https://auth.example.com/token")
    ///     .form(&[("grant_type", "client_credentials")])?
    ///     .send()
    ///     .await?;
    /// ```
    pub fn post(&self, url: &str) -> RequestBuilder {
        RequestBuilder::new(
            self.service.clone(),
            self.max_body_size,
            http::Method::POST,
            url.to_owned(),
            self.transport_security,
        )
    }

    /// Create a PUT request builder
    ///
    /// Returns a [`RequestBuilder`] that can be configured with headers,
    /// body (JSON, form, bytes), etc. before sending with `.send().await`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .put("https://api.example.com/resource/1")
    ///     .json(&UpdateData { value: 42 })?
    ///     .send()
    ///     .await?;
    /// ```
    pub fn put(&self, url: &str) -> RequestBuilder {
        RequestBuilder::new(
            self.service.clone(),
            self.max_body_size,
            http::Method::PUT,
            url.to_owned(),
            self.transport_security,
        )
    }

    /// Create a PATCH request builder
    ///
    /// Returns a [`RequestBuilder`] that can be configured with headers,
    /// body (JSON, form, bytes), etc. before sending with `.send().await`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .patch("https://api.example.com/resource/1")
    ///     .json(&PatchData { field: "new_value" })?
    ///     .send()
    ///     .await?;
    /// ```
    pub fn patch(&self, url: &str) -> RequestBuilder {
        RequestBuilder::new(
            self.service.clone(),
            self.max_body_size,
            http::Method::PATCH,
            url.to_owned(),
            self.transport_security,
        )
    }

    /// Create a DELETE request builder
    ///
    /// Returns a [`RequestBuilder`] that can be configured with headers
    /// before sending with `.send().await`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .delete("https://api.example.com/resource/42")
    ///     .header("authorization", "Bearer token")
    ///     .send()
    ///     .await?;
    /// ```
    pub fn delete(&self, url: &str) -> RequestBuilder {
        RequestBuilder::new(
            self.service.clone(),
            self.max_body_size,
            http::Method::DELETE,
            url.to_owned(),
            self.transport_security,
        )
    }
}

/// Map buffer errors to `HttpError`
///
/// Buffer can return `ServiceError` which wraps the inner service error,
/// or `Closed` if the buffer worker has shut down.
pub fn map_buffer_error(err: tower::BoxError) -> HttpError {
    // Try to downcast to HttpError (from inner service)
    match err.downcast::<HttpError>() {
        Ok(http_err) => *http_err,
        Err(err) => {
            // Buffer closed or other internal failure.
            // This happens when buffer worker panics or channel is dropped.
            //
            // Return ServiceClosed (not Overloaded) to distinguish from normal
            // overload (buffer full). This is a serious condition indicating
            // the background worker has died unexpectedly.
            tracing::error!(
                error = %err,
                "buffer worker closed unexpectedly; service unavailable"
            );
            HttpError::ServiceClosed
        }
    }
}

/// Try to acquire a buffer slot with fail-fast semantics.
///
/// If the buffer is full, returns `HttpError::Overloaded` immediately instead
/// of blocking. This prevents request pile-up under load.
pub async fn try_acquire_buffer_slot(service: &mut BufferedService) -> Result<(), HttpError> {
    use std::task::Poll;

    // Poll once to check if buffer has space available
    let poll_result = std::future::poll_fn(|cx| match service.poll_ready(cx) {
        Poll::Ready(result) => Poll::Ready(Some(result)),
        Poll::Pending => Poll::Ready(None), // Buffer full, don't block
    })
    .await;

    match poll_result {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(map_buffer_error(e)),
        None => Err(HttpError::Overloaded), // Buffer full, fail fast
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;

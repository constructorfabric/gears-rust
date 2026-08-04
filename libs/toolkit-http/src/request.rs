use crate::client::{BufferedService, map_buffer_error, try_acquire_buffer_slot};
use crate::config::TransportSecurity;
use crate::error::{HttpError, InvalidUriKind};
use crate::response::{HttpResponse, ResponseBody};
use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use serde::Serialize;
use tower::Service;

/// Body type for the request builder
#[derive(Clone, Debug)]
enum BodyKind {
    /// Empty body
    Empty,
    /// Raw bytes body
    Bytes(Bytes),
    /// JSON-serialized body (stored as bytes after serialization)
    Json(Bytes),
    /// Form URL-encoded body (stored as bytes after serialization)
    Form(Bytes),
}

/// Per-request label for the `request_type` metrics attribute.
///
/// Attach to a request via [`RequestBuilder::with_request_type`] to break down
/// `http.client.request.duration` metrics by logical operation (e.g.
/// `"tenants_resolve"`, `"token_fetch"`). This is the Rust analogue of
/// go-appkit's `NewContextWithRequestType` / `GetRequestTypeFromContext`.
///
/// The metrics layer reads this from request extensions when the `otel` feature
/// is enabled; setting it without the feature is a safe no-op.
///
/// # Example
///
/// ```ignore
/// client
///     .get("https://api.example.com/tenants/123")
///     .with_request_type("tenants_resolve")
///     .send()
///     .await?;
/// ```
#[derive(Clone, Debug)]
pub struct RequestType(pub std::borrow::Cow<'static, str>);

impl RequestType {
    /// Create a request type label from a static string.
    #[must_use]
    pub fn new(t: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self(t.into())
    }
}

/// HTTP request builder with fluent API
///
/// Created by [`HttpClient::get`], [`HttpClient::post`], etc.
/// Supports chaining headers and body configuration before sending
/// with [`send()`](RequestBuilder::send).
///
/// # URL Construction
///
/// This crate does **not** provide query-string composition. Build your URL
/// externally (e.g. via `url::Url`) and pass the final string to `HttpClient`:
///
/// ```ignore
/// use url::Url;
/// use toolkit_http::HttpClient;
///
/// let mut url = Url::parse("https://api.example.com/users")?;
/// url.query_pairs_mut()
///     .append_pair("page", "1")
///     .append_pair("limit", "10");
///
/// let client = HttpClient::builder().build()?;
/// let resp = client.get(url.as_str()).send().await?;
/// ```
///
/// # Example
///
/// ```ignore
/// use toolkit_http::HttpClient;
///
/// let client = HttpClient::builder().build()?;
///
/// // Simple GET
/// let resp = client
///     .get("https://api.example.com/users")
///     .send()
///     .await?;
///
/// // POST with JSON body
/// let resp = client
///     .post("https://api.example.com/users")
///     .header("x-request-id", "123")
///     .json(&NewUser { name: "Alice" })?
///     .send()
///     .await?;
///
/// // POST with form body
/// let resp = client
///     .post("https://auth.example.com/token")
///     .header("authorization", "Basic xyz")
///     .form(&[("grant_type", "client_credentials")])?
///     .send()
///     .await?;
/// ```
#[must_use = "RequestBuilder does nothing until .send() is called"]
pub struct RequestBuilder {
    service: BufferedService,
    max_body_size: usize,
    method: http::Method,
    url: String,
    headers: Vec<(http::header::HeaderName, http::header::HeaderValue)>,
    body: BodyKind,
    /// Error captured during building (deferred to `send()`)
    error: Option<HttpError>,
    /// Transport security mode for URL scheme validation
    transport_security: TransportSecurity,
    /// Optional per-request metrics label (read by the metrics layer)
    request_type: Option<RequestType>,
}

impl RequestBuilder {
    /// Create a new request builder (internal use only)
    pub(crate) fn new(
        service: BufferedService,
        max_body_size: usize,
        method: http::Method,
        url: String,
        transport_security: TransportSecurity,
    ) -> Self {
        Self {
            service,
            max_body_size,
            method,
            url,
            headers: Vec::new(),
            body: BodyKind::Empty,
            error: None,
            transport_security,
            request_type: None,
        }
    }

    /// Add a single header to the request
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .get("https://api.example.com")
    ///     .header("authorization", "Bearer token")
    ///     .header("x-request-id", "abc123")
    ///     .send()
    ///     .await?;
    /// ```
    pub fn header(mut self, name: &str, value: &str) -> Self {
        if self.error.is_some() {
            return self;
        }

        match (
            http::header::HeaderName::try_from(name),
            http::header::HeaderValue::try_from(value),
        ) {
            (Ok(name), Ok(value)) => {
                self.headers.push((name, value));
            }
            (Err(e), _) => {
                self.error = Some(HttpError::InvalidHeaderName(e));
            }
            (_, Err(e)) => {
                self.error = Some(HttpError::InvalidHeaderValue(e));
            }
        }
        self
    }

    /// Attach a tenant-plane bearer token as a **sensitive** `Authorization`
    /// header (`Authorization: Bearer <token>`).
    ///
    /// The header value is marked sensitive (`HeaderValue::set_sensitive(true)`)
    /// so header-logging / redaction layers never emit it — the same guarantee
    /// as [`crate::security::attach_bearer_http`]. Prefer this over
    /// `.header("authorization", ..)` for credentials, which cannot mark the
    /// value sensitive. `token` is the raw credential; the `Bearer ` scheme is
    /// prepended here.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        if self.error.is_some() {
            return self;
        }
        match http::header::HeaderValue::try_from(format!("Bearer {token}")) {
            Ok(mut value) => {
                value.set_sensitive(true);
                self.headers.push((http::header::AUTHORIZATION, value));
            }
            Err(e) => {
                self.error = Some(HttpError::InvalidHeaderValue(e));
            }
        }
        self
    }

    /// Add multiple headers to the request
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .get("https://api.example.com")
    ///     .headers(vec![
    ///         ("authorization".to_owned(), "Bearer token".to_owned()),
    ///         ("x-request-id".to_owned(), "abc123".to_owned()),
    ///     ])
    ///     .send()
    ///     .await?;
    /// ```
    pub fn headers(mut self, headers: Vec<(String, String)>) -> Self {
        if self.error.is_some() {
            return self;
        }

        for (name, value) in headers {
            match (
                http::header::HeaderName::try_from(name),
                http::header::HeaderValue::try_from(value),
            ) {
                (Ok(name), Ok(value)) => {
                    self.headers.push((name, value));
                }
                (Err(e), _) => {
                    self.error = Some(HttpError::InvalidHeaderName(e));
                    return self;
                }
                (_, Err(e)) => {
                    self.error = Some(HttpError::InvalidHeaderValue(e));
                    return self;
                }
            }
        }
        self
    }

    /// Attach a `request_type` label for metrics.
    ///
    /// The label is added as a `request_type` attribute on the
    /// `http.client.request.duration` histogram when the `otel` feature and a
    /// metrics layer are configured. This mirrors go-appkit's
    /// `NewContextWithRequestType` / `GetRequestTypeFromContext` pattern and lets
    /// you break down metrics by logical operation rather than route alone.
    ///
    /// Setting this without a metrics layer is a safe no-op.
    ///
    /// # Example
    ///
    /// ```ignore
    /// client
    ///     .get("https://api.example.com/tenants/123")
    ///     .with_request_type("tenants_resolve")
    ///     .send()
    ///     .await?;
    /// ```
    pub fn with_request_type(mut self, t: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        self.request_type = Some(RequestType::new(t));
        self
    }

    /// Set request body as JSON
    ///
    /// Serializes the value using `serde_json` and sets Content-Type to application/json.
    /// unless a Content-Type header was already provided.
    ///
    /// # Errors
    ///
    /// Returns `Err(HttpError::Json)` if serialization fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// #[derive(Serialize)]
    /// struct CreateUser { name: String }
    ///
    /// let resp = client
    ///     .post("https://api.example.com/users")
    ///     .json(&CreateUser { name: "Alice".into() })?
    ///     .send()
    ///     .await?;
    /// ```
    pub fn json<T: Serialize>(mut self, body: &T) -> Result<Self, HttpError> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }

        let json_bytes = serde_json::to_vec(body)?;
        self.body = BodyKind::Json(Bytes::from(json_bytes));
        Ok(self)
    }

    /// Set request body as form URL-encoded
    ///
    /// Serializes the fields and sets Content-Type to application/x-www-form-urlencoded.
    /// unless a Content-Type header was already provided.
    ///
    /// # Errors
    ///
    /// Returns `Err(HttpError::FormEncode)` if encoding fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .post("https://auth.example.com/token")
    ///     .form(&[
    ///         ("grant_type", "client_credentials"),
    ///         ("client_id", "my-app"),
    ///     ])?
    ///     .send()
    ///     .await?;
    /// ```
    pub fn form(mut self, fields: &[(&str, &str)]) -> Result<Self, HttpError> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }

        let form_string = serde_urlencoded::to_string(fields)?;
        self.body = BodyKind::Form(Bytes::from(form_string));
        Ok(self)
    }

    /// Set request body as raw bytes
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .post("https://api.example.com/upload")
    ///     .header("content-type", "application/octet-stream")
    ///     .body_bytes(Bytes::from(file_contents))
    ///     .send()
    ///     .await?;
    /// ```
    pub fn body_bytes(mut self, body: Bytes) -> Self {
        self.body = BodyKind::Bytes(body);
        self
    }

    /// Set request body as a string
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .post("https://api.example.com/text")
    ///     .header("content-type", "text/plain")
    ///     .body_string("Hello, World!".into())
    ///     .send()
    ///     .await?;
    /// ```
    pub fn body_string(mut self, body: String) -> Self {
        self.body = BodyKind::Bytes(Bytes::from(body));
        self
    }

    /// Validate URL and scheme against transport security configuration.
    ///
    /// Uses proper `http::Uri` parsing instead of string prefix matching.
    /// Returns the parsed URI on success for use in request building.
    fn validate_url(&self) -> Result<http::Uri, HttpError> {
        // Parse URL using http::Uri for proper validation
        let uri: http::Uri =
            self.url
                .parse()
                .map_err(|e: http::uri::InvalidUri| HttpError::InvalidUri {
                    url: self.url.clone(),
                    kind: InvalidUriKind::ParseError,
                    reason: e.to_string(),
                })?;

        // Require authority (host) for absolute URLs
        if uri.authority().is_none() {
            return Err(HttpError::InvalidUri {
                url: self.url.clone(),
                kind: InvalidUriKind::MissingAuthority,
                reason: "missing host/authority".to_owned(),
            });
        }

        // Validate scheme
        match uri.scheme_str() {
            Some("https") => Ok(uri),
            Some("http") => match self.transport_security {
                TransportSecurity::AllowInsecureHttp => Ok(uri),
                TransportSecurity::TlsOnly => Err(HttpError::InvalidScheme {
                    scheme: "http".to_owned(),
                    reason: "HTTPS required (transport security is TlsOnly)".to_owned(),
                }),
            },
            Some(scheme) => Err(HttpError::InvalidScheme {
                scheme: scheme.to_owned(),
                reason: "only http:// and https:// schemes are supported".to_owned(),
            }),
            None => Err(HttpError::InvalidUri {
                url: self.url.clone(),
                kind: InvalidUriKind::MissingScheme,
                reason: "missing scheme".to_owned(),
            }),
        }
    }

    /// Send the request and return the response
    ///
    /// # Errors
    ///
    /// Returns `HttpError` if:
    /// - Request building failed (invalid headers, URL, etc.)
    /// - URL scheme is invalid for the transport security mode
    /// - Network/transport error
    /// - Request timeout
    /// - Concurrency limit reached (`Overloaded`)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resp = client
    ///     .get("https://api.example.com/data")
    ///     .send()
    ///     .await?;
    ///
    /// let data: MyData = resp.json().await?;
    /// ```
    pub async fn send(mut self) -> Result<HttpResponse, HttpError> {
        // Return any deferred error
        if let Some(e) = self.error.take() {
            return Err(e);
        }

        // Validate URL and scheme against transport security
        let uri = self.validate_url()?;

        // Build the request using the validated URI
        let mut builder = Request::builder().method(self.method).uri(uri);

        // Add default Content-Type only if caller didn't supply one
        let has_content_type = self
            .headers
            .iter()
            .any(|(name, _)| name == http::header::CONTENT_TYPE);
        if !has_content_type {
            match &self.body {
                BodyKind::Json(_) => {
                    builder = builder.header("content-type", "application/json");
                }
                BodyKind::Form(_) => {
                    builder = builder.header("content-type", "application/x-www-form-urlencoded");
                }
                BodyKind::Empty | BodyKind::Bytes(_) => {}
            }
        }

        // Add user-provided headers
        // Note: We checked has_content_type above to avoid duplicates. The http builder
        // appends headers rather than replacing, so if user provided Content-Type,
        // we skipped the default above and only their header is added here.
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }

        // Attach request_type extension so the metrics layer can read it without
        // accessing the request body or headers (go-appkit analogue: context value).
        if let Some(rt) = self.request_type {
            builder = builder.extension(rt);
        }

        // Build body
        let body_bytes = match self.body {
            BodyKind::Empty => Bytes::new(),
            BodyKind::Bytes(b) | BodyKind::Json(b) | BodyKind::Form(b) => b,
        };

        let request = builder.body(Full::new(body_bytes))?;

        // Fail-fast if buffer is full
        try_acquire_buffer_slot(&mut self.service).await?;

        let inner: Response<ResponseBody> =
            self.service.call(request).await.map_err(map_buffer_error)?;

        Ok(HttpResponse {
            inner,
            max_body_size: self.max_body_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::HttpClient;

    #[tokio::test]
    async fn bearer_auth_sets_sensitive_authorization_header() {
        let client = HttpClient::new().expect("default toolkit-http build");
        let builder = client
            .get("https://example.invalid/x")
            .bearer_auth("secret-token");

        let value = builder
            .headers
            .iter()
            .find(|(name, _)| *name == http::header::AUTHORIZATION)
            .map(|(_, value)| value)
            .expect("authorization header present");

        assert_eq!(value.to_str().unwrap(), "Bearer secret-token");
        assert!(
            value.is_sensitive(),
            "bearer Authorization header must be marked sensitive"
        );
    }
}

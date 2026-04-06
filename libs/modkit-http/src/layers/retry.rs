use crate::config::{ExponentialBackoff, RetryConfig, RetryTrigger};
use crate::error::HttpError;
use crate::response::{ResponseBody, parse_retry_after};
use bytes::Bytes;
use http::{HeaderValue, Request, Response};
use http_body_util::{BodyExt, Full};
use rand::Rng;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};

/// Header name for retry attempt number (1-indexed).
/// Added to retried requests to indicate which retry attempt this is.
pub const RETRY_ATTEMPT_HEADER: &str = "X-Retry-Attempt";

/// Tower layer that implements retry with exponential backoff and jitter
///
/// This layer operates on services that return `HttpError` and makes retry
/// decisions based on error type and HTTP status codes.
#[derive(Clone)]
pub struct RetryLayer {
    config: RetryConfig,
    total_timeout: Option<Duration>,
}

impl RetryLayer {
    /// Create a new `RetryLayer` with the specified configuration
    #[must_use]
    pub fn new(config: RetryConfig) -> Self {
        Self {
            config,
            total_timeout: None,
        }
    }

    /// Create a new `RetryLayer` with total timeout (deadline across all retries)
    #[must_use]
    pub fn with_total_timeout(config: RetryConfig, total_timeout: Option<Duration>) -> Self {
        Self {
            config,
            total_timeout,
        }
    }
}

impl<S> Layer<S> for RetryLayer {
    type Service = RetryService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RetryService {
            inner,
            config: self.config.clone(),
            total_timeout: self.total_timeout,
        }
    }
}

/// Service that implements retry logic with exponential backoff
///
/// Retries on both `Err(HttpError)` and `Ok(Response)` based on status codes.
/// When retrying on status codes, drains response body up to configured limit
/// to allow connection reuse.
///
/// `send()` returns `Ok(Response)` for ALL HTTP statuses after retries exhaust.
/// `send()` returns `Err` only for transport/timeout errors.
///
/// # Total Timeout (Deadline)
///
/// When `total_timeout` is set, the entire operation (including all retries and
/// backoff delays) must complete within that duration. This provides a hard
/// deadline for the caller, regardless of how many retries are configured.
#[derive(Clone)]
pub struct RetryService<S> {
    inner: S,
    config: RetryConfig,
    total_timeout: Option<Duration>,
}

impl<S> Service<Request<Full<Bytes>>> for RetryService<S>
where
    S: Service<Request<Full<Bytes>>, Response = Response<ResponseBody>, Error = HttpError>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    type Response = S::Response;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
        // Swap so we consume the instance that was poll_ready'd,
        // leaving a fresh clone for the next poll_ready cycle.
        let clone = self.inner.clone();
        let inner = std::mem::replace(&mut self.inner, clone);
        let config = self.config.clone();
        let total_timeout = self.total_timeout;

        let (parts, body_bytes) = req.into_parts();

        // Preserve HTTP version for retry requests (required per HTTP spec)
        let http_version = parts.version;

        // Preserve extensions for retry requests (tracing context, matched routes, etc.)
        // Note: Only extensions implementing Clone + Send + Sync are preserved.
        // Non-cloneable extensions (like some tracing spans) will be lost on retry.
        let extensions = parts.extensions.clone();

        // Check for idempotency key header before wrapping in Arc
        // Header name is pre-parsed at config construction, so just check directly
        let has_idempotency_key = config
            .idempotency_key_header
            .as_ref()
            .is_some_and(|name| parts.headers.contains_key(name));

        let parts = std::sync::Arc::new(parts);

        Box::pin(async move {
            let method = parts.method.clone();

            // Extract request identity for logging (host + optional request-id)
            // Use authority() for full host:port, falling back to host() or "unknown"
            let url_host = parts
                .uri
                .authority()
                .map(ToString::to_string)
                .or_else(|| parts.uri.host().map(ToOwned::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            let request_id = parts
                .headers
                .get("x-request-id")
                .or_else(|| parts.headers.get("x-correlation-id"))
                .and_then(|v| v.to_str().ok())
                .map(String::from);

            // Calculate deadline if total_timeout is set.
            // Store (deadline_instant, timeout_duration) together to avoid unsafe unwrap/expect later.
            let deadline_info = total_timeout.map(|t| (tokio::time::Instant::now() + t, t));

            let mut attempt = 0usize;
            loop {
                // Check deadline before each attempt
                if let Some((deadline, timeout_duration)) = deadline_info
                    && tokio::time::Instant::now() >= deadline
                {
                    return Err(HttpError::DeadlineExceeded(timeout_duration));
                }

                // Reconstruct request from preserved parts
                let mut req = Request::from_parts((*parts).clone(), body_bytes.clone());

                // Restore HTTP version (may have been lost during Parts clone)
                *req.version_mut() = http_version;

                // Restore extensions (tracing context, matched routes, etc.)
                // This ensures retry requests maintain the same context as the original
                *req.extensions_mut() = extensions.clone();

                // Add retry attempt header for retried requests (attempt > 0)
                if attempt > 0 {
                    // Safe: attempt is a small usize, always valid as a header value
                    if let Ok(value) = HeaderValue::try_from(attempt.to_string()) {
                        req.headers_mut().insert(RETRY_ATTEMPT_HEADER, value);
                    }
                }

                let mut svc = inner.clone();
                svc.ready().await?;

                match svc.call(req).await {
                    Ok(resp) => {
                        // Check if we should retry based on HTTP status code
                        let status_code = resp.status().as_u16();
                        let trigger = RetryTrigger::Status(status_code);

                        if config.max_retries > 0
                            && attempt < config.max_retries
                            && config.should_retry(trigger, &method, has_idempotency_key)
                        {
                            // Parse Retry-After from response headers.
                            // Clamp to backoff.max to prevent a malicious/misconfigured
                            // upstream from stalling the client with an absurdly large value.
                            let retry_after = parse_retry_after(resp.headers())
                                .map(|d| d.min(config.backoff.max));
                            let backoff_duration = if config.ignore_retry_after {
                                calculate_backoff(&config.backoff, attempt)
                            } else {
                                retry_after
                                    .unwrap_or_else(|| calculate_backoff(&config.backoff, attempt))
                            };

                            // Drain response body to allow connection reuse
                            let drain_limit = config.retry_response_drain_limit;
                            let should_drain = if config.skip_drain_on_retry {
                                // Configured to skip drain entirely
                                tracing::trace!("Skipping drain: skip_drain_on_retry enabled");
                                false
                            } else if let Some(content_length) = resp
                                .headers()
                                .get(http::header::CONTENT_LENGTH)
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.parse::<u64>().ok())
                            {
                                if content_length > drain_limit as u64 {
                                    // Content-Length exceeds drain limit, skip to avoid
                                    // expensive decompression of large error bodies
                                    tracing::debug!(
                                        content_length,
                                        drain_limit,
                                        "Skipping drain: Content-Length exceeds limit"
                                    );
                                    false
                                } else {
                                    true
                                }
                            } else {
                                // No Content-Length, attempt drain up to limit
                                true
                            };

                            if should_drain
                                && let Err(e) = drain_response_body(resp, drain_limit).await
                            {
                                // If drain fails, log but continue with retry
                                tracing::debug!(
                                    error = %e,
                                    "Failed to drain response body before retry; connection may not be reused"
                                );
                            }

                            // Check if backoff would exceed deadline
                            let effective_backoff =
                                if let Some((deadline, timeout_duration)) = deadline_info {
                                    let remaining = deadline
                                        .saturating_duration_since(tokio::time::Instant::now());
                                    if remaining.is_zero() {
                                        return Err(HttpError::DeadlineExceeded(timeout_duration));
                                    }
                                    backoff_duration.min(remaining)
                                } else {
                                    backoff_duration
                                };

                            tracing::debug!(
                                retry = attempt + 1,
                                max_retries = config.max_retries,
                                status = status_code,
                                trigger = ?trigger,
                                method = %method,
                                host = %url_host,
                                request_id = ?request_id,
                                backoff_ms = effective_backoff.as_millis(),
                                retry_after_used = retry_after.is_some() && !config.ignore_retry_after,
                                "Retrying request after status code"
                            );
                            tokio::time::sleep(effective_backoff).await;
                            attempt += 1;
                            continue;
                        }

                        // No retry needed or retries exhausted - return Ok(Response)
                        return Ok(resp);
                    }
                    Err(err) => {
                        if config.max_retries == 0 || attempt >= config.max_retries {
                            return Err(err);
                        }

                        let trigger = get_retry_trigger(&err);
                        if !config.should_retry(trigger, &method, has_idempotency_key) {
                            return Err(err);
                        }

                        // For errors, there's no response body to drain
                        let backoff_duration = calculate_backoff(&config.backoff, attempt);

                        // Check if backoff would exceed deadline
                        let effective_backoff =
                            if let Some((deadline, timeout_duration)) = deadline_info {
                                let remaining =
                                    deadline.saturating_duration_since(tokio::time::Instant::now());
                                if remaining.is_zero() {
                                    return Err(HttpError::DeadlineExceeded(timeout_duration));
                                }
                                backoff_duration.min(remaining)
                            } else {
                                backoff_duration
                            };

                        tracing::debug!(
                            retry = attempt + 1,
                            max_retries = config.max_retries,
                            error = %err,
                            trigger = ?trigger,
                            method = %method,
                            host = %url_host,
                            request_id = ?request_id,
                            backoff_ms = effective_backoff.as_millis(),
                            "Retrying request after error"
                        );
                        tokio::time::sleep(effective_backoff).await;
                        attempt += 1;
                    }
                }
            }
        })
    }
}

/// Drain response body up to limit bytes to allow connection reuse.
///
/// # Connection Reuse
///
/// For HTTP/1.1, the response body must be fully consumed before the connection
/// can be reused for subsequent requests. This function drains up to `limit`
/// bytes to enable connection pooling.
///
/// # Decompression Note
///
/// This operates on the **decompressed** body (after `DecompressionLayer`).
/// The limit applies to decompressed bytes. For compressed responses, the
/// actual network traffic may be smaller than the configured limit.
///
/// This means draining can cost CPU for highly compressible responses, but
/// provides protection against unexpected memory consumption.
///
/// # Behavior
///
/// - Stops draining once `limit` bytes have been read
/// - If the body exceeds the limit, draining stops early and the connection
///   may not be reused (a new connection will be established for the retry)
/// - Returns `Ok(())` on success, or `HttpError` if body read fails
async fn drain_response_body(
    response: Response<ResponseBody>,
    limit: usize,
) -> Result<(), HttpError> {
    let (_parts, body) = response.into_parts();
    let mut body = std::pin::pin!(body);
    let mut drained = 0usize;

    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(HttpError::Transport)?;
        if let Some(chunk) = frame.data_ref() {
            drained += chunk.len();
            if drained >= limit {
                // Hit limit, stop draining (connection may not be reused)
                break;
            }
        }
    }

    Ok(())
}

/// Extract retry trigger from an error
fn get_retry_trigger(err: &HttpError) -> RetryTrigger {
    match err {
        HttpError::Transport(_) => RetryTrigger::TransportError,
        HttpError::Timeout(_) => RetryTrigger::Timeout,
        // DeadlineExceeded, ServiceClosed, and other errors are not retryable
        _ => RetryTrigger::NonRetryable,
    }
}

/// Calculate backoff duration for a given attempt
///
/// Safely handles edge cases (NaN, infinity, negative values) to avoid panics.
pub fn calculate_backoff(backoff: &ExponentialBackoff, attempt: usize) -> Duration {
    // Maximum safe backoff in seconds (1 day - beyond this is unreasonable for retry logic)
    const MAX_BACKOFF_SECS: f64 = 86400.0;

    // Safely convert attempt to i32, clamping to i32::MAX (which is already way beyond
    // any reasonable retry count - at that point backoff will be at max anyway)
    let attempt_i32 = i32::try_from(attempt).unwrap_or(i32::MAX);

    // Sanitize multiplier: must be finite and >= 0, default to 1.0
    let multiplier = if backoff.multiplier.is_finite() && backoff.multiplier >= 0.0 {
        backoff.multiplier
    } else {
        1.0
    };

    // Sanitize initial backoff
    let initial_secs = backoff.initial.as_secs_f64();
    let initial_secs = if initial_secs.is_finite() && initial_secs >= 0.0 {
        initial_secs
    } else {
        0.0
    };

    // Sanitize max backoff
    let max_secs = backoff.max.as_secs_f64();
    let max_secs = if max_secs.is_finite() && max_secs >= 0.0 {
        max_secs.min(MAX_BACKOFF_SECS)
    } else {
        MAX_BACKOFF_SECS
    };

    // Calculate with sanitized values
    let base_duration = initial_secs * multiplier.powi(attempt_i32);

    // Clamp to valid range for Duration::from_secs_f64 (must be finite, non-negative)
    let clamped = if base_duration.is_finite() {
        base_duration.min(max_secs).max(0.0)
    } else {
        max_secs
    };
    let duration = Duration::from_secs_f64(clamped);

    // Apply jitter
    let duration = if backoff.jitter {
        let mut rng = rand::rng();
        let jitter_factor = rng.random_range(0.0..=0.25);
        let jitter = duration.mul_f64(jitter_factor);
        duration + jitter
    } else {
        duration
    };

    // Keep jittered value within max_backoff
    let max_duration = Duration::from_secs_f64(max_secs);
    duration.min(max_duration)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retry_tests.rs"]
mod tests;

//! `TransportError` — uniform transport-layer error surfaced by generated
//! REST clients.
//!
//! The wire envelope is `toolkit_canonical_errors::Problem` (RFC 9457) when
//! the peer participates in the canonical error system. Older peers may
//! return raw HTTP status codes without a Problem body — those land in
//! [`TransportError::HttpStatus`].

#[cfg(feature = "canonical-errors")]
use toolkit_canonical_errors::Problem;

/// Errors produced by the generated REST client transport layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The server returned a structured RFC 9457 `Problem` payload.
    #[cfg(feature = "canonical-errors")]
    #[error("server returned problem: {} ({})", .problem.title, .problem.status.map_or_else(|| "unknown".to_owned(), |s| s.to_string()))]
    Problem {
        /// The structured RFC 9457 problem payload. Boxed: `Problem` itself is
        /// large enough (several `String`/`Value` fields) that an unboxed copy
        /// here would make `TransportError` — and therefore every
        /// `Result<_, TransportError>` return type across the crate — bloat
        /// past clippy's `result_large_err` threshold.
        problem: Box<Problem>,
        /// Server-advised minimum wait before retry, parsed from the
        /// `Retry-After` response header (delta-seconds), when present. Carried
        /// alongside the `Problem` so in-mesh peers that speak canonical errors
        /// still get their advised backoff honored by the retry loop.
        retry_after: Option<std::time::Duration>,
    },

    /// The server returned a non-success status with a non-Problem body.
    #[error("HTTP {status}: {body}")]
    HttpStatus {
        /// Numeric HTTP status code.
        status: u16,
        /// Body excerpt suitable for diagnostics. Truncated at the call site.
        body: String,
        /// Server-advised minimum wait before retry, parsed from the
        /// `Retry-After` response header (delta-seconds), when present. The
        /// retry loop honors this in preference to computed backoff.
        retry_after: Option<std::time::Duration>,
    },

    /// The gRPC server returned a non-OK status. Preserves the original
    /// `tonic::Code` so callers can map it back to canonical categories
    /// without losing information through an HTTP-status detour.
    #[cfg(feature = "grpc-client")]
    #[error("gRPC {code:?}: {message}")]
    Grpc {
        /// The raw gRPC status code as returned by the server.
        code: tonic::Code,
        /// Human-readable detail copied from `tonic::Status::message`.
        message: String,
    },

    /// Low-level network failure (DNS, connect, TLS, mid-flight reset).
    #[error("network error: {0}")]
    Network(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// The total deadline elapsed before the response was complete.
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    /// Request or response (de)serialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Streaming framing-protocol error: the peer's bytes do not conform to
    /// the wire framing in use — a malformed SSE frame, a bad multipart
    /// delimiter or part header, a part length that overruns its delimiter, an
    /// accumulation guard trip.
    ///
    /// Distinct from [`TransportError::Serialization`], which is a
    /// well-framed frame or part whose *payload* would not decode.
    ///
    /// Replaces an earlier SSE-only variant: naming the framing is what keeps
    /// a fault attributable once more than one framing exists, so there is
    /// deliberately no framing-specific variant to reach for instead.
    #[error("{} framing error: {source}", framing.media_type())]
    Framing {
        /// Which wire framing produced the fault.
        framing: crate::ir::binding::StreamFraming,
        /// Underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// URL construction error (missing path parameter, invalid template).
    #[error("URL build error: {0}")]
    UrlBuild(String),

    /// The providing gear could not be resolved to a live endpoint via the
    /// service directory: it has not registered yet, or every instance was
    /// evicted (e.g., the provider pod went away). Treated as transient — the
    /// directory-resolving client re-resolves on the next call and recovers
    /// once a live instance reappears.
    #[error("provider `{gear}` is not resolvable (not ready or no live instance)")]
    Unresolved {
        /// Logical gear name that could not be resolved to an endpoint.
        gear: String,
    },
}

impl TransportError {
    /// Convenience constructor for [`TransportError::Network`] from any
    /// boxable error. Preserves the source via `Error::source()`.
    pub fn network<E>(err: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Self::Network(err.into())
    }

    /// Convenience constructor for [`TransportError::Serialization`] from any
    /// boxable error. Preserves the source via `Error::source()`.
    pub fn serialization<E>(err: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Self::Serialization(err.into())
    }

    /// Convenience constructor for [`TransportError::Framing`] from any
    /// boxable error. Preserves the source via `Error::source()`.
    pub fn framing<E>(framing: crate::ir::binding::StreamFraming, err: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Self::Framing {
            framing,
            source: err.into(),
        }
    }

    /// Convenience constructor for [`TransportError::Unresolved`].
    pub fn unresolved(gear: impl Into<String>) -> Self {
        Self::Unresolved { gear: gear.into() }
    }

    /// Convenience constructor for [`TransportError::Problem`] with no
    /// server-advised `Retry-After`.
    #[cfg(feature = "canonical-errors")]
    #[must_use]
    pub fn problem(problem: Problem) -> Self {
        Self::Problem {
            problem: Box::new(problem),
            retry_after: None,
        }
    }

    /// Server-advised retry delay (`Retry-After`), when the error carries one.
    ///
    /// Carried by both [`TransportError::HttpStatus`] (non-`Problem` peers) and
    /// [`TransportError::Problem`] (canonical-error peers), parsed from the
    /// response header. Returns `None` for all other error classes.
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            TransportError::HttpStatus { retry_after, .. } => *retry_after,
            #[cfg(feature = "canonical-errors")]
            TransportError::Problem { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether this error class is generally safe to retry without a higher-level
    /// idempotency strategy. Used by [`crate::runtime::retry`] when a method is
    /// declared `#[retryable]`.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            // `Framing` is transient deliberately: that classification is
            // what makes a mid-stream framing fault reconnect-eligible.
            TransportError::Network(_)
            | TransportError::Timeout(_)
            | TransportError::Framing { .. }
            | TransportError::Unresolved { .. } => true,
            TransportError::HttpStatus { status, .. } => is_retryable_status(*status),
            #[cfg(feature = "canonical-errors")]
            TransportError::Problem { problem, .. } => {
                problem.status.is_some_and(is_retryable_status)
            }
            #[cfg(feature = "grpc-client")]
            TransportError::Grpc { code, .. } => matches!(
                code,
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::Cancelled
                    | tonic::Code::Aborted
                    | tonic::Code::ResourceExhausted
            ),
            TransportError::Serialization(_) | TransportError::UrlBuild(_) => false,
        }
    }
}

fn is_retryable_status(status: u16) -> bool {
    // PRD §5.7 retryable set: throttling + gateway/upstream transient failures.
    // Deliberately excludes 408 and 500 — a bare 500 is often a deterministic
    // server-side failure and blindly retrying it (especially a write) risks
    // duplicate side effects.
    matches!(status, 429 | 502 | 503 | 504)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn network_and_timeout_are_transient() {
        assert!(TransportError::network("dns").is_transient());
        assert!(TransportError::Timeout(std::time::Duration::from_secs(1)).is_transient());
    }

    #[test]
    fn serialization_is_not_transient() {
        assert!(!TransportError::serialization("bad json").is_transient());
        assert!(!TransportError::UrlBuild("missing path param".into()).is_transient());
    }

    #[test]
    fn unresolved_is_transient() {
        assert!(TransportError::unresolved("billing").is_transient());
    }

    #[test]
    fn framing_is_transient_for_every_framing() {
        // Q9: a framing fault is transient on purpose — that classification is
        // what makes it reconnect-eligible, and it must not depend on which
        // framing faulted.
        for framing in [
            crate::ir::binding::StreamFraming::ServerSentEvents,
            crate::ir::binding::StreamFraming::MultipartMixed,
        ] {
            assert!(
                TransportError::framing(framing, "bad frame").is_transient(),
                "expected {framing:?} framing errors to be transient"
            );
        }
    }

    #[test]
    fn framing_display_names_the_media_type() {
        assert_eq!(
            TransportError::framing(
                crate::ir::binding::StreamFraming::MultipartMixed,
                "bad delimiter",
            )
            .to_string(),
            "multipart/mixed framing error: bad delimiter"
        );
        assert_eq!(
            TransportError::framing(
                crate::ir::binding::StreamFraming::ServerSentEvents,
                "bad frame",
            )
            .to_string(),
            "text/event-stream framing error: bad frame"
        );
    }

    #[cfg(feature = "grpc-client")]
    #[test]
    fn grpc_transient_codes() {
        for code in [
            tonic::Code::Unavailable,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Cancelled,
            tonic::Code::Aborted,
            tonic::Code::ResourceExhausted,
        ] {
            assert!(
                TransportError::Grpc {
                    code,
                    message: String::new(),
                }
                .is_transient(),
                "expected {code:?} to be transient"
            );
        }
        for code in [
            tonic::Code::NotFound,
            tonic::Code::InvalidArgument,
            tonic::Code::PermissionDenied,
            tonic::Code::Internal,
        ] {
            assert!(
                !TransportError::Grpc {
                    code,
                    message: String::new(),
                }
                .is_transient(),
                "expected {code:?} not to be transient"
            );
        }
    }

    #[test]
    fn five_xx_is_transient_but_4xx_mostly_is_not() {
        assert!(
            TransportError::HttpStatus {
                status: 503,
                body: String::new(),
                retry_after: None,
            }
            .is_transient()
        );
        assert!(
            !TransportError::HttpStatus {
                status: 404,
                body: String::new(),
                retry_after: None,
            }
            .is_transient()
        );
        assert!(
            TransportError::HttpStatus {
                status: 429,
                body: String::new(),
                retry_after: None,
            }
            .is_transient()
        );
    }

    #[test]
    fn bare_500_and_408_are_not_retried() {
        // PRD §5.7: 500 and 408 are deliberately excluded from the retryable set.
        for status in [500u16, 408] {
            assert!(
                !TransportError::HttpStatus {
                    status,
                    body: String::new(),
                    retry_after: None,
                }
                .is_transient(),
                "status {status} must not be retryable"
            );
        }
    }

    #[test]
    fn retry_after_accessor_reads_http_status_field() {
        let err = TransportError::HttpStatus {
            status: 429,
            body: String::new(),
            retry_after: Some(std::time::Duration::from_secs(2)),
        };
        assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(2)));
        assert_eq!(TransportError::network("x").retry_after(), None);
    }
}

//! Wire `reason` discriminator vocabulary for OAGW canonical errors.
//!
//! Consumers dispatch on these constants to handle specific OAGW
//! sub-categories without string-typing literals at the call site. The
//! impl crate references the same constants in its
//! `with_reason(...)` / `From<DomainError> for CanonicalError` boundary
//! so the SDK vocabulary and the wire literals can never drift —
//! the round-trip test in [`crate::tests::reason_constants_round_trip`]
//! pins each value.
//!
//! Gears group constants by the canonical context where the value
//! lands:
//!
//! * [`auth`] — values for `CanonicalError::Unauthenticated.ctx.reason`
//! * [`permission`] — values for `CanonicalError::PermissionDenied.ctx.reason`
//! * [`unavailable`] — values for `CanonicalError::ServiceUnavailable.ctx.reason`
//! * [`timeout`] — values for `CanonicalError::DeadlineExceeded.ctx.reason`
//!
//! For field-violation reasons (which live in
//! `CanonicalError::InvalidArgument.ctx.field_violations[].reason`) see
//! [`crate::field`]. For quota subjects (which live in
//! `CanonicalError::ResourceExhausted.ctx.violations[].subject`) see
//! [`crate::quota`].
//!
//! ## Example
//!
//! ```ignore
//! use oagw_sdk::reason;
//! use toolkit_canonical_errors::CanonicalError;
//!
//! match err {
//!     CanonicalError::Unauthenticated { ctx, .. } => {
//!         match ctx.reason.as_deref() {
//!             Some(reason::auth::PLUGIN_INTERNAL) => /* retry or escalate */,
//!             Some(reason::auth::PLUGIN_FAILED)   => /* creds rejected */,
//!             _                                   => /* generic auth failure */,
//!         }
//!     }
//!     _ => /* other */,
//! }
//! ```

/// Wire `reason` values for [`CanonicalError::Unauthenticated`] emitted
/// by the OAGW data-plane authentication pipeline.
///
/// [`CanonicalError::Unauthenticated`]: toolkit_canonical_errors::CanonicalError::Unauthenticated
pub mod auth {
    /// The configured auth plugin GTS is not registered in `ClientHub`.
    /// Treat as a deployment / configuration failure — retrying the same
    /// request will not help.
    pub const PLUGIN_NOT_FOUND: &str = "AUTH_PLUGIN_NOT_FOUND";

    /// The auth plugin rejected the request (e.g. invalid credentials,
    /// expired token, signature mismatch). Surfacing this distinctly
    /// from [`PLUGIN_INTERNAL`] lets callers retry with refreshed
    /// credentials.
    pub const PLUGIN_FAILED: &str = "AUTH_PLUGIN_FAILED";

    /// The auth plugin itself errored unexpectedly (panic, transport
    /// failure, internal exception). Treat as transient — the request
    /// can usually be retried.
    pub const PLUGIN_INTERNAL: &str = "AUTH_PLUGIN_INTERNAL";

    /// A secret the upstream's auth config references is missing from the
    /// credential store, or not accessible to the calling tenant. The
    /// gateway log names the config key. Retrying does not help until the
    /// secret is provisioned or shared.
    pub const CREDENTIAL_NOT_FOUND: &str = "AUTH_CREDENTIAL_NOT_FOUND";

    use core::fmt;

    /// Typed view of the wire `reason` strings emitted alongside
    /// `CanonicalError::Unauthenticated` by the OAGW data-plane.
    ///
    /// Carried by [`crate::ServiceGatewayError::AuthFailed::reason`].
    /// [`Self::Unknown`] preserves any value the SDK does not specifically
    /// model so the projection stays forward-compatible.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FailureReason {
        /// See [`PLUGIN_NOT_FOUND`]. Operator action needed; retry won't help.
        PluginNotFound,
        /// See [`PLUGIN_FAILED`]. User action needed (refresh credentials).
        PluginFailed,
        /// See [`PLUGIN_INTERNAL`]. Transient — retry usually clears it.
        PluginInternal,
        /// See [`CREDENTIAL_NOT_FOUND`]. Operator action needed (provision
        /// or share the secret); retry won't help.
        CredentialNotFound,
        /// Unknown / unmodeled reason — preserves the raw wire string.
        Unknown(String),
    }

    impl FailureReason {
        /// Project a wire `Unauthenticated.ctx.reason` string into the
        /// typed discriminator. `None` (canonical context has no reason)
        /// and empty strings map to `Unknown("")`.
        #[must_use]
        pub fn from_wire(s: Option<&str>) -> Self {
            match s.unwrap_or("") {
                PLUGIN_NOT_FOUND => Self::PluginNotFound,
                PLUGIN_FAILED => Self::PluginFailed,
                PLUGIN_INTERNAL => Self::PluginInternal,
                CREDENTIAL_NOT_FOUND => Self::CredentialNotFound,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire `reason` string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::PluginNotFound => PLUGIN_NOT_FOUND,
                Self::PluginFailed => PLUGIN_FAILED,
                Self::PluginInternal => PLUGIN_INTERNAL,
                Self::CredentialNotFound => CREDENTIAL_NOT_FOUND,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for FailureReason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

/// Wire `reason` values for [`CanonicalError::PermissionDenied`] emitted
/// by the OAGW data-plane authorization pipeline.
///
/// [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied
pub mod permission {
    /// The authorization policy denied the request without a more
    /// specific reason code. Default for `DomainError::Forbidden`
    /// when no plugin-supplied error code applies.
    pub const AUTHZ_DENIED: &str = "AUTHZ_DENIED";

    /// The tenant resolver rejected the request — the calling principal
    /// is not allowed to act in the tenant scope of the upstream/route.
    pub const TENANT_RESOLVER_UNAUTHORIZED: &str = "TENANT_RESOLVER_UNAUTHORIZED";

    /// The policy enforcement point denied the request because the
    /// subject attempted to act outside its own tenant boundary.
    /// Emitted by PEPs that supply structured deny reasons; surfaces
    /// here through `EnforcerError::AccessDenied(AccessDenial::Pdp { reason })`'s
    /// `reason.error_code`.
    pub const TENANT_BOUNDARY_VIOLATION: &str = "TENANT_BOUNDARY_VIOLATION";

    /// CORS pre-flight or simple request: the request origin is not in
    /// the upstream's CORS `allowed_origins` list.
    pub const CORS_ORIGIN_NOT_ALLOWED: &str = "CORS_ORIGIN_NOT_ALLOWED";

    /// CORS pre-flight or simple request: the request method is not in
    /// the upstream's CORS `allowed_methods` list.
    pub const CORS_METHOD_NOT_ALLOWED: &str = "CORS_METHOD_NOT_ALLOWED";

    /// The resolved upstream serves the alias on endpoints the owner of its
    /// enforced auth did not configure, so the credential cannot be sent.
    /// A tenant above the caller changed its endpoints or switched its auth
    /// to `enforce` after the caller's tenant bound the alias. Projects to
    /// [`DenialReason::Unknown`], which keeps the enum unchanged.
    pub const ENFORCED_AUTH_OTHER_SERVER: &str = "ENFORCED_AUTH_OTHER_SERVER";

    /// The policy granted a management permission only on part of the
    /// caller's tenant (a row-level constraint), which OAGW cannot apply, so
    /// the call is refused. Projects to [`DenialReason::Unknown`].
    pub const AUTHZ_SCOPE_NOT_SUPPORTED: &str = "AUTHZ_SCOPE_NOT_SUPPORTED";

    /// The secret a request names in [`crate::SECRET_REF_HEADER`] (or the
    /// upstream's default in `request_ref` mode) is missing or not visible
    /// to the caller. Missing and invisible secrets answer the same.
    pub const SECRET_REF_NOT_ACCESSIBLE: &str = "SECRET_REF_NOT_ACCESSIBLE";

    /// The requested secret resolved, but OAGW refused to send it: its owner
    /// is above the tenant whose auth config is in effect, or its type is not
    /// in the upstream's `request_secret_types`. (A header rule or transform
    /// that sets the auth header is a gateway misconfiguration and returns
    /// an internal error instead.)
    pub const SECRET_REF_POLICY_DENIED: &str = "SECRET_REF_POLICY_DENIED";

    use core::fmt;

    /// Typed view of the wire `reason` strings emitted alongside
    /// `CanonicalError::PermissionDenied` by the OAGW data-plane.
    ///
    /// Carried by [`crate::ServiceGatewayError::PermissionDenied::reason`].
    /// [`Self::Unknown`] preserves any value the SDK does not specifically
    /// model (e.g. plugin-supplied error codes) so the projection stays
    /// forward-compatible.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DenialReason {
        /// See [`AUTHZ_DENIED`].
        AuthzDenied,
        /// See [`TENANT_RESOLVER_UNAUTHORIZED`].
        TenantResolverUnauthorized,
        /// See [`TENANT_BOUNDARY_VIOLATION`].
        TenantBoundaryViolation,
        /// See [`CORS_ORIGIN_NOT_ALLOWED`].
        CorsOriginNotAllowed,
        /// See [`CORS_METHOD_NOT_ALLOWED`].
        CorsMethodNotAllowed,
        /// See [`SECRET_REF_NOT_ACCESSIBLE`].
        SecretRefNotAccessible,
        /// See [`SECRET_REF_POLICY_DENIED`].
        SecretRefPolicyDenied,
        /// Plugin-supplied or unmodeled reason — preserves the raw wire string.
        Unknown(String),
    }

    impl DenialReason {
        /// Project a wire `PermissionDenied.ctx.reason` string into the
        /// typed discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                AUTHZ_DENIED => Self::AuthzDenied,
                TENANT_RESOLVER_UNAUTHORIZED => Self::TenantResolverUnauthorized,
                TENANT_BOUNDARY_VIOLATION => Self::TenantBoundaryViolation,
                CORS_ORIGIN_NOT_ALLOWED => Self::CorsOriginNotAllowed,
                CORS_METHOD_NOT_ALLOWED => Self::CorsMethodNotAllowed,
                SECRET_REF_NOT_ACCESSIBLE => Self::SecretRefNotAccessible,
                SECRET_REF_POLICY_DENIED => Self::SecretRefPolicyDenied,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire `reason` string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::AuthzDenied => AUTHZ_DENIED,
                Self::TenantResolverUnauthorized => TENANT_RESOLVER_UNAUTHORIZED,
                Self::TenantBoundaryViolation => TENANT_BOUNDARY_VIOLATION,
                Self::CorsOriginNotAllowed => CORS_ORIGIN_NOT_ALLOWED,
                Self::CorsMethodNotAllowed => CORS_METHOD_NOT_ALLOWED,
                Self::SecretRefNotAccessible => SECRET_REF_NOT_ACCESSIBLE,
                Self::SecretRefPolicyDenied => SECRET_REF_POLICY_DENIED,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for DenialReason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

/// Wire `reason` values for [`CanonicalError::ServiceUnavailable`] emitted
/// by the OAGW data plane (HTTP 502 and 503).
///
/// A 502 or 503 from the upstream exchange always carries one of the two
/// dispatch reasons, [`REQUEST_NOT_SENT`] or
/// [`REQUEST_MAY_HAVE_BEEN_SENT`], so a client can tell whether a retry of a
/// non-idempotent request is safe. The other reasons also mean the request
/// was not sent; [`crate::ServiceGatewayError::request_not_sent`] covers all
/// of them.
///
/// [`CanonicalError::ServiceUnavailable`]: toolkit_canonical_errors::CanonicalError::ServiceUnavailable
pub mod unavailable {
    /// No byte of the HTTP request was sent, in any attempt: the
    /// connection was refused, the host did not resolve or was refused by
    /// the SSRF policy, the TLS handshake or certificate check failed, the
    /// circuit breaker is open, or the gateway failed before sending. TCP
    /// or TLS setup may have reached the upstream; the request did not.
    /// Safe to retry for any method.
    pub const REQUEST_NOT_SENT: &str = "REQUEST_NOT_SENT";

    /// The request, or part of it, may have reached the upstream before the
    /// exchange failed (connection closed, read or write error, HTTP/2 or
    /// protocol error), or in an earlier attempt the gateway resent, even
    /// when the resend failed to connect. Retry only an idempotent request.
    pub const REQUEST_MAY_HAVE_BEEN_SENT: &str = "REQUEST_MAY_HAVE_BEEN_SENT";

    /// The upstream is disabled by its configuration. Only an operator can
    /// enable it again; `retry_after_seconds` is a long hint.
    pub const UPSTREAM_DISABLED: &str = "UPSTREAM_DISABLED";

    /// The credential store an auth plugin, or a `request_ref` secret
    /// reference, reads secrets from is unavailable. `retry_after_seconds`
    /// carries the store's own hint when it gave one. The upstream was not
    /// contacted.
    pub const CREDENTIAL_STORE_UNAVAILABLE: &str = "CREDENTIAL_STORE_UNAVAILABLE";

    /// The OAuth2 identity provider's token endpoint an auth plugin calls
    /// failed: transport error, timeout, 408, 429 or 5xx. Retry later. The
    /// upstream was not contacted.
    pub const IDENTITY_PROVIDER_UNAVAILABLE: &str = "IDENTITY_PROVIDER_UNAVAILABLE";

    use core::fmt;

    /// Typed view of the wire `reason` strings emitted alongside
    /// `CanonicalError::ServiceUnavailable` by the OAGW data plane.
    ///
    /// Carried by [`crate::ServiceGatewayError::Unavailable::reason`].
    /// [`Self::Unknown`] preserves any value the SDK does not specifically
    /// model so the projection stays forward-compatible.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum UnavailableReason {
        /// See [`REQUEST_NOT_SENT`].
        RequestNotSent,
        /// See [`REQUEST_MAY_HAVE_BEEN_SENT`].
        RequestMayHaveBeenSent,
        /// See [`UPSTREAM_DISABLED`].
        UpstreamDisabled,
        /// See [`CREDENTIAL_STORE_UNAVAILABLE`].
        CredentialStoreUnavailable,
        /// See [`IDENTITY_PROVIDER_UNAVAILABLE`].
        IdentityProviderUnavailable,
        /// Unmodeled reason — preserves the raw wire string.
        Unknown(String),
    }

    impl UnavailableReason {
        /// Project a wire `ServiceUnavailable.ctx.reason` string into the
        /// typed discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                REQUEST_NOT_SENT => Self::RequestNotSent,
                REQUEST_MAY_HAVE_BEEN_SENT => Self::RequestMayHaveBeenSent,
                UPSTREAM_DISABLED => Self::UpstreamDisabled,
                CREDENTIAL_STORE_UNAVAILABLE => Self::CredentialStoreUnavailable,
                IDENTITY_PROVIDER_UNAVAILABLE => Self::IdentityProviderUnavailable,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire `reason` string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::RequestNotSent => REQUEST_NOT_SENT,
                Self::RequestMayHaveBeenSent => REQUEST_MAY_HAVE_BEEN_SENT,
                Self::UpstreamDisabled => UPSTREAM_DISABLED,
                Self::CredentialStoreUnavailable => CREDENTIAL_STORE_UNAVAILABLE,
                Self::IdentityProviderUnavailable => IDENTITY_PROVIDER_UNAVAILABLE,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for UnavailableReason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

/// Wire `reason` values for [`CanonicalError::DeadlineExceeded`] emitted
/// by the OAGW data plane (HTTP 504).
///
/// The same two dispatch reasons as [`unavailable`](super::unavailable): whether
/// the request may have reached the upstream before the timeout.
///
/// [`CanonicalError::DeadlineExceeded`]: toolkit_canonical_errors::CanonicalError::DeadlineExceeded
pub mod timeout {
    /// The timeout hit before any byte of the request was sent (connect
    /// timeout, endpoint selection, DNS, signing). Safe to retry for any
    /// method.
    pub const REQUEST_NOT_SENT: &str = super::unavailable::REQUEST_NOT_SENT;

    /// The timeout hit after the request, or part of it, may have been
    /// sent (read or write timeout, request timeout). Retry only an
    /// idempotent request.
    pub const REQUEST_MAY_HAVE_BEEN_SENT: &str = super::unavailable::REQUEST_MAY_HAVE_BEEN_SENT;

    use core::fmt;

    /// Typed view of the wire `reason` strings emitted alongside
    /// `CanonicalError::DeadlineExceeded` by the OAGW data plane.
    ///
    /// Carried by [`crate::ServiceGatewayError::Timeout::reason`].
    /// [`Self::Unknown`] preserves any value the SDK does not specifically
    /// model so the projection stays forward-compatible.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum TimeoutReason {
        /// See [`REQUEST_NOT_SENT`].
        RequestNotSent,
        /// See [`REQUEST_MAY_HAVE_BEEN_SENT`].
        RequestMayHaveBeenSent,
        /// Unmodeled reason — preserves the raw wire string.
        Unknown(String),
    }

    impl TimeoutReason {
        /// Project a wire `DeadlineExceeded.ctx.reason` string into the
        /// typed discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                REQUEST_NOT_SENT => Self::RequestNotSent,
                REQUEST_MAY_HAVE_BEEN_SENT => Self::RequestMayHaveBeenSent,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire `reason` string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::RequestNotSent => REQUEST_NOT_SENT,
                Self::RequestMayHaveBeenSent => REQUEST_MAY_HAVE_BEEN_SENT,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for TimeoutReason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

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

    /// The caller has no usable per-user authorization for the upstream and
    /// must complete an interactive OAuth authorization-code flow (e.g. via
    /// mini-chat's connection `:authorize` endpoint). Distinct from
    /// [`PLUGIN_FAILED`]: credentials are not merely rejected — none exist
    /// yet — so the remedy is (re-)authorization, not a credential refresh.
    pub const AUTHORIZATION_REQUIRED: &str = "AUTHORIZATION_REQUIRED";

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
        /// See [`AUTHORIZATION_REQUIRED`]. User must (re-)authorize — no
        /// usable per-user authorization exists yet (not a credential refresh).
        AuthorizationRequired,
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
                AUTHORIZATION_REQUIRED => Self::AuthorizationRequired,
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
                Self::AuthorizationRequired => AUTHORIZATION_REQUIRED,
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
    /// here through `EnforcerError::Denied.deny_reason.error_code`.
    pub const TENANT_BOUNDARY_VIOLATION: &str = "TENANT_BOUNDARY_VIOLATION";

    /// CORS pre-flight or simple request: the request origin is not in
    /// the upstream's CORS `allowed_origins` list.
    pub const CORS_ORIGIN_NOT_ALLOWED: &str = "CORS_ORIGIN_NOT_ALLOWED";

    /// CORS pre-flight or simple request: the request method is not in
    /// the upstream's CORS `allowed_methods` list.
    pub const CORS_METHOD_NOT_ALLOWED: &str = "CORS_METHOD_NOT_ALLOWED";

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

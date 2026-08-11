//! Error types for the license resolver.
//!
//! A not-granted decision is **not** an error — it is
//! [`LicenseDecision { granted: false, .. }`](crate::LicenseDecision). This
//! enum covers only "cannot-determine" conditions; on any of them the caller
//! **fails closed** (treats the outcome as not-granted).
//!
//! ## Canonical (RFC-9457) mapping
//!
//! The SDK provides a **default mapping** of each variant to a canonical
//! [`CanonicalError`] via `From<LicenseResolverError>`, so consumers do not
//! re-implement it (nor re-carry the [`FieldViolation`]s). Render it as an
//! RFC-9457 `Problem` with `Problem::from_error(&err.into())`, or ignore the
//! mapping and match the typed variant directly — the choice is the caller's.
//! The category → GTS error type id per variant:
//!
//! | Variant | Canonical category — GTS error type id |
//! |---|---|
//! | [`Unauthorized`](LicenseResolverError::Unauthorized) | `PermissionDenied` — `gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~` |
//! | [`InvalidRequest`](LicenseResolverError::InvalidRequest) | `InvalidArgument` — `…cf.core.err.invalid_argument.v1~` |
//! | [`NoPluginAvailable`](LicenseResolverError::NoPluginAvailable) | `Internal` — `…cf.core.err.internal.v1~` |
//! | [`ServiceUnavailable`](LicenseResolverError::ServiceUnavailable) | `ServiceUnavailable` — `…cf.core.err.service_unavailable.v1~` |
//! | [`Internal`](LicenseResolverError::Internal) | `Internal` — `…cf.core.err.internal.v1~` |

use thiserror::Error;
pub use toolkit_canonical_errors::FieldViolation;
use toolkit_canonical_errors::{CanonicalError, resource_error};

/// Errors that can occur when performing a license check.
///
/// Infrastructure / cannot-determine conditions only — see the module docs for
/// the fail-closed contract and the canonical error mapping.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum LicenseResolverError {
    /// The backend refused to answer the check for this caller/subject.
    ///
    /// Originates in the plugin — typically one fronting a remote licensing
    /// service that rejects the query itself; the gateway has no caller
    /// identity of its own and propagates this unchanged. **Not** a way to say
    /// "not licensed": that is `LicenseDecision { granted: false }`, which is
    /// not an error.
    #[error("unauthorized")]
    Unauthorized,

    /// The request does not conform to its registered licensing contracts
    /// (schema mismatch, missing contract type, subject type not admitted, or an
    /// unregistered contract type). Fail-closed and **distinct** from a
    /// not-granted decision.
    ///
    /// Carries canonical [`FieldViolation`]s: `field` locates the offending
    /// element (e.g. the contract type plus a JSON-pointer such as
    /// `gts.cf.core.lic.res.v1~…/metadata/model_name`), `reason` is a
    /// machine-readable code, `description` is human-readable. Maps directly to
    /// `InvalidArgument::fields(..)`.
    #[error("invalid request: {} violation(s)", violations.len())]
    InvalidRequest {
        /// The contract-validation violations that made the request
        /// non-conforming (mirrors the canonical `field_violations` carrier).
        violations: Vec<FieldViolation>,
    },

    /// No backend plugin was discovered — selection is by the plugin spec's
    /// vendor + priority, so this means none is registered in this environment,
    /// not that none handles a particular resource. Fail-closed — never a
    /// granted decision.
    #[error("no plugin available")]
    NoPluginAvailable,

    /// The selected backend is unreachable or erroring, or a required schema
    /// could not be resolved from the registry. Fail-closed.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// An unexpected internal failure.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Resource marker for the resolver's own canonical envelopes. The resolver is
/// generic over the resource being checked, so its permission / argument errors
/// are labelled with the license-check resource type rather than a caller's
/// resource type. (`resource_type` on the canonical error is a label; it need
/// not be a registered type.)
#[resource_error(gts_id!("cf.core.lic.check.v1~"))]
struct LicenseCheckResource;

/// Default mapping to the platform canonical error (AIP-193 / RFC-9457).
///
/// Consumers get a `Problem` via `Problem::from_error(&err.into())`. The
/// per-variant category is documented on [`LicenseResolverError`]; the GTS
/// error type id is set automatically by each canonical category, and
/// [`InvalidRequest`](LicenseResolverError::InvalidRequest) forwards its
/// [`FieldViolation`]s onto `InvalidArgument` losslessly.
impl From<LicenseResolverError> for CanonicalError {
    fn from(err: LicenseResolverError) -> Self {
        match err {
            LicenseResolverError::Unauthorized => LicenseCheckResource::permission_denied()
                .with_reason("LICENSE_CHECK_UNAUTHORIZED")
                .create(),
            LicenseResolverError::InvalidRequest { violations } => {
                let mut iter = violations.into_iter();
                match iter.next() {
                    // At least one violation: fold them onto the field-violation
                    // list (the typestate requires the first before `create`).
                    Some(first) => {
                        let mut builder = LicenseCheckResource::invalid_argument()
                            .with_field_violation(first.field, first.description, first.reason);
                        for v in iter {
                            builder =
                                builder.with_field_violation(v.field, v.description, v.reason);
                        }
                        builder.create()
                    }
                    // Defensive: an InvalidRequest with no violations still maps
                    // to a well-formed InvalidArgument rather than panicking.
                    None => LicenseCheckResource::invalid_argument()
                        .with_format(
                            "request does not conform to its registered licensing contracts",
                        )
                        .create(),
                }
            }
            LicenseResolverError::NoPluginAvailable => {
                CanonicalError::internal("no license backend plugin available").create()
            }
            LicenseResolverError::ServiceUnavailable(_) => {
                // The diagnostic may contain backend URLs, hostnames, driver text, or configuration
                // fragments. Keep the public RFC-9457 detail stable and non-sensitive.
                CanonicalError::service_unavailable()
                    .with_detail("License service temporarily unavailable")
                    .create()
            }
            LicenseResolverError::Internal(reason) => CanonicalError::internal(reason).create(),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;

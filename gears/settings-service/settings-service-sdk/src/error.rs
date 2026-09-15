// Created: 2026-08-12 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-error-taxonomy:p1
//! The reader degradation contract, as a typed projection over `CanonicalError`.
//!
//! # This is an opt-in convenience, not the contract
//!
//! Every trait method in [`crate::api`] returns `Result<_, CanonicalError>`. The
//! enum below is a *view* over that canonical error, offered because settings
//! consumers must distinguish three outcomes that demand different responses,
//! and two of them have no canonical category of their own.
//!
//! Adding a variant here is not a breaking change for anyone using the traits.
//!
//! # Why a projection at all
//!
//! Settings are a boot-time dependency: every gear reads them at startup, and
//! this service does not mask its own unavailability. The consumer owns its
//! degradation posture, so the contract must be hard to misuse:
//!
//! | Outcome | What the consumer should do |
//! |---|---|
//! | [`SettingsError::Unavailable`] | retry, hold a last-known value, or degrade |
//! | [`SettingsError::Retired`] | stop reading — a retry will never help |
//! | [`SettingsError::NotFound`] | decide wait-vs-give-up from its own boot ordering |
//! | [`SettingsError::SecretNotConfigured`] | treat the placeholder as absent, never as a credential |
//! | [`SettingsError::Unauthorized`] | not entitled to this specific setting |
//!
//! `Retired` is not a canonical category, and the credential-absent case is a
//! `NotFound` that collides with the resolver's own. Without this projection a
//! consumer would have to compare context strings to tell them apart.
//!
//! # Using it
//!
//! Three integration patterns, in ascending order of coupling. All three are
//! valid; pick by how much the consumer actually dispatches.
//!
//! **1 — pure propagation.** Ignore this type entirely and let `?` carry the
//! `CanonicalError` up, projecting nowhere:
//!
//! ```ignore
//! let value = settings.get_effective(&ctx, req).await?;
//! ```
//!
//! Right when the consumer only propagates, or already dispatches at the
//! granularity of canonical categories. Shipping a projection does not oblige
//! anyone to use it.
//!
//! **2 — explicit projection at the call site.** Project where the distinction
//! is needed and nowhere else:
//!
//! ```ignore
//! let value = settings.get_effective(&ctx, req).await
//!     .map_err(SettingsError::from)?;
//! ```
//!
//! **3 — transparent chaining.** Define `From<CanonicalError>` for the
//! consumer's own error type and route through this projection inside it, so
//! every call site stays a plain `?`:
//!
//! ```ignore
//! impl From<CanonicalError> for MyGearError {
//!     fn from(err: CanonicalError) -> Self {
//!         match SettingsError::from(err) {
//!             SettingsError::SecretNotConfigured { .. } => Self::MisconfiguredBackend,
//!             other => Self::Settings(other),
//!         }
//!     }
//! }
//! ```
//!
//! # Contract
//!
//! See `docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md` for
//! the platform rules this projection follows: infallible construction, the
//! mandatory catch-all, co-located wire constants, and no transport fields.

use toolkit_canonical_errors::CanonicalError;

use crate::gts::Resource;
use crate::precondition::PreconditionKind;

/// Typed view over the canonical errors this SDK emits.
///
/// Built infallibly from [`CanonicalError`]; anything not modelled here arrives
/// as [`SettingsError::Other`] with its canonical value intact.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    /// The value could not be resolved. A retry may succeed.
    #[error("settings unavailable: {detail}")]
    Unavailable {
        /// How long the caller is asked to wait, when the service said.
        retry_after_secs: Option<u64>,
        /// Human-readable detail from the canonical error.
        detail: String,
    },

    /// The declaration was withdrawn. A retry will never succeed.
    ///
    /// The consumer should drop the dependency rather than wait.
    #[error("setting has been retired: {detail}")]
    Retired {
        /// Human-readable detail from the canonical error.
        detail: String,
    },

    /// No declaration exists for the key.
    ///
    /// Deliberately conflates two cases the service cannot tell apart: the
    /// owning gear has not registered yet, and the key never existed. The
    /// consumer resolves that from its own boot ordering.
    #[error("setting not declared: {detail}")]
    NotFound {
        /// Human-readable detail from the canonical error.
        detail: String,
    },

    /// A secret-backed setting has no credential configured at any scope.
    ///
    /// Distinct from [`SettingsError::NotFound`]: the declaration exists and
    /// resolves, but its value is the non-secret placeholder. Handing that
    /// placeholder to a backend as if it were a credential is the failure this
    /// variant exists to prevent.
    #[error("no credential configured for this setting: {detail}")]
    SecretNotConfigured {
        /// Human-readable detail from the canonical error.
        detail: String,
    },

    /// The caller is not entitled to this specific setting.
    #[error("not authorized for this setting: {detail}")]
    Unauthorized {
        /// Human-readable detail from the canonical error.
        detail: String,
    },

    /// A canonical category this SDK does not model.
    ///
    /// Mandatory catch-all: it keeps the conversion infallible and lets a new
    /// canonical category reach the consumer with full fidelity rather than
    /// being collapsed into a neighbour.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other {
        /// The canonical error, preserved verbatim.
        canonical: CanonicalError,
    },
}

impl From<CanonicalError> for SettingsError {
    fn from(err: CanonicalError) -> Self {
        match &err {
            CanonicalError::ServiceUnavailable { ctx, detail, .. } => Self::Unavailable {
                retry_after_secs: ctx.retry_after_seconds,
                detail: detail.clone(),
            },

            // One canonical category, two consumer outcomes. The resource the
            // error was attributed to is the only thing separating "this setting
            // was never declared" from "this secret has no credential anywhere".
            CanonicalError::NotFound { detail, .. } => match not_found_resource(&err) {
                Some(Resource::Value) => Self::SecretNotConfigured {
                    detail: detail.clone(),
                },
                _ => Self::NotFound {
                    detail: detail.clone(),
                },
            },

            // `Retired` has no canonical category, so it rides on a precondition
            // violation. Guarded rather than unconditional: a failed precondition
            // that is not a retirement belongs in the catch-all, not here.
            CanonicalError::FailedPrecondition { detail, .. }
                if precondition_kind(&err) == Some(PreconditionKind::SettingRetired) =>
            {
                Self::Retired {
                    detail: detail.clone(),
                }
            }

            CanonicalError::PermissionDenied { detail, .. } => Self::Unauthorized {
                detail: detail.clone(),
            },

            // Mandatory catch-all. `CanonicalError` is `#[non_exhaustive]`, so
            // this arm is also what keeps the projection compiling when the
            // platform adds a category.
            _ => Self::Other {
                canonical: err.clone(),
            },
        }
    }
}

/// Which resource a not-found error was attributed to, if any.
///
/// Exposed so a caller can reproduce the projection's own discrimination
/// without re-deriving it from wire strings.
///
/// # When the attribution is missing
///
/// In-process construction always supplies it, but this can arrive over the
/// wire: `Problem` carries `resource_type` as ordinary context, so a producer or
/// proxy that omits or strips it yields `None` here.
///
/// The projection then falls to [`SettingsError::NotFound`], **never** to
/// [`SettingsError::SecretNotConfigured`]. That direction is deliberate and is
/// the safe one: `SecretNotConfigured` is the stronger claim — it asserts the
/// declaration resolved and only the credential is absent — and fabricating it
/// from an unattributed error would tell a consumer something the error does not
/// support. Losing the distinction degrades the typed surface; inventing it
/// would mislead.
#[must_use]
pub fn not_found_resource(err: &CanonicalError) -> Option<Resource> {
    match err {
        CanonicalError::NotFound { resource_type, .. } => {
            resource_type.as_deref().map(Resource::from_wire)
        }
        _ => None,
    }
}

/// Which precondition a failed-precondition error reported, if any.
///
/// Reports the first violation: the canonical envelope allows several, but this
/// service raises one per failure.
#[must_use]
pub fn precondition_kind(err: &CanonicalError) -> Option<PreconditionKind> {
    match err {
        CanonicalError::FailedPrecondition { ctx, .. } => ctx
            .violations
            .first()
            .map(|violation| PreconditionKind::from_wire(&violation.type_)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;

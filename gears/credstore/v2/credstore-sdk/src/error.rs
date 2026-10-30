// Updated: 2026-10-06 by Constructor Tech
//! Credstore SDK error surface: the opt-in typed projection of
//! [`CanonicalError`].
//!
//! [`CredStoreClientV2`](crate::CredStoreClientV2) returns
//! [`CanonicalError`] for every fallible method. [`CredStoreError`] is a
//! convenience for consumers that want flat typed dispatch over what the gear
//! emits. It is not part of the trait contract: adding or refining a variant
//! never breaks the trait, and a consumer that only propagates failures never
//! needs it. The conversion is infallible through
//! `From<CanonicalError>`; a canonical category or reason the projection does
//! not model falls through to [`CredStoreError::Other`], which keeps the full
//! [`CanonicalError`].
//!
//! See `docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md` for
//! the workspace pattern.
//!
//! ## Consumer integration
//!
//! Three patterns are valid.
//!
//! Propagate [`CanonicalError`] with `?` when the caller's own error type
//! implements `From<CanonicalError>` and no dispatch is needed:
//!
//! ```ignore
//! let secret = credstore.get_secret(&ctx, &key).await?;
//! ```
//!
//! Project at the call site when one site wants typed dispatch:
//!
//! ```ignore
//! use credstore_v2_sdk::CredStoreError;
//!
//! match credstore.get_secret(&ctx, &key).await.map_err(CredStoreError::from) {
//!     Ok(secret) => /* use it */,
//!     Err(CredStoreError::SecretExpired) => /* renew the credential */,
//!     Err(CredStoreError::Unavailable { retry_after, .. }) => /* back off */,
//!     Err(other) => return Err(other.into()),
//! }
//! ```
//!
//! Chain through the projection once, in the consumer crate, so every call
//! site stays a plain `?`:
//!
//! ```ignore
//! impl From<CanonicalError> for OwnError {
//!     fn from(err: CanonicalError) -> Self {
//!         CredStoreError::from(err).into()
//!     }
//! }
//! ```
//!
//! ## Typed reason enums
//!
//! Per the ADR's rule 4, each canonical category submodule of [`crate::reason`] keeps
//! its wire constants next to one typed enum (`reason::aborted::Reason`,
//! `reason::failed_precondition::Reason`, `reason::permission_denied::Reason`),
//! each with an `Unknown(String)` catch-all, `from_wire` and `as_wire`. They
//! serve consumers that match [`CanonicalError`] directly and want a typed
//! view of `ctx.reason` without going through [`CredStoreError`]. There is
//! exactly one typed enum per category: [`CredStoreError::TypeConflict`]
//! carries the `aborted` one (restricted to its three type-conflict variants)
//! instead of a second, narrower enum.
//!
//! ## Consumer dispatch reference
//!
//! Each variant, with the canonical category and reason it projects from:
//!
//! * [`CredStoreError::NotFound`] comes from `NotFound`
//! * [`CredStoreError::AlreadyExists`] comes from `AlreadyExists`
//! * [`CredStoreError::PreconditionFailed`] comes from `Aborted` with
//!   [`reason::aborted::OPTIMISTIC_LOCK_FAILURE`]
//! * [`CredStoreError::TypeConflict`] comes from `Aborted` with
//!   [`reason::aborted::TYPE_IMMUTABLE`],
//!   [`reason::aborted::TYPE_MISMATCH_WITH_INHERITED`] or
//!   [`reason::aborted::TYPE_MISMATCH_WITH_DESCENDANT`]
//! * [`CredStoreError::SecretExpired`] comes from `FailedPrecondition` with
//!   [`reason::failed_precondition::SECRET_EXPIRED`]
//! * [`CredStoreError::UnsupportedTransition`] comes from `FailedPrecondition` with
//!   [`reason::failed_precondition::UNSUPPORTED_TRANSITION`]
//! * [`CredStoreError::Invalid`] comes from `InvalidArgument`; the first field violation
//!   gives `field` and the [`field::Reason`]
//! * [`CredStoreError::PermissionDenied`] comes from `PermissionDenied`
//! * [`CredStoreError::Unavailable`] comes from `ServiceUnavailable`, with the retry
//!   hint when there is one; also "no storage plugin registered" and "the
//!   store refused the plugin's credentials", which carry no hint
//! * [`CredStoreError::Internal`] comes from `Internal`
//! * [`CredStoreError::Other`] comes from any other category, and an `Aborted` or
//!   `FailedPrecondition` with a reason this SDK does not know

use std::time::Duration;

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::field;
use crate::reason::aborted::Reason as AbortedReason;
use crate::reason::failed_precondition;

/// Typed projection of [`CanonicalError`] for credstore consumers.
///
/// The gear's `From<DomainError> for CanonicalError` is the single
/// authoritative AIP-193 mapping; this projection translates canonical
/// categories into the dispositions consumers dispatch on. The conversion is
/// infallible; unmodeled canonical variants fall through to [`Self::Other`].
/// The enum is `#[non_exhaustive]`: match with a wildcard arm.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum CredStoreError {
    /// No record under the reference is visible to the caller (a record the
    /// PDP's constraints exclude is the same miss; a denial of the decision
    /// itself is [`Self::PermissionDenied`]).
    #[error("secret not found")]
    NotFound,

    /// A create-only `put` over an existing own record, an expired own record
    /// included (it is renewed by a replace or `patch`, or deleted).
    #[error("secret already exists")]
    AlreadyExists,

    /// A replace (`Exists` / `Matches`) whose target is absent, or a
    /// `Matches` validator that no longer holds (lost compare-and-set).
    /// Re-read the record and retry.
    #[error("write precondition failed")]
    PreconditionFailed,

    /// A write named a secret type that conflicts with the one already in
    /// play; `reason` says which.
    #[error("secret type conflict ({reason}): {detail}")]
    TypeConflict {
        /// One of [`AbortedReason::TypeImmutable`],
        /// [`AbortedReason::TypeMismatchWithInherited`] or
        /// [`AbortedReason::TypeMismatchWithDescendant`].
        reason: AbortedReason,
        detail: String,
    },

    /// The decisive `active` record's `expires_at` has passed: the secret is
    /// never served and resolution does not continue to an ancestor's value.
    /// Renewing the record (`patch` of `expires_at` or a new `secret`)
    /// restores it.
    #[error("secret expired")]
    SecretExpired,

    /// A `sharing` change would move the record across the private /
    /// tenant-shared boundary.
    #[error("unsupported sharing transition: {detail}")]
    UnsupportedTransition { detail: String },

    /// The request is invalid. `field` is the offending field (empty when the
    /// canonical error names none) and `reason` is its typed [`field::Reason`].
    #[error("invalid request [{field}/{reason}]: {detail}")]
    Invalid {
        field: String,
        reason: field::Reason,
        detail: String,
    },

    /// The caller lacks a required action.
    #[error("access denied")]
    PermissionDenied,

    /// A dependency is unreachable: the PDP, the registry, the database or
    /// the storage backend; also no storage plugin registered, or the store
    /// refused the plugin's credentials (both without a retry hint).
    #[error("service unavailable: {detail}")]
    Unavailable {
        detail: String,
        retry_after: Option<Duration>,
    },

    /// An unexpected internal failure; not retryable.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Catch-all for canonical categories the projection does not model, and
    /// for an `Aborted` / `FailedPrecondition` with an unknown reason.
    /// Preserves the full [`CanonicalError`] for inspection.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

impl From<CanonicalError> for CredStoreError {
    fn from(err: CanonicalError) -> Self {
        match project(&err) {
            Some(projected) => projected,
            None => Self::Other { canonical: err },
        }
    }
}

/// The modeled projection of `err`, or `None` for the `Other` fall-through.
fn project(err: &CanonicalError) -> Option<CredStoreError> {
    let detail = err.detail().to_owned();
    Some(match err {
        CanonicalError::NotFound { .. } => CredStoreError::NotFound,
        CanonicalError::AlreadyExists { .. } => CredStoreError::AlreadyExists,

        CanonicalError::Aborted { ctx, .. } => match AbortedReason::from_wire(&ctx.reason) {
            AbortedReason::OptimisticLockFailure => CredStoreError::PreconditionFailed,
            reason @ (AbortedReason::TypeImmutable
            | AbortedReason::TypeMismatchWithInherited
            | AbortedReason::TypeMismatchWithDescendant) => {
                CredStoreError::TypeConflict { reason, detail }
            }
            _ => return None,
        },

        CanonicalError::FailedPrecondition { ctx, .. } => {
            let violation = ctx.violations.first()?;
            match violation.type_.as_str() {
                failed_precondition::SECRET_EXPIRED => CredStoreError::SecretExpired,
                failed_precondition::UNSUPPORTED_TRANSITION => {
                    CredStoreError::UnsupportedTransition {
                        detail: violation.description.clone(),
                    }
                }
                _ => return None,
            }
        }

        CanonicalError::InvalidArgument { ctx, .. } => project_invalid_argument(ctx, detail),
        CanonicalError::PermissionDenied { .. } => CredStoreError::PermissionDenied,

        CanonicalError::ServiceUnavailable { ctx, .. } => CredStoreError::Unavailable {
            detail,
            retry_after: ctx.retry_after_seconds.map(Duration::from_secs),
        },

        CanonicalError::Internal { .. } => CredStoreError::Internal { detail },

        _ => return None,
    })
}

/// Project the first field violation; a `Format` / `Constraint` context or an
/// empty violation list yields an empty field and `Unknown("")`.
fn project_invalid_argument(ctx: &InvalidArgument, detail: String) -> CredStoreError {
    let first_violation = match ctx {
        InvalidArgument::FieldViolations { field_violations } => field_violations.first(),
        _ => None,
    };
    match first_violation {
        Some(v) => CredStoreError::Invalid {
            field: v.field.clone(),
            reason: field::Reason::from_wire(&v.reason),
            detail: v.description.clone(),
        },
        None => CredStoreError::Invalid {
            field: String::new(),
            reason: field::Reason::Unknown(String::new()),
            detail,
        },
    }
}

impl CredStoreError {
    /// `true` for [`Self::NotFound`].
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }

    /// `true` for [`Self::AlreadyExists`] (a create-only `put` over an
    /// existing own record).
    #[must_use]
    pub fn is_already_exists(&self) -> bool {
        matches!(self, Self::AlreadyExists)
    }

    /// `true` only for [`Self::PreconditionFailed`] (absent replace target or
    /// lost compare-and-set). A create-only duplicate is [`Self::AlreadyExists`].
    #[must_use]
    pub fn is_precondition_failed(&self) -> bool {
        matches!(self, Self::PreconditionFailed)
    }

    /// `true` when the decisive record's secret has expired.
    #[must_use]
    pub fn is_secret_expired(&self) -> bool {
        matches!(self, Self::SecretExpired)
    }

    /// `true` for an authorization denial.
    #[must_use]
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied)
    }

    /// `true` for [`Self::Unavailable`].
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// The retry hint of [`Self::Unavailable`]; `None` for every other
    /// variant and for an outage without a hint.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Unavailable { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Round-trip tests pinning the SDK vocabulary to the canonical wire.
    //!
    //! Every constant of [`crate::reason`] and [`crate::field`] is built into
    //! a `CanonicalError` exactly the way the gear builds it, rendered to
    //! `Problem`, asserted at its JSON path, reconstructed with
    //! `TryFrom<Problem>` and projected.
    use super::*;
    use crate::reason::{self, aborted};
    use toolkit_canonical_errors::{Http, Problem, resource_error};

    #[resource_error(gts_id!("cf.core.credstore.credential.v1~"))]
    struct CredentialScope;

    /// canonical -> `Problem` -> JSON, and back to a canonical error.
    fn wire(err: CanonicalError) -> (serde_json::Value, CanonicalError) {
        let problem = Problem::from(err);
        let json = serde_json::to_value(&problem).expect("Problem serializes");
        let restored = CanonicalError::try_from(problem).expect("Problem reconstructs");
        (json, restored)
    }

    fn aborted_with(reason: &str, detail: &str) -> CanonicalError {
        CredentialScope::aborted(detail)
            .with_reason(reason)
            .create()
    }

    fn precondition(subject: &str, detail: &str, reason: &str) -> CanonicalError {
        CredentialScope::failed_precondition()
            .with_precondition_violation(subject, detail, reason)
            .create()
    }

    fn invalid(field: &str, reason: &str) -> CanonicalError {
        CredentialScope::invalid_argument()
            .with_field_violation(field, "bad", reason)
            .create()
    }

    #[test]
    fn aborted_constants_land_in_context_reason_and_project() {
        let optimistic = aborted::OPTIMISTIC_LOCK_FAILURE;
        let (json, back) = wire(aborted_with(
            optimistic,
            "credential version precondition failed",
        ));
        assert_eq!(json["context"]["reason"], optimistic);
        assert!(matches!(
            CredStoreError::from(back),
            CredStoreError::PreconditionFailed
        ));

        for (wire_reason, expected) in [
            (aborted::TYPE_IMMUTABLE, AbortedReason::TypeImmutable),
            (
                aborted::TYPE_MISMATCH_WITH_INHERITED,
                AbortedReason::TypeMismatchWithInherited,
            ),
            (
                aborted::TYPE_MISMATCH_WITH_DESCENDANT,
                AbortedReason::TypeMismatchWithDescendant,
            ),
        ] {
            let (json, back) = wire(aborted_with(wire_reason, "type differs"));
            assert_eq!(json["context"]["reason"], wire_reason);
            match CredStoreError::from(back) {
                CredStoreError::TypeConflict { reason, detail } => {
                    assert_eq!(reason, expected);
                    assert_eq!(detail, "type differs");
                }
                other => panic!("expected TypeConflict for {wire_reason}, got {other:?}"),
            }
        }
    }

    #[test]
    fn failed_precondition_constants_land_in_violation_type_and_project() {
        // SECRET_EXPIRED is pinned to HTTP 409 by an explicit override.
        let expired = CredentialScope::failed_precondition()
            .with_precondition_violation(
                "secret",
                "the credential's secret has expired; renew the credential",
                reason::failed_precondition::SECRET_EXPIRED,
            )
            .with_override(Http::status_code(409))
            .create();
        let (json, back) = wire(expired);
        assert_eq!(
            json["context"]["violations"][0]["type"],
            reason::failed_precondition::SECRET_EXPIRED
        );
        assert!(matches!(
            CredStoreError::from(back),
            CredStoreError::SecretExpired
        ));

        let (json, back) = wire(precondition(
            "sharing",
            "private to shared",
            reason::failed_precondition::UNSUPPORTED_TRANSITION,
        ));
        assert_eq!(
            json["context"]["violations"][0]["type"],
            reason::failed_precondition::UNSUPPORTED_TRANSITION
        );
        match CredStoreError::from(back) {
            CredStoreError::UnsupportedTransition { detail } => {
                assert_eq!(detail, "private to shared");
            }
            other => panic!("expected UnsupportedTransition, got {other:?}"),
        }
    }

    #[test]
    fn permission_denied_constant_lands_in_context_reason_and_projects() {
        let err = CredentialScope::permission_denied()
            .with_reason(reason::permission_denied::ACCESS_DENIED)
            .create();
        let (json, back) = wire(err);
        assert_eq!(
            json["context"]["reason"],
            reason::permission_denied::ACCESS_DENIED
        );
        assert!(matches!(
            CredStoreError::from(back),
            CredStoreError::PermissionDenied
        ));
    }

    #[test]
    fn field_constants_land_in_field_violation_reason_and_project() {
        for code in field::ALL {
            let (json, back) = wire(invalid("some_field", code));
            let violations = json["context"]["field_violations"]
                .as_array()
                .unwrap_or_else(|| panic!("field_violations must be an array for {code}"));
            assert_eq!(violations[0]["reason"], *code, "{code}");
            match CredStoreError::from(back) {
                CredStoreError::Invalid {
                    field,
                    reason,
                    detail,
                } => {
                    assert_eq!(field, "some_field");
                    assert_eq!(reason.as_wire(), *code);
                    assert!(!matches!(reason, field::Reason::Unknown(_)), "{code}");
                    assert_eq!(detail, "bad");
                }
                other => panic!("expected Invalid for {code}, got {other:?}"),
            }
        }
    }

    #[test]
    fn gear_field_names_survive_the_round_trip() {
        for (name, code) in [
            ("reference", field::INVALID_SECRET_REF),
            ("If-Match", field::INVALID_IF_MATCH),
            ("If-Match", field::IF_MATCH_REQUIRED),
            ("$orderby", field::ORDER_WITH_CURSOR),
            ("body", field::INVALID_MERGE_PATCH_BODY),
        ] {
            let (_, back) = wire(invalid(name, code));
            match CredStoreError::from(back) {
                CredStoreError::Invalid { field, reason, .. } => {
                    assert_eq!(field, name);
                    assert_eq!(reason.as_wire(), code);
                }
                other => panic!("expected Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn unsupported_media_type_keeps_its_415_override_on_the_wire() {
        let err = CredentialScope::invalid_argument()
            .with_field_violation("Content-Type", "wrong", field::UNSUPPORTED_MEDIA_TYPE)
            .with_override(Http::status_code(415))
            .create();
        let (json, back) = wire(err);
        assert_eq!(
            json["context"]["field_violations"][0]["reason"],
            field::UNSUPPORTED_MEDIA_TYPE
        );
        assert!(matches!(
            CredStoreError::from(back),
            CredStoreError::Invalid {
                reason: field::Reason::UnsupportedMediaType,
                ..
            }
        ));
    }

    #[test]
    fn not_found_and_already_exists_project() {
        let not_found = CredentialScope::not_found("credential not found")
            .with_resource("credential")
            .create();
        let (_, back) = wire(not_found);
        assert!(CredStoreError::from(back).is_not_found());

        let exists = CredentialScope::already_exists("credential already exists")
            .with_resource("credential")
            .create();
        let (_, back) = wire(exists);
        assert!(CredStoreError::from(back).is_already_exists());
    }

    #[test]
    fn service_unavailable_projects_with_and_without_a_retry_hint() {
        let hinted = CanonicalError::service_unavailable()
            .with_detail("store busy")
            .with_retry_after_seconds(30)
            .create();
        let (_, back) = wire(hinted);
        let projected = CredStoreError::from(back);
        assert!(projected.is_unavailable());
        assert_eq!(projected.retry_after(), Some(Duration::from_secs(30)));

        let bare = CanonicalError::service_unavailable()
            .with_detail("no storage plugin registered")
            .create();
        let (_, back) = wire(bare);
        match CredStoreError::from(back) {
            CredStoreError::Unavailable {
                detail,
                retry_after: None,
            } => assert_eq!(detail, "no storage plugin registered"),
            other => panic!("expected Unavailable without hint, got {other:?}"),
        }
    }

    #[test]
    fn internal_projects_with_the_in_process_detail() {
        let projected = CredStoreError::from(CanonicalError::internal("boom").create());
        match projected {
            CredStoreError::Internal { detail } => assert!(!detail.is_empty()),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn unmodeled_categories_and_reasons_fall_through_to_other() {
        let cases = [
            CredentialScope::deadline_exceeded("slow").create(),
            aborted_with("SOMETHING_NEW", "x"),
            precondition("x", "y", "SOMETHING_NEW"),
        ];
        for canonical in cases {
            let gts = canonical.gts_type();
            match CredStoreError::from(canonical) {
                CredStoreError::Other { canonical } => assert_eq!(canonical.gts_type(), gts),
                other => panic!("expected Other, got {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_without_a_field_violation_projects_empty_field_and_unknown_reason() {
        let canonical = CredentialScope::invalid_argument()
            .with_format("not a date")
            .create();
        match CredStoreError::from(canonical) {
            CredStoreError::Invalid { field, reason, .. } => {
                assert!(field.is_empty());
                assert_eq!(reason, field::Reason::Unknown(String::new()));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn display_strings_are_stable() {
        assert_eq!(CredStoreError::NotFound.to_string(), "secret not found");
        assert_eq!(
            CredStoreError::AlreadyExists.to_string(),
            "secret already exists"
        );
        assert_eq!(
            CredStoreError::PreconditionFailed.to_string(),
            "write precondition failed"
        );
        assert_eq!(CredStoreError::SecretExpired.to_string(), "secret expired");
        assert_eq!(
            CredStoreError::PermissionDenied.to_string(),
            "access denied"
        );
        assert_eq!(
            CredStoreError::TypeConflict {
                reason: AbortedReason::TypeImmutable,
                detail: "x".to_owned(),
            }
            .to_string(),
            "secret type conflict (TYPE_IMMUTABLE): x"
        );
    }

    #[test]
    fn predicates_match_only_their_own_variant() {
        assert!(CredStoreError::NotFound.is_not_found());
        assert!(CredStoreError::AlreadyExists.is_already_exists());
        assert!(CredStoreError::PreconditionFailed.is_precondition_failed());
        assert!(CredStoreError::SecretExpired.is_secret_expired());
        assert!(CredStoreError::PermissionDenied.is_permission_denied());
        assert!(!CredStoreError::NotFound.is_secret_expired());
        assert!(!CredStoreError::SecretExpired.is_not_found());
        assert!(!CredStoreError::AlreadyExists.is_precondition_failed());
        assert!(!CredStoreError::PreconditionFailed.is_already_exists());
        assert!(!CredStoreError::NotFound.is_unavailable());
        assert_eq!(CredStoreError::NotFound.retry_after(), None);
    }

    #[test]
    fn invalid_secret_ref_is_an_invalid_argument_on_the_credential_resource() {
        let canonical = crate::SecretRef::new("bad ref").expect_err("space is not allowed");
        assert_eq!(
            canonical.resource_type(),
            Some(crate::CREDENTIAL_RESOURCE_TYPE)
        );
        let (json, back) = wire(canonical);
        assert_eq!(
            json["context"]["field_violations"][0]["reason"],
            field::INVALID_SECRET_REF
        );
        match CredStoreError::from(back) {
            CredStoreError::Invalid { field, reason, .. } => {
                assert_eq!(field, "reference");
                assert_eq!(reason, field::Reason::InvalidSecretRef);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}

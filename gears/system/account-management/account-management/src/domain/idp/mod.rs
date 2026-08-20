//! AM-internal mapping from the SDK `IdP` failure shapes to
//! [`crate::domain::error::DomainError`].
//!
//! The trait + DTO are stable contract and live in
//! [`account_management_sdk::idp`] (see that gear's docs for the
//! provisioner contract). This module owns the **mapping** from those
//! plugin-facing failure variants onto AM's internal error taxonomy
//! via the crate-private extension trait
//! [`ProvisionFailureExt::into_domain_error`].
//!
//! The method takes a `tenant_id: Uuid` so the per-conversion
//! `tracing::warn!` carries enough structured context for operator
//! grep — without that field a stack of identical `am.idp` warns from
//! different tenants becomes impossible to attribute. We deliberately
//! do NOT provide `From<IdpProvisionFailure> for DomainError`: the
//! conversion has a mandatory side effect (redaction + structured log
//! emit) and a mandatory context input (`tenant_id`). Forcing every
//! call site through `into_domain_error(tenant_id)` makes the
//! requirement locally visible and prevents an accidental
//! `?`-propagation that would emit a context-less log line.
//!
//! The symmetric `DeprovisionFailureExt` is intentionally NOT defined
//! at this revision — there are no production callers (the retention
//! and reaper pipelines and the saga step-3 compensator all
//! pattern-match on the variant manually). It will land together
//! with its first real consumer rather than as dead infrastructure;
//! the rationale is restated next to the trait's eventual home at
//! the bottom of this file.
//!
//! The mapping redacts provider-supplied detail strings (which can
//! carry vendor SDK error text, hostnames, or token-bearing fragments)
//! through [`redact_provider_detail`] and emits a `tracing::warn!` on
//! `am.idp` so operators can correlate the redacted public envelope
//! back to the raw provider response via the digest + length pair.
//!
//! [`ServiceAccountFailureExt`] — the third mapping in this file — is
//! deliberately stricter: it forwards no digest either, discarding the
//! adapter's text outright in favour of fixed AM-owned messages. See the
//! rationale block above that trait.

use std::hash::Hasher;

use account_management_sdk::{
    IdpProvisionFailure, IdpServiceAccountFailure, IdpUserOperationFailure,
};
use fnv::FnvHasher;
use uuid::Uuid;

use crate::domain::error::{DomainError, UnsupportedResource};

/// Stable, non-secret correlation handle for a provider-supplied error
/// detail. The raw text can carry vendor SDK strings, hostnames, or
/// token-bearing fragments — even into operator logs (which have a
/// longer retention horizon than the request envelope) those values
/// must not surface verbatim. Operators correlate the hash + length
/// across `am.idp` log events and audit rows; the inverse mapping
/// stays inside the audit-only `Internal::diagnostic` field where
/// access is governed by the audit-storage policy.
///
/// FNV-1a is chosen over [`std::hash::DefaultHasher`] because the
/// stdlib hasher's algorithm is explicitly unspecified and may change
/// between Rust toolchain versions — that would silently desync
/// digests emitted by older binaries from the same input hashed in a
/// newer one, breaking the cross-upgrade correlation a forensic
/// handle exists for. FNV-1a is spec-pinned, so a digest emitted
/// today still matches one emitted next year against the same input.
/// Collision resistance is not a concern here (non-cryptographic;
/// the inverse mapping lives in the audit-only `diagnostic` field).
///
/// We feed the bytes directly via [`Hasher::write`] rather than the
/// [`Hash`] trait because `Hash::hash` for `str` has no documented
/// stability guarantee across compiler versions (the std docs
/// explicitly warn that the byte stream `Hash` produces "should not
/// be considered stable between compiler versions"). FNV-1a being
/// spec-pinned only protects the *algorithm*; the cross-upgrade
/// digest stability we promise above also requires a spec-pinned
/// *encoding* of the input. UTF-8 bytes from `as_bytes()` are that
/// stable encoding. Length is reported in `chars` to keep the public
/// log field a Unicode-grapheme-aligned magnitude, while the digest
/// commits to the byte sequence one-to-one.
pub(crate) fn redact_provider_detail(detail: &str) -> (u64, usize) {
    let mut hasher = FnvHasher::default();
    hasher.write(detail.as_bytes());
    (hasher.finish(), detail.chars().count())
}

/// Typed, log-safe wrapper attached on the `cause` chain of
/// [`DomainError::Internal`] when an `IdP` failure maps onto an
/// internal-error envelope. Keeps the chain walkable via
/// `Error::source` without forwarding raw vendor text through
/// `Display` — operators correlate via the redacted digest + length
/// that already lives on the `am.idp` `tracing::warn!` line.
///
/// Variants are kept minimal: today only `Ambiguous` chains a cause
/// (the other arms either redact into `UnsupportedOperation` /
/// `ServiceUnavailable` envelopes where the chain is not needed, or
/// remain unknown / future-extension paths).
#[toolkit_macros::domain_model]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum RedactedProvisionFailure {
    #[error(
        "idp provision ambiguous outcome (provider detail redacted; digest=0x{digest:016x} len={len})"
    )]
    Ambiguous { digest: u64, len: usize },
}

impl RedactedProvisionFailure {
    pub(crate) const fn ambiguous(digest: u64, len: usize) -> Self {
        Self::Ambiguous { digest, len }
    }
}

/// Map [`IdpProvisionFailure`] onto the [`DomainError`] taxonomy with
/// caller-supplied `tenant_id` context.
///
/// * `CleanFailure` → [`DomainError::ServiceUnavailable`] (HTTP 503;
///   compensation already ran; AM proved no provider state was
///   retained). Provider-supplied `detail` is **not** forwarded into
///   the public envelope: vendor SDK strings can carry endpoint
///   names, hostnames, or token-bearing fragments, and the
///   `with_detail` contract on `toolkit-canonical-errors` mandates
///   pre-redacted public text. The raw detail is logged at `am.idp`
///   and reaches operators via trace correlation.
/// * `Ambiguous` → [`DomainError::Internal`] (HTTP 500). The provider
///   may have retained state; the provisioning reaper compensates
///   asynchronously. The raw detail is redacted and digested; the
///   diagnostic field carries only the digest + length.
/// * `UnsupportedOperation` → [`DomainError::UnsupportedOperation`]
///   (HTTP 501); the boundary mapping further redacts the public
///   message (provider detail kept private — see
///   `infra::canonical_mapping`).
pub(crate) trait ProvisionFailureExt {
    fn into_domain_error(self, tenant_id: Uuid) -> DomainError;
}

impl ProvisionFailureExt for IdpProvisionFailure {
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat 3-arm match with redact + warn! per arm; splitting fragments the variant->DomainError mapping reviewers must eyeball-check"
    )]
    fn into_domain_error(self, tenant_id: Uuid) -> DomainError {
        match self {
            Self::CleanFailure { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP provision CleanFailure; surfacing generic ServiceUnavailable, raw detail redacted (correlate via digest + audit-only diagnostic)"
                );
                DomainError::service_unavailable(
                    "identity provider unavailable; provisioning compensated",
                )
            }
            Self::Ambiguous { detail } => {
                // `DomainError::Internal::diagnostic` does NOT reach
                // the public `Problem.detail` — the canonical-errors
                // boundary stores it in `InternalV1::description`,
                // which is `#[serde(skip)]` (see
                // `libs/toolkit-canonical-errors/src/context.rs`), and
                // the public envelope uses the fixed
                // `__internal()` placeholder. The redaction here is
                // therefore log-safety, not envelope-safety: vendor
                // text would otherwise reach the long-retention
                // `am.idp` warn line below and (if `cause` is later
                // chained through `Display`) the audit trail.
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP provision Ambiguous outcome; surfacing generic Internal, raw detail redacted"
                );
                // Preserve the SDK failure on `cause` so the error
                // chain stays walkable (`Error::source`) for retry-
                // classification and any future
                // `feature-errors-observability` consumer. The
                // wrapper struct hides the raw `detail` from `Display`
                // — only the redacted digest/len reaches stringified
                // sinks — while `Error::source` still exposes the
                // typed failure to downcasters.
                DomainError::Internal {
                    diagnostic: format!(
                        "idp provision ambiguous outcome (provider detail redacted; \
                         digest=0x{digest:016x} len={len})"
                    ),
                    cause: Some(Box::new(RedactedProvisionFailure::ambiguous(digest, len))),
                }
            }
            Self::UnsupportedOperation { detail } => {
                // The canonical-mapping boundary logs `detail` on a
                // `tracing::warn!` for operator correlation; that
                // makes the variant's `detail` field part of the
                // operator-log surface. Redact it at construction so
                // the long-lived log retains correlation without the
                // raw vendor text.
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP provision UnsupportedOperation; raw detail redacted"
                );
                DomainError::UnsupportedOperation {
                    resource: UnsupportedResource::Tenant,
                    detail: format!(
                        "provider declined the operation (detail redacted; \
                         digest=0x{digest:016x} len={len})"
                    ),
                }
            }
            Self::InvalidInput { detail, field } => {
                // Permanent client error: the plugin proved no IdP-side
                // state was attempted because the input failed the
                // plugin's shape-check before any provider call. Surface
                // as `DomainError::IdpInvalidInput { detail, field }` so
                // the SDK boundary keeps the dotted-path `field` as a
                // structured attribute — the canonical envelope then
                // carries it as `field_violations[0].field` with
                // `reason = "IDP_INVALID_INPUT"` instead of squashing
                // it into the free-text detail.
                //
                // The `detail` text comes from the plugin's wire-shape
                // validator (it describes the structural issue, not a
                // vendor error message), so it is safe to surface
                // verbatim in the public envelope. This matches the
                // existing `Validation`/`MetadataValidation` arms:
                // `detail` is already operator-safe by construction.
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    field = field.as_deref().unwrap_or("<unknown>"),
                    "IdP provision InvalidInput; surfacing as 400 idp_invalid_input"
                );
                DomainError::IdpInvalidInput { detail, field }
            }
            // SDK enum is `#[non_exhaustive]`. A new variant added in a
            // future SDK release lands here until the AM-side mapping
            // is updated; surface as `Internal` with a loud `error!`
            // so the gap shows up in operator logs the moment the
            // new variant flows through.
            other => {
                let label = other.as_metric_label();
                tracing::error!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    variant = label,
                    "unknown IdpProvisionFailure variant; mapping conservatively to Internal -- update ProvisionFailureExt::into_domain_error"
                );
                DomainError::Internal {
                    diagnostic: format!(
                        "idp provision unknown failure variant `{label}` (raw detail redacted; \
                         update ProvisionFailureExt::into_domain_error)"
                    ),
                    cause: None,
                }
            }
        }
    }
}

// `IdpDeprovisionFailure → DomainError` has no production callers
// today: the retention pipeline ([`super::tenant::service::retention`])
// and the reaper ([`super::tenant::service::reaper`]) both
// pattern-match on the variant and decide per-arm; the saga
// step-3 compensator ([`super::tenant::service::TenantService::compensate_failed_activation`])
// likewise matches manually. The conversion shape (with the same
// redaction + per-variant `DomainError` mapping as the
// `IdpProvisionFailure` side) lands together with its first real
// caller — typically an admin endpoint that surfaces a deprovision
// outcome to a caller — rather than as dead infrastructure here.

/// Map [`IdpUserOperationFailure`] onto the [`DomainError`] taxonomy with
/// caller-supplied `tenant_id` context.
///
/// * `Unavailable` → [`DomainError::IdpUnavailable`] (HTTP 503). The
///   provider was unreachable, the call timed out, or the transport
///   returned a retryable failure. AM holds no fallback projection
///   per `cpt-cf-account-management-constraint-no-user-storage`.
/// * `UnsupportedOperation` → [`DomainError::UnsupportedOperation`]
///   (HTTP 501). The provider declined a mutating operation in its
///   current implementation profile.
/// * `Rejected` → [`DomainError::Validation`] (HTTP 400). The
///   provider returned a payload-rejection category; the canonical
///   mapping refinement is owned by `feature-errors-observability`
///   and may diverge per provider in a follow-up. The Validation
///   surface is the most conservative public envelope today.
/// * `FieldNotWritable` → [`DomainError::IdpFieldNotWritable`] (HTTP
///   400). The attributes are provider-managed and not overridable, so
///   the envelope carries one set of structured `<field>` /
///   `IDP_MANAGED_FIELD` field-violation tokens per refused attribute
///   rather than the generic `request` / `VALIDATION` pair. An empty
///   refused set is a provider contract violation and degrades to
///   [`DomainError::Validation`].
///
/// Provider-supplied detail strings are redacted via
/// [`redact_provider_detail`] before reaching public envelopes:
/// vendor SDK text can carry endpoint names, hostnames, or token-
/// bearing fragments. Operators correlate redacted public envelopes
/// to raw provider responses via the digest + length emitted on the
/// `am.idp` `tracing::warn!` line.
pub(crate) trait UserOperationFailureExt {
    fn into_domain_error(self, tenant_id: Uuid) -> DomainError;
}

impl UserOperationFailureExt for IdpUserOperationFailure {
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat 3-arm match with redact + warn! per arm; splitting fragments the variant->DomainError mapping reviewers must eyeball-check"
    )]
    fn into_domain_error(self, tenant_id: Uuid) -> DomainError {
        match self {
            // @cpt-begin:cpt-cf-account-management-dod-idp-user-operations-contract-idp-unavailability-contract:p1:inst-dod-idp-user-operations-contract-unavailability-mapping
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-transport-failure
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-transport-return
            Self::Unavailable { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation Unavailable; surfacing IdpUnavailable, raw detail redacted (correlate via digest)"
                );
                DomainError::IdpUnavailable {
                    detail: format!(
                        "identity provider unavailable for user operation (detail redacted; \
                         digest=0x{digest:016x} len={len})"
                    ),
                }
            }
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-transport-return
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-transport-failure
            // @cpt-end:cpt-cf-account-management-dod-idp-user-operations-contract-idp-unavailability-contract:p1:inst-dod-idp-user-operations-contract-unavailability-mapping
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-unsupported-branch
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-unsupported-return
            Self::UnsupportedOperation { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation UnsupportedOperation; raw detail redacted"
                );
                DomainError::UnsupportedOperation {
                    resource: UnsupportedResource::User,
                    detail: format!(
                        "provider declined the user operation (detail redacted; \
                         digest=0x{digest:016x} len={len})"
                    ),
                }
            }
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-unsupported-return
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-unsupported-branch
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-provider-error
            // @cpt-begin:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-provider-error-return
            Self::Rejected { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation Rejected; surfacing Validation, raw detail redacted"
                );
                DomainError::Validation {
                    detail: format!(
                        "identity provider rejected the user operation (detail redacted; \
                         digest=0x{digest:016x} len={len})"
                    ),
                }
            }
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-provider-error-return
            // @cpt-end:cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation:p1:inst-algo-ici-provider-error
            // Classified uniqueness collision: surface HTTP
            // 409 `already_exists` with the stable colliding-field
            // token, instead of collapsing into the redacted generic
            // Validation. The provider detail is still digest-only in
            // logs — the public detail is derived from the typed field
            // at the canonical boundary.
            Self::DuplicateUser { field, detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    field = field.as_field_token(),
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation DuplicateUser; surfacing already_exists, raw detail redacted"
                );
                DomainError::UserAlreadyExists { field }
            }
            // Classified password-policy reject: surface
            // the structured `password` / `PASSWORD_POLICY` violation
            // instead of the generic `request` / `VALIDATION`. Raw
            // policy text stays digest-only in logs.
            Self::PasswordPolicy { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation PasswordPolicy; surfacing password field violation, raw detail redacted"
                );
                DomainError::IdpPasswordPolicy {
                    detail: "the supplied password does not meet the identity provider's password policy"
                        .to_owned(),
                }
            }
            // Classified non-writable-attribute reject: surface one
            // structured `<field>` / `IDP_MANAGED_FIELD` violation per
            // refused attribute so a client can disable the exact
            // inputs, instead of collapsing into the redacted generic
            // Validation. The typed attributes are safe to log
            // un-redacted (they are AM's own field tokens, not vendor
            // text); only the provider `detail` is digested.
            Self::FieldNotWritable { fields, detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                // A provider MUST name what it refused; an empty set
                // carries no attribution, so it degrades to the generic
                // rejection rather than a 400 pointing at nothing.
                if fields.is_empty() {
                    tracing::warn!(
                        target: "am.idp",
                        tenant_id = %tenant_id,
                        provider_detail_digest = digest,
                        provider_detail_len = len,
                        "IdP user operation FieldNotWritable with an empty field set (provider contract violation); surfacing Validation, raw detail redacted"
                    );
                    return DomainError::Validation {
                        detail: format!(
                            "identity provider rejected the user operation (detail redacted; \
                             digest=0x{digest:016x} len={len})"
                        ),
                    };
                }
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    fields = fields.iter().map(|f| f.as_field_token()).collect::<Vec<_>>().join(","),
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation FieldNotWritable; surfacing IDP_MANAGED_FIELD violations, raw detail redacted"
                );
                DomainError::IdpFieldNotWritable { fields }
            }
            // Absent target user. `update_user` surfaces this as a
            // `404`; the shared mapping here carries no `user_id`, so
            // `UserService::update_user` pre-empts this arm to attach
            // the precise resource identifier. Kept explicit (rather
            // than falling through to the `Internal`/"unknown variant"
            // arm below) so a `NotFound` from any path is a clean 404.
            Self::NotFound { detail } => {
                let (digest, len) = redact_provider_detail(&detail);
                tracing::warn!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "IdP user operation NotFound; surfacing UserNotFound, raw detail redacted"
                );
                DomainError::UserNotFound {
                    detail: "user not found for the requested operation".to_owned(),
                    resource: String::new(),
                }
            }
            // SDK enum is `#[non_exhaustive]`. A new variant added in
            // a future SDK release lands here until the AM-side
            // mapping is updated; surface as `Internal` with a loud
            // `error!` so the gap shows up in operator logs the
            // moment the new variant flows through.
            other => {
                let label = other.as_metric_label();
                tracing::error!(
                    target: "am.idp",
                    tenant_id = %tenant_id,
                    variant = label,
                    "unknown IdpUserOperationFailure variant; mapping conservatively to Internal -- update UserOperationFailureExt::into_domain_error"
                );
                DomainError::Internal {
                    diagnostic: format!(
                        "idp user operation unknown failure variant `{label}` (raw detail redacted; \
                         update UserOperationFailureExt::into_domain_error)"
                    ),
                    cause: None,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Service-account failures — discard, do not digest
// ---------------------------------------------------------------------------

// Adapter-supplied `detail`/`field` text is UNTRUSTED, and on the
// service-account surface it is never relayed to a caller nor logged,
// in any form — not even as the digest the tenant / user halves above
// forward. Character filtering was tried and removed: it cannot tell a
// credential (`secret=abc123` is pure ASCII graphic text) from
// legitimate operator prose, so it launders a leak into a "sanitized"
// response instead of preventing it, and no heuristic closes that gap.
// Length caps and control-character stripping likewise only bound the
// size of a leak.
//
// The asymmetry with `ProvisionFailureExt` / `UserOperationFailureExt`
// is deliberate: those surfaces provision *humans*, whose vendor errors
// describe profile fields, while every service-account call is about a
// live machine credential — the one place a vendor error string is most
// likely to quote the secret it just minted. So the conversions below
// discard the adapter's text entirely and substitute one of these fixed,
// AM-owned messages; adapters that need diagnostics MUST log them
// in-process, where they hold the context to decide what is safe (see
// `IdpServiceAccountFailure` in the SDK). ASCII only:
// `non_ascii_literal` is denied workspace-wide.

/// Public message for a provider-reported invalid-input rejection (400).
pub(crate) const SA_INVALID_INPUT_MESSAGE: &str = "the provider rejected the request as invalid";

/// Public message for a provider-reported clean failure (503). Names no
/// vendor, host, or internal condition, and says retry is safe.
pub(crate) const SA_UPSTREAM_MESSAGE: &str =
    "the service-account provider could not complete the request";

/// Public message for a provider-reported ambiguous outcome (409).
/// Worded to steer the caller into reconciliation (list the tenant,
/// match the submitted name) rather than a blind retry, which could
/// collide with an account the uncertain call already created.
pub(crate) const SA_AMBIGUOUS_MESSAGE: &str =
    "list the tenant and reconcile by the submitted name instead of retrying";

/// Public message for a provider that does not implement
/// service-account management at all (501).
pub(crate) const SA_UNSUPPORTED_MESSAGE: &str =
    "the identity provider does not support service accounts";

/// Record the operator's breadcrumb for an adapter-sourced failure: the
/// variant label, how long the discarded text was, and whether a field
/// was attributed. Deliberately no text argument anywhere — the
/// adapter's `detail`/`field` never reach a `tracing` macro, so no
/// subscriber, log file, or aggregator can hold what the response
/// withholds. Adapters that need the vendor's reason recorded must log
/// it themselves.
// Inflated by the `tracing` macros, which each expand to a callsite
// guard plus a field-recording closure. The function itself is one flat
// match over a closed taxonomy; splitting it would only scatter the
// log-safety argument above across several call sites.
#[allow(clippy::cognitive_complexity)]
// @cpt-begin:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-log-level
fn log_service_account_failure(err: &IdpServiceAccountFailure, tenant_id: Uuid) {
    let failure = err.as_metric_label();
    let detail_len = err.detail().len();
    match err {
        // `debug!`, not `warn!`: a 400 is caller-attributable input
        // rejection, so it belongs in a diagnosis session, not in the
        // default-level stream an operator watches for provider trouble.
        IdpServiceAccountFailure::InvalidInput { field, .. } => tracing::debug!(
            target: "am.idp",
            tenant_id = %tenant_id,
            failure,
            detail_len,
            field_present = field.is_some(),
            "service-account provider rejected the request as invalid"
        ),
        // Routine and expected (it is how an idempotent revoke confirms
        // absence), so it earns no record of its own.
        IdpServiceAccountFailure::NotFound { .. } => {}
        IdpServiceAccountFailure::CleanFailure { .. } => tracing::warn!(
            target: "am.idp",
            tenant_id = %tenant_id,
            failure,
            detail_len,
            "service-account provider reported a clean failure"
        ),
        IdpServiceAccountFailure::Ambiguous { .. } => tracing::warn!(
            target: "am.idp",
            tenant_id = %tenant_id,
            failure,
            detail_len,
            "service-account provider reported an ambiguous outcome"
        ),
        IdpServiceAccountFailure::UnsupportedOperation { .. } => tracing::warn!(
            target: "am.idp",
            tenant_id = %tenant_id,
            failure,
            detail_len,
            "service-account provider declined the operation as unsupported"
        ),
        other => tracing::error!(
            target: "am.idp",
            tenant_id = %tenant_id,
            variant = other.as_metric_label(),
            "unknown IdpServiceAccountFailure variant; update ServiceAccountFailureExt::into_domain_error"
        ),
    }
}
// @cpt-end:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-log-level

/// Map [`IdpServiceAccountFailure`] onto the [`DomainError`] taxonomy
/// with caller-supplied `tenant_id` context.
///
/// * `InvalidInput` → [`DomainError::ServiceAccountInvalidInput`]
///   (HTTP 400). The provider retained no state; the violation is
///   attributed to the request as a whole, never to the adapter's own
///   field name.
/// * `NotFound` → [`DomainError::ServiceAccountNotFound`] (HTTP 404)
///   carrying `resource` — see the parameter note below. `revoke` folds
///   the variant into success before reaching here.
/// * `CleanFailure` → [`DomainError::IdpUnavailable`] (HTTP 503) —
///   nothing was retained, so retrying the same request is safe.
/// * `Ambiguous` → [`DomainError::ServiceAccountAmbiguous`] (HTTP
///   409), NOT 503: a half-applied provision means "retry the same
///   request" would hit `InvalidInput` ("name already live"), so the
///   caller is steered to reconcile instead.
/// * `UnsupportedOperation` → [`DomainError::UnsupportedOperation`]
///   (HTTP 501) — the deployment's adapter ships no service-account
///   support (the SDK default impls).
///
/// Every arm drops the adapter's payload and answers with a fixed
/// message, so no binding from the failure can reach the constructed
/// variant.
///
/// # The `resource` parameter
///
/// `resource` is the address the CALLER supplied for this operation —
/// the `client_id` on rotate / revoke, the submitted `name` on create.
/// It is the caller's own input, never adapter text, so echoing it into
/// the AIP-193 envelope's `resource_name` is safe and is what makes a
/// 404 actionable. Pass `""` only where the operation addresses no
/// single account (a tenant-wide `list`), which is also the one path
/// where the provider reporting `NotFound` is a contract violation
/// rather than a real miss.
pub(crate) trait ServiceAccountFailureExt {
    fn into_domain_error(self, tenant_id: Uuid, resource: &str) -> DomainError;

    /// Map a failure from the non-retaining list operation. Provider
    /// uncertainty is retry-safe for a read and therefore becomes 503 rather
    /// than the mutation-oriented 409 reconciliation response. Every branch
    /// still emits the same safe failure metadata as the shared mapping.
    fn into_list_domain_error(self, tenant_id: Uuid) -> DomainError;
}

impl ServiceAccountFailureExt for IdpServiceAccountFailure {
    // @cpt-begin:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-select-fixed-message
    // @cpt-begin:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-neutral-field
    // @cpt-begin:cpt-cf-account-management-dod-service-accounts-no-adapter-text:p1:inst-dod-sa-no-adapter-text-boundary
    fn into_domain_error(self, tenant_id: Uuid, resource: &str) -> DomainError {
        // @cpt-begin:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-safe-metadata-log
        log_service_account_failure(&self, tenant_id);
        // @cpt-end:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-safe-metadata-log
        match self {
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-invalid-input-return
            Self::InvalidInput { .. } => DomainError::ServiceAccountInvalidInput {
                detail: SA_INVALID_INPUT_MESSAGE.to_owned(),
            },
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-invalid-input-return
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-not-found-return
            Self::NotFound { .. } => DomainError::ServiceAccountNotFound {
                detail: format!("service account not found in tenant {tenant_id}"),
                resource: resource.to_owned(),
            },
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-not-found-return
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-clean-failure-return
            Self::CleanFailure { .. } => DomainError::IdpUnavailable {
                detail: SA_UPSTREAM_MESSAGE.to_owned(),
            },
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-clean-failure-return
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-ambiguous-return
            // @cpt-begin:cpt-cf-account-management-dod-service-accounts-ambiguous-outcome:p1:inst-dod-sa-ambiguous-outcome-domain
            Self::Ambiguous { .. } => DomainError::ServiceAccountAmbiguous {
                detail: SA_AMBIGUOUS_MESSAGE.to_owned(),
            },
            // @cpt-end:cpt-cf-account-management-dod-service-accounts-ambiguous-outcome:p1:inst-dod-sa-ambiguous-outcome-domain
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-ambiguous-return
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-unsupported-return
            Self::UnsupportedOperation { .. } => DomainError::UnsupportedOperation {
                resource: UnsupportedResource::ServiceAccount,
                detail: SA_UNSUPPORTED_MESSAGE.to_owned(),
            },
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-contract-invocation:p1:inst-algo-sa-contract-invocation-unsupported-return
            // SDK enum is `#[non_exhaustive]`. A new variant added in a
            // future SDK release lands here until the AM-side mapping is
            // updated. It maps to `Internal` for the same reason
            // `UserOperationFailureExt` does: an unclassified provider
            // outcome is an AM-side mapping gap, and answering 500
            // states that honestly instead of asserting the 503
            // "nothing was retained, retry is safe" contract we cannot
            // actually vouch for. `log_service_account_failure` has
            // already emitted the loud `error!` that surfaces the gap.
            //
            // `diagnostic` names only the category label — never the
            // provider's text. It does not reach the public envelope
            // either (the canonical boundary keeps it in a
            // `#[serde(skip)]` field), so an unmapped variant cannot
            // become the one path that leaks what every other arm
            // discards.
            // @cpt-begin:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-unknown-category
            other => DomainError::Internal {
                diagnostic: format!(
                    "service-account provider returned unmapped failure category \
                     `{}` (adapter detail discarded; update \
                     ServiceAccountFailureExt::into_domain_error)",
                    other.as_metric_label()
                ),
                cause: None,
            },
            // @cpt-end:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-unknown-category
        }
    }
    // @cpt-end:cpt-cf-account-management-dod-service-accounts-no-adapter-text:p1:inst-dod-sa-no-adapter-text-boundary
    // @cpt-end:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-neutral-field
    // @cpt-end:cpt-cf-account-management-algo-service-accounts-adapter-text-discard:p1:inst-algo-sa-text-discard-select-fixed-message

    fn into_list_domain_error(self, tenant_id: Uuid) -> DomainError {
        match self {
            ambiguous @ Self::Ambiguous { .. } => {
                // The list-specific 503 remap must not bypass the mandatory
                // safe metadata record. Log the category and discarded-text
                // length exactly once, then return an AM-owned message.
                log_service_account_failure(&ambiguous, tenant_id);
                DomainError::IdpUnavailable {
                    detail: SA_UPSTREAM_MESSAGE.to_owned(),
                }
            }
            // `list` addresses the tenant rather than one account. Reuse the
            // shared mapper for every other category so logging, redaction,
            // unknown-variant handling, and fixed messages cannot drift.
            other => other.into_domain_error(tenant_id, ""),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "idp_tests.rs"]
mod tests;

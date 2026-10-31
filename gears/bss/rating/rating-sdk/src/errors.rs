//! Canonical errors of the Rating contract (DESIGN §3.3).
//!
//! Every SDK method returns `Result<T, CanonicalError>`; REST maps the same error to RFC 9457
//! problem details. The resources below are the GTS resource types of DESIGN §4.8, so a refusal
//! names the resource type it is about. An object outside the caller's scope is `NotFound`,
//! indistinguishable from an absent one; a selector outside the scope on a collection read is
//! `PermissionDenied`.
//!
//! Reason codes are an open set: a consumer treats an unknown code as "not understood", never as a
//! default (T-D-71).

/// Usage records, exceptions, inbox entries and source losses.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.usage.v1~"))]
pub struct UsageResource;

/// Children, window and fact results, deliveries and snapshots.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.result.v1~"))]
pub struct ResultResource;

/// Administrative re-rate runs.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.rerate.v1~"))]
pub struct RerateResource;

/// Pre-purchase order evaluation.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.evaluation.v1~"))]
pub struct EvaluationResource;

/// Seller finalization policies and their `delay_only` acceptances.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.finalization_policy.v1~"))]
pub struct FinalizationPolicyResource;

/// Platform-default finalization policies.
#[toolkit_canonical_errors::resource_error(gts_id!(
    "cf.bss.rating.platform_finalization_policy.v1~"
))]
pub struct PlatformFinalizationPolicyResource;

/// Source-loss resolution.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.source_loss.v1~"))]
pub struct SourceLossResource;

/// Retention holds.
#[toolkit_canonical_errors::resource_error(gts_id!("cf.bss.rating.retention_hold.v1~"))]
pub struct RetentionHoldResource;

/// Reason codes the Rating contract answers with (an open set).
pub mod reason {
    /// No authenticated caller reached the operation.
    pub const SECURITY_CONTEXT_REQUIRED: &str = "SECURITY_CONTEXT_REQUIRED";
    /// The PDP denied the operation, or a selector lies outside the granted scope.
    pub const PERMISSION_DENIED: &str = "PERMISSION_DENIED";
    /// The request body or a path segment is malformed.
    pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
    /// A command that requires an `Idempotency-Key` arrived without a valid one.
    pub const IDEMPOTENCY_KEY_REQUIRED: &str = "IDEMPOTENCY_KEY_REQUIRED";
    /// An inbox release names an entry that is not quarantined.
    pub const INBOX_ENTRY_NOT_QUARANTINED: &str = "INBOX_ENTRY_NOT_QUARANTINED";
    /// An inbox entry cannot be applied: no feature applies its source yet.
    pub const INBOX_SOURCE_NOT_APPLICABLE: &str = "INBOX_SOURCE_NOT_APPLICABLE";
    /// A concurrent writer changed the row first; the caller may retry.
    pub const CONCURRENT_MODIFICATION: &str = "CONCURRENT_MODIFICATION";
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;

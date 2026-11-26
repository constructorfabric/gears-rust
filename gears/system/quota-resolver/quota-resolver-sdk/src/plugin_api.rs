//! Plugin contract implemented by a quota backend, in process only.

use async_trait::async_trait;
use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformIdentity;

use crate::models::{
    LeaseOutcome, PlatformCommitLeaseRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest,
    PluginAcquireLeaseRequest, PluginDebitRequest, PluginQuotaStateRequest, QuotaDecision,
    QuotaState,
};

/// The backend behind the resolver. Registered as a scoped `ClientHub` entry
/// keyed by its GTS instance id and discovered through
/// [`QuotaResolverPluginSpecV1`](crate::gts::QuotaResolverPluginSpecV1).
///
/// The resolver passes the fixed tenant and quota subjects; errors reach the
/// resolver's caller unchanged. A plugin that refuses a request for a subject
/// answers `InvalidArgument` with a field violation that names the subject
/// field as the request has it: a field that starts with `subjects` for a
/// debit or a lease acquisition, with `subject` for a quota state read. After
/// a debit or a lease acquisition the resolver takes this as a sign of a wrong
/// mapping of the principal. The plugin runs no policy check. `caller` is the
/// platform identity of the resolver's caller, for the record only, and `None`
/// for a caller in the resolver's process.
#[async_trait]
pub trait QuotaResolverPluginClient: Send + Sync {
    /// Charge the metric items against the subjects.
    async fn debit(&self, request: PluginDebitRequest) -> Result<QuotaDecision, CanonicalError>;

    /// Hold capacity for a bounded time.
    async fn acquire_lease(
        &self,
        request: PluginAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError>;

    /// Settle a lease for the amounts actually used.
    async fn commit_lease(
        &self,
        request: PlatformCommitLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError>;

    /// Release a lease without charging.
    async fn release_lease(
        &self,
        request: PlatformReleaseLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError>;

    /// Give back amounts of an earlier charge.
    async fn return_amount(
        &self,
        request: PlatformReturnRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError>;

    /// Read the state of the quotas that apply to one subject.
    async fn quota_state(
        &self,
        request: PluginQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError>;
}

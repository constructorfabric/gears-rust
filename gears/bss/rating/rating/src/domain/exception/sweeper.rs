//! The exception sweeper (feature 01 §3.3 step 2): every 15 minutes, as the Cluster leader
//! `exception-sweep`, it re-enqueues the subjects of retryable open exceptions through the
//! retrier their owning feature registered. Re-enqueueing is idempotent, so a second sweeper
//! during a leadership handover only repeats work.
//!
//! A sweep walks keyset pages, tenants in `tenant_id` order and each tenant's exceptions in
//! `exception_id` order, until it has visited every open exception a retrier handles. Exceptions
//! whose retries keep failing therefore never hold others back, however many there are.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_db::Db;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{ExceptionRecord, ExceptionRepo, ExceptionRetrier, SubjectKind, TenantDiscovery};
use crate::domain::error::DomainError;
use crate::domain::ports::{SweepReport, TenantGrant};

/// The page size of the sweeper's reads: tenants per discovery page and exceptions per page of one
/// tenant. Each page is read in a transaction of its own.
pub const SWEEP_BATCH: u64 = 500;

/// How long one retry may take. A retrier stalled on the database or a downstream call fails that
/// exception for this sweep, as any failed retry does, instead of holding every later exception
/// and every later sweep of the term behind it.
pub const RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Re-enqueues retryable subjects tenant by tenant.
#[domain_model]
#[derive(Clone)]
pub struct ExceptionSweeper {
    retriers: Vec<Arc<dyn ExceptionRetrier>>,
    grant: Arc<dyn TenantGrant>,
    repo: Arc<dyn ExceptionRepo>,
    retry_budget: Duration,
}

impl ExceptionSweeper {
    /// A sweeper over the retriers the built features registered.
    #[must_use]
    pub fn new(
        retriers: Vec<Arc<dyn ExceptionRetrier>>,
        grant: Arc<dyn TenantGrant>,
        repo: Arc<dyn ExceptionRepo>,
    ) -> Self {
        Self {
            retriers,
            grant,
            repo,
            retry_budget: RETRY_BUDGET,
        }
    }

    /// The sweeper with each retry bounded by `budget` instead of [`RETRY_BUDGET`].
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn with_retry_budget(mut self, budget: Duration) -> Self {
        self.retry_budget = budget;
        self
    }

    /// One sweep over every open exception a retrier handles. With no retrier registered there
    /// is nothing to re-enqueue and nothing is read. Each tenant's exceptions are read and retried
    /// under the scope of Rating's service grant for that tenant; a tenant outside the grant is
    /// skipped. A cancelled sweep stops between two exceptions.
    ///
    /// Cancel-safe: every page is read in a transaction of its own and each retry commits on its
    /// own, so dropping the sweep mid-page rolls back at most the page read in flight; the
    /// exceptions not reached yet stay open for the next sweep.
    ///
    /// # Errors
    ///
    /// A storage failure of a page read, or [`DomainError::AuthzUnavailable`] when the PDP cannot
    /// be evaluated (a tenant the PDP refuses is skipped). Failures of single retries are counted
    /// in the report and logged.
    pub async fn sweep_once(
        &self,
        db: &Db,
        stop: &CancellationToken,
    ) -> Result<SweepReport, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-sweep
        let mut report = SweepReport::default();
        if self.retriers.is_empty() {
            return Ok(report);
        }
        let mut kinds: Vec<SubjectKind> = self.retriers.iter().map(|r| r.subject_kind()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let mut after = None;
        loop {
            let tenants = {
                let (repo, kinds) = (Arc::clone(&self.repo), kinds.clone());
                db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let discovery = TenantDiscovery::for_sweeper();
                        repo.open_tenants(tx, &discovery, &kinds, after, SWEEP_BATCH)
                            .await
                    })
                })
                .await?
            };
            for &tenant in &tenants {
                if stop.is_cancelled() {
                    return Ok(report);
                }
                self.sweep_tenant(db, tenant, &kinds, stop, &mut report)
                    .await?;
            }
            if !is_full(tenants.len()) {
                return Ok(report);
            }
            after = tenants.last().copied();
        }
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-exceptions:p1:inst-ex-sweep
    }

    /// Retries every open exception of `tenant`, page by page, under its service-grant scope.
    async fn sweep_tenant(
        &self,
        db: &Db,
        tenant: Uuid,
        kinds: &[SubjectKind],
        stop: &CancellationToken,
        report: &mut SweepReport,
    ) -> Result<(), DomainError> {
        let scope = match self.grant.scope_for(tenant).await {
            Ok(scope) => scope,
            Err(err @ DomainError::OutsideGrant { .. }) => {
                tracing::warn!(
                    target: "bss_rating.exception_sweep",
                    tenant_id = %tenant,
                    error = %err,
                    "tenant skipped: no service grant"
                );
                return Ok(());
            }
            // A PDP outage is not a missing grant: the sweep ends, and the next one starts over.
            Err(err) => return Err(err),
        };
        report.tenants += 1;
        let mut after = None;
        loop {
            let page = {
                let (repo, kinds, scope) = (Arc::clone(&self.repo), kinds.to_vec(), scope.clone());
                db.transaction_ref_mapped(move |tx| {
                    Box::pin(
                        async move { repo.list_open(tx, &scope, &kinds, after, SWEEP_BATCH).await },
                    )
                })
                .await?
            };
            let full = is_full(page.len());
            after = page.last().map(|exception| exception.exception_id);
            self.retry_all(db, &scope, page, stop, report).await;
            if !full || stop.is_cancelled() {
                return Ok(());
            }
        }
    }

    /// Hands each exception a retrier handles back to it, under the tenant's `scope`. Each retry
    /// runs within [`RETRY_BUDGET`], and the end of the term drops the retry in flight, so neither a
    /// stalled retrier nor a stop waits on one.
    async fn retry_all(
        &self,
        db: &Db,
        scope: &AccessScope,
        open: Vec<ExceptionRecord>,
        stop: &CancellationToken,
        report: &mut SweepReport,
    ) {
        for exception in open {
            if stop.is_cancelled() {
                break;
            }
            let Some(retrier) = self.retriers.iter().find(|r| {
                r.subject_kind() == exception.key.subject_kind()
                    && r.retries(exception.key.reason_code())
            }) else {
                continue;
            };
            let outcome = tokio::select! {
                // The term ended: the retry in flight is dropped and its transaction rolls back.
                () = stop.cancelled() => break,
                outcome = tokio::time::timeout(self.retry_budget, retrier.retry(db, scope, &exception)) => {
                    outcome
                }
            };
            let failure = match outcome {
                Ok(Ok(())) => {
                    report.retried += 1;
                    continue;
                }
                Ok(Err(err)) => err.to_string(),
                Err(_elapsed) => format!(
                    "the retry took longer than {} s; dropped",
                    self.retry_budget.as_secs_f64()
                ),
            };
            report.failed += 1;
            tracing::warn!(
                target: "bss_rating.exception_sweep",
                tenant_id = %exception.key.tenant_id(),
                exception_id = %exception.exception_id,
                subject_kind = %exception.key.subject_kind(),
                reason_code = %exception.key.reason_code(),
                error = %failure,
                "retry failed; the next sweep tries again"
            );
        }
    }
}

/// Whether a page came back full, so another may follow.
fn is_full(len: usize) -> bool {
    u64::try_from(len).is_ok_and(|len| len >= SWEEP_BATCH)
}

#[cfg(test)]
#[path = "sweeper_tests.rs"]
mod tests;

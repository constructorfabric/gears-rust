//! One retry budget for a whole ledger transaction, including BEGIN and COMMIT.

use super::error_transport::{decode_post_error, decode_sentinel};
use crate::domain::{error::DomainError, model::RepoError};
use sea_orm::{DbBackend, DbErr, RuntimeErr};
use std::{future::Future, pin::Pin, time::Duration};
use toolkit_db::{
    DbError,
    secure::{Db, DbTx, ScopeError, TxConfig},
};
use uuid::Uuid;

/// Maximum transaction attempts, including the initial attempt.
pub(crate) const MAX_ATTEMPTS: usize = 3;

/// Infrastructure causes stay in infrastructure; domain models carry only conflicts.
#[derive(Debug)]
pub(crate) enum AttemptError {
    /// A lost race (a version CAS or a classified contention); the whole attempt retries.
    Conflict,
    /// A business rejection; returned to the caller as is, never retried.
    Business(DomainError),
    /// An infrastructure failure; retried only when it is driver contention.
    Database(DbError),
}

impl From<DomainError> for AttemptError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::ConcurrentModification(_) => Self::Conflict,
            other => Self::Business(other),
        }
    }
}

impl From<DbError> for AttemptError {
    fn from(error: DbError) -> Self {
        match decode_sentinel(&error) {
            Some(business) => business.into(),
            None => Self::Database(error),
        }
    }
}

/// Classify actual driver failures, never application-composed diagnostic text.
pub(crate) fn is_driver_contention(backend: DbBackend, error: &DbErr) -> bool {
    matches!(
        error,
        DbErr::Exec(RuntimeErr::SqlxError(_)) | DbErr::Query(RuntimeErr::SqlxError(_))
    ) && toolkit_db::contention::is_retryable_contention(backend, error)
}

/// Preserve contention before a repository serializes its infrastructure diagnostic.
pub(crate) fn scope_to_repo(error: ScopeError, backend: DbBackend) -> RepoError {
    if let ScopeError::Db(ref db_error) = error
        && is_driver_contention(backend, db_error)
    {
        return RepoError::Conflict("database contention".to_owned());
    }
    RepoError::Db(error.to_string())
}

/// Only use for a missing-row insert whose sole unique key is the intended grain.
pub(crate) fn insert_to_repo(error: ScopeError, backend: DbBackend) -> RepoError {
    if let ScopeError::Db(ref db_error) = error
        && matches!(
            db_error,
            DbErr::Exec(RuntimeErr::SqlxError(_)) | DbErr::Query(RuntimeErr::SqlxError(_))
        )
        && error.is_unique_violation()
    {
        return RepoError::Conflict("concurrent grain insertion".to_owned());
    }
    scope_to_repo(error, backend)
}

/// [`retry_transaction_for`] without an operation label, for test bodies.
#[cfg(test)]
pub(crate) async fn retry_transaction<F, T>(db: &Db, body: F) -> Result<T, DomainError>
where
    F: for<'a> FnMut(
            &'a DbTx<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<T, AttemptError>> + Send + 'a>>
        + Send,
    T: Send + 'static,
{
    retry_transaction_for(db, RetryOp::new("ledger transaction", None), body).await
}

/// The identity of a retried ledger transaction in its logs: the operation
/// (for example `"invoice post"`) and the tenant when known.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryOp {
    operation: &'static str,
    tenant: Option<Uuid>,
}

impl RetryOp {
    /// Name a retried transaction.
    #[must_use]
    pub(crate) const fn new(operation: &'static str, tenant: Option<Uuid>) -> Self {
        Self { operation, tenant }
    }
}

/// Execute a fresh transactional body at most three times.
///
/// Builders must perform all state-dependent reads and calculations in `body`,
/// and invoke the single-attempt posting body there. Do not call a retrying
/// public posting method from this closure. External effects belong after success.
/// Begin, body and commit failures consume this same budget; body errors roll back.
/// Both the per-attempt retry warning and the exhausted-budget error carry the
/// operation and tenant `op` names, so a conflict storm can be traced to its
/// flow and tenant.
pub(crate) async fn retry_transaction_for<F, T>(
    db: &Db,
    op: RetryOp,
    mut body: F,
) -> Result<T, DomainError>
where
    F: for<'a> FnMut(
            &'a DbTx<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<T, AttemptError>> + Send + 'a>>
        + Send,
    T: Send + 'static,
{
    let RetryOp { operation, tenant } = op;
    let mut last_kind = "none";
    for attempt in 0..MAX_ATTEMPTS {
        let result = db
            .transaction_ref_mapped_with_config(TxConfig::serializable(), |txn| body(txn))
            .await;
        let error = match result {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let retry = match &error {
            AttemptError::Conflict => {
                last_kind = "version conflict";
                true
            }
            AttemptError::Database(DbError::Sea(error)) => {
                last_kind = "database contention";
                is_driver_contention(db.backend(), error)
            }
            _ => false,
        };
        if !retry {
            return Err(match error {
                AttemptError::Business(error) => error,
                AttemptError::Database(error) => decode_post_error(&error),
                AttemptError::Conflict => {
                    DomainError::ConcurrentModification("concurrent modification".to_owned())
                }
            });
        }
        if attempt + 1 < MAX_ATTEMPTS {
            let delay = retry_delay(attempt);
            // Kinds only: driver error text may carry row data and never reaches a log.
            tracing::warn!(
                target: "bss-ledger",
                operation,
                tenant_id = tenant.map(tracing::field::display),
                attempt = attempt + 1,
                max_attempts = MAX_ATTEMPTS,
                kind = last_kind,
                delay = ?delay,
                "bss-ledger: retrying a ledger transaction after a conflict"
            );
            tokio::time::sleep(delay).await;
        }
    }
    tracing::error!(
        target: "bss-ledger",
        operation,
        tenant_id = tenant.map(tracing::field::display),
        max_attempts = MAX_ATTEMPTS,
        kind = last_kind,
        "bss-ledger: a ledger transaction conflicted on every attempt"
    );
    Err(DomainError::ConcurrentModification(format!(
        "{operation} conflicted after {MAX_ATTEMPTS} attempts ({last_kind})"
    )))
}

/// Full-jitter delay before the next attempt: uniformly random up to 10 ms, then
/// 20 ms, so two transactions that collided on one cache row do not restart in
/// lockstep and collide again.
fn retry_delay(attempt: usize) -> Duration {
    let ceiling = Duration::from_millis(10_u64 << attempt.min(4));
    tokio_retry::strategy::jitter(ceiling)
}

/// Preserve a raw database failure before converting it to a domain repository error.
pub(crate) fn db_to_repo(error: DbError, backend: DbBackend) -> RepoError {
    if let DbError::Sea(ref db_error) = error
        && is_driver_contention(backend, db_error)
    {
        return RepoError::Conflict("database contention".to_owned());
    }
    RepoError::Db(error.to_string())
}

impl From<RepoError> for AttemptError {
    fn from(error: RepoError) -> Self {
        super::error_transport::repo_to_domain(error).into()
    }
}

impl From<crate::domain::money::ScaleError> for AttemptError {
    fn from(error: crate::domain::money::ScaleError) -> Self {
        super::error_transport::scale_error_to_domain(error).into()
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;

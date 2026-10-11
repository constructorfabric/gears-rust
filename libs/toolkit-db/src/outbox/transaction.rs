use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use crate::DbError;
use crate::secure::{Db, DbTx};

use super::Wake;
use super::admission::Arrivals;

/// Run `f` in a transaction and fire its outbox wake - but only if the
/// transaction commits.
///
/// The closure enqueues within `tx` and returns its own result together with
/// the [`Wake`] the enqueue produced. On a committed transaction the wake is
/// fired here, waking the sequencers against durable rows. If the closure
/// returns `Err`, the transaction rolls back and the wake is dropped unfired;
/// no sequencer is woken, because the rows never became durable.
///
/// This puts the commit-then-fire contract in one place so call sites never
/// hold a wake across the commit boundary themselves - the pattern that made
/// it easy to fire before the commit landed (the race this whole design
/// closes) or to forget the fire entirely.
///
/// Enqueue **last** in the closure: an error after the enqueue would drop the
/// wake on the rollback path, which is correct but trips the [`Wake`] drop
/// warning. Producing the wake as the final step keeps the error paths
/// wake-free.
///
/// A rolled-back or failed commit also hands back what the enqueues counted
/// against a bounded queue, so the refused work stops occupying its allowance
/// at once. That holds for the wake the closure returns; one it dropped on its
/// own error path keeps its count until the counter audit clears it - another
/// reason to enqueue last.
///
/// # Errors
///
/// Returns `E` if the transaction cannot be started, the closure returns an
/// error, or the commit fails.
pub async fn in_transaction<F, T, E>(db: &Db, f: F) -> Result<T, E>
where
    E: From<DbError> + Send + 'static,
    F: for<'a> FnOnce(
            &'a DbTx<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(T, Wake), E>> + Send + 'a>>
        + Send,
    T: Send + 'static,
{
    // What the enqueues counted against a bounded queue, held outside the
    // transaction so a failed commit - which drops the wake with it - can
    // still hand it back.
    let counted = Arc::new(Mutex::new(Arrivals::default()));
    let stash = Arc::clone(&counted);
    let result = db
        .transaction_ref_mapped(move |tx| {
            let body = f(tx);
            Box::pin(async move {
                let (value, mut wake) = body.await?;
                *stash.lock().unwrap_or_else(PoisonError::into_inner) = wake.take_arrivals();
                Ok((value, wake))
            })
        })
        .await;
    match result {
        Ok((value, wake)) => {
            wake.fire();
            Ok(value)
        }
        Err(e) => {
            std::mem::take(&mut *counted.lock().unwrap_or_else(PoisonError::into_inner))
                .roll_back();
            Err(e)
        }
    }
}

use std::future::Future;
use std::pin::Pin;

use crate::DbError;
use crate::secure::{Db, DbTx};

use super::Wake;

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
    let (value, wake) = db.transaction_ref_mapped(f).await?;
    wake.fire();
    Ok(value)
}

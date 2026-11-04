//! The producer-chain transaction behind `Storage::check_and_enqueue`: read
//! the `(producer, topic)` head, let the domain decide, enqueue the accepted
//! event, then move the head with a compare-and-set.
//!
//! The head write is the transaction's last statement and is conditional on
//! the head it read: an existing head moves only while `last_sequence` still
//! equals the head read, and a first head is a plain `INSERT` that loses to a
//! concurrent first insert on the primary key. Either way a writer that got
//! there first leaves this attempt with nothing written; the attempt rolls
//! back (taking its outbox row with it) and the whole transaction runs again,
//! re-reading the head and answering duplicate, `412` or accept against it.
//! On `PostgreSQL` (read committed) and `MySQL` (an `UPDATE` reads the latest
//! committed row) a concurrent second `UPDATE` waits for the first to commit,
//! then re-checks its condition and changes nothing. `SQLite` has one writer
//! at a time, so there the transaction takes the write lock before it reads
//! (see [`take_write_lock`]) and the compare-and-set always holds. Database
//! contention (deadlock, serialization failure, `SQLite` busy) is retried by
//! the same loop.
//!
//! The first head is not an `INSERT ... ON CONFLICT DO NOTHING`: on `MySQL`
//! sea-query renders that as `ON DUPLICATE KEY UPDATE <pk> = <pk>`, and with
//! the `CLIENT_FOUND_ROWS` flag sqlx connects with, a row left at its current
//! values counts as one affected row - indistinguishable from an insert.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, DbBackend, DbErr, EntityTrait, QueryFilter, Set, SqlErr};
use toolkit_db::outbox::{Outbox, OutboxError, Wake};
use toolkit_db::secure::{ScopeError, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_db::{Db, DbError, DbTx};
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::idempotency::{ProducerChainCheck, ProducerIdempotencyOutcome, PublishEnqueue};
use crate::infra::storage::entity::{producer, producer_sequence};

/// Attempts one publish gets at the chain transaction, the first included.
/// A lost compare-and-set is answered by the retry (another writer committed,
/// so the re-read head decides), so more than two attempts are only needed
/// under sustained contention on one producer.
const ATTEMPTS: u32 = 5;

/// Why one attempt at the chain transaction failed.
#[derive(Debug, thiserror::Error)]
enum AttemptError {
    #[error("another writer moved the producer head between this attempt's read and its write")]
    HeadMoved,
    #[error(transparent)]
    Db(DbErr),
    #[error(transparent)]
    Scope(ScopeError),
    #[error(transparent)]
    Other(DbError),
}

impl From<DbError> for AttemptError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(err) => Self::Db(err),
            other => Self::Other(other),
        }
    }
}

impl From<ScopeError> for AttemptError {
    fn from(err: ScopeError) -> Self {
        match err {
            ScopeError::Db(err) => Self::Db(err),
            other => Self::Scope(other),
        }
    }
}

impl From<OutboxError> for AttemptError {
    fn from(err: OutboxError) -> Self {
        match err {
            OutboxError::Database(err) => Self::Db(err),
            other => Self::Other(DbError::Other(anyhow::anyhow!("outbox enqueue: {other}"))),
        }
    }
}

/// Everything one attempt needs, owned, because each attempt is a fresh
/// transaction closure.
#[derive(Clone)]
struct Attempt {
    backend: DbBackend,
    request: PublishEnqueue,
    outbox: Arc<Outbox>,
    #[cfg(any(test, feature = "test-utils"))]
    head_read_pause: crate::test_support::PausePoint,
}

/// Runs the chain transaction until it commits, retrying a lost
/// compare-and-set and database contention up to [`ATTEMPTS`] times. Any
/// failure that still ends the publish is logged with its cause and answered
/// as an unavailable storage path.
pub(super) async fn check_and_enqueue(
    db: &Db,
    outbox: Arc<Outbox>,
    request: PublishEnqueue,
    #[cfg(any(test, feature = "test-utils"))] head_read_pause: crate::test_support::PausePoint,
) -> Result<(ProducerIdempotencyOutcome, Wake), DomainError> {
    let backend = db.backend();
    let attempt = Attempt {
        backend,
        request,
        outbox,
        #[cfg(any(test, feature = "test-utils"))]
        head_read_pause,
    };
    let mut tries = 1;
    loop {
        let this = attempt.clone();
        let err = match db
            .transaction_ref_mapped(move |tx| Box::pin(this.run(tx)))
            .await
        {
            Ok(done) => return Ok(done),
            Err(err) => err,
        };
        let retryable = match &err {
            AttemptError::HeadMoved => true,
            AttemptError::Db(db_err) => {
                toolkit_db::contention::is_retryable_contention(backend, db_err)
            }
            AttemptError::Scope(_) | AttemptError::Other(_) => false,
        };
        if retryable && tries < ATTEMPTS {
            tokio::time::sleep(retry_delay(tries)).await;
            tries += 1;
            continue;
        }
        if retryable {
            tracing::warn!(
                attempts = tries,
                error = %err,
                "publish refused: the producer-chain transaction kept losing to concurrent writers"
            );
        } else {
            tracing::warn!(error = %err, "publish refused: the producer-chain transaction failed");
        }
        return Err(DomainError::StorageUnavailable {
            reason: "database unavailable".to_owned(),
            source: Some(Box::new(err)),
        });
    }
}

/// Full jitter over a window that doubles from 2 ms, so writers that just
/// collided do not retry in lockstep.
fn retry_delay(failed_tries: u32) -> Duration {
    let window_us = 2_000_u64 << failed_tries.saturating_sub(1).min(4);
    Duration::from_micros(rand::random_range(0..=window_us))
}

impl Attempt {
    async fn run(self, tx: &DbTx<'_>) -> Result<(ProducerIdempotencyOutcome, Wake), AttemptError> {
        let PublishEnqueue {
            chain,
            ingest_partition,
            payload,
            payload_type,
        } = self.request;

        let (outcome, head) = match &chain {
            None => (ProducerIdempotencyOutcome::Accept, None),
            Some(c) => {
                let sqlite = self.backend == DbBackend::Sqlite;
                // The test pause sits at the last point where another writer
                // can still commit before this attempt writes: before the
                // write lock on `SQLite`, after the head read elsewhere.
                #[cfg(any(test, feature = "test-utils"))]
                if sqlite {
                    self.head_read_pause.pass().await;
                }
                if sqlite {
                    take_write_lock(tx, c).await?;
                }
                let head = producer_sequence::Entity::find()
                    .filter(producer_sequence::Column::ProducerId.eq(c.producer_id))
                    .filter(producer_sequence::Column::Topic.eq(c.topic.as_ref()))
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .one(tx)
                    .await?
                    .map(|row| row.last_sequence);
                #[cfg(any(test, feature = "test-utils"))]
                if !sqlite {
                    self.head_read_pause.pass().await;
                }
                // The domain owns the sequencing decision; infra only reads
                // the head and performs the write it dictates.
                (c.decide(head), head)
            }
        };
        if !matches!(outcome, ProducerIdempotencyOutcome::Accept) {
            return Ok((outcome, Wake::empty()));
        }

        let record = toolkit_db::outbox::Record::to(
            crate::domain::outbox::INGEST_QUEUE_NAME,
            ingest_partition,
        )
        .payload(payload, &payload_type)
        .build()?;
        let wake = self.outbox.enqueue(tx, record).await?;

        if let Some(c) = &chain {
            match head {
                Some(read) => advance_head(tx, c, read).await?,
                None => seed_head(tx, c).await?,
            }
        }
        Ok((outcome, wake))
    }
}

/// Makes `SQLite` take its database write lock before the head read. `SQLite`
/// has one writer at a time, and a transaction that read before another
/// connection committed is refused the upgrade to writer at once
/// (`SQLITE_BUSY`), without waiting - under a burst of first publishes that
/// exhausts any retry budget. Writing first makes the transaction wait its
/// turn (`busy_timeout`) instead, and read a head nobody else can move. The
/// statement changes nothing: it rewrites the head, if any, to itself.
async fn take_write_lock(tx: &DbTx<'_>, chain: &ProducerChainCheck) -> Result<(), AttemptError> {
    producer_sequence::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(producer_sequence::Column::ProducerId.eq(chain.producer_id))
                .add(producer_sequence::Column::Topic.eq(chain.topic.as_ref())),
        )
        .col_expr(
            producer_sequence::Column::LastSequence,
            Expr::col(producer_sequence::Column::LastSequence),
        )
        .exec(tx)
        .await?;
    Ok(())
}

/// Moves the head from `read` to the accepted sequence, only if it is still
/// `read`.
async fn advance_head(
    tx: &DbTx<'_>,
    chain: &ProducerChainCheck,
    read: i64,
) -> Result<(), AttemptError> {
    let moved = producer_sequence::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(producer_sequence::Column::ProducerId.eq(chain.producer_id))
                .add(producer_sequence::Column::Topic.eq(chain.topic.as_ref()))
                .add(producer_sequence::Column::LastSequence.eq(read)),
        )
        .col_expr(
            producer_sequence::Column::LastSequence,
            Expr::value(chain.sequence),
        )
        .col_expr(
            producer_sequence::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .exec(tx)
        .await?;
    if moved.rows_affected == 1 {
        Ok(())
    } else {
        Err(AttemptError::HeadMoved)
    }
}

/// Writes the first head for the producer on this topic, unless a concurrent
/// first publish wrote it first.
async fn seed_head(tx: &DbTx<'_>, chain: &ProducerChainCheck) -> Result<(), AttemptError> {
    // The tenant is denormalized from the producer row, which also guards the
    // foreign key: a producer deleted after the ingest path resolved it is
    // reported by name rather than as a constraint failure.
    let producer_row = producer::Entity::find_by_id(chain.producer_id)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(tx)
        .await?
        .ok_or_else(|| {
            AttemptError::Other(DbError::Other(anyhow::anyhow!(
                "producer '{}' is not registered",
                chain.producer_id
            )))
        })?;
    let am = producer_sequence::ActiveModel {
        producer_id: Set(chain.producer_id),
        topic: Set(chain.topic.as_ref().to_owned()),
        tenant_id: Set(producer_row.tenant_id),
        last_sequence: Set(chain.sequence),
        updated_at: Set(Utc::now()),
    };
    match secure_insert::<producer_sequence::Entity>(
        am,
        &AccessScope::for_tenant(producer_row.tenant_id),
        tx,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(ScopeError::Db(err))
            if matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) =>
        {
            Err(AttemptError::HeadMoved)
        }
        Err(err) => Err(err.into()),
    }
}

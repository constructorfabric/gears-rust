//! The producer-chain transaction behind `Storage::check_and_enqueue` and
//! `Storage::check_and_enqueue_batch`: split the requests into runs, one per
//! `(producer, topic)` (feature 0001 §5.2); read each run's head and let the
//! domain decide the run against it (R4 chained, R5 monotonic); enqueue every
//! event that is not a duplicate, in request order; then move each advancing
//! run's head to its last `sequence`. A single publish is a run of one.
//!
//! A batch is one transaction. The first run that breaks its chain ends the
//! transaction with an error, so it rolls back and the batch leaves nothing
//! enqueued and no head moved; a duplicate enqueues nothing and the batch goes
//! on.
//!
//! On `PostgreSQL` and `MySQL` each head is read with a locking read
//! (`SELECT ... FOR UPDATE`), so no other writer moves it before this
//! transaction commits, and the `last_sequence` a `412` reports is the
//! committed head. An absent head locks nothing, so a first head can still
//! race: it is a plain `INSERT` that loses to a concurrent first insert on the
//! primary key. An existing head moves with a compare-and-set against the
//! head read, which the lock makes hold. Either way a writer that got there
//! first leaves this attempt with nothing written; the attempt rolls back
//! (taking its outbox rows with it) and the whole transaction runs again,
//! re-reading the heads and answering duplicate, `412` or accept against
//! them. `SQLite` has one writer at a time and renders no row lock, so there
//! the transaction takes the write lock before it reads (see
//! [`take_write_lock`]) and the compare-and-set always holds. Database
//! contention (deadlock, serialization failure, `SQLite` busy) is retried by
//! the same loop.
//!
//! On `MySQL` the transaction runs at read committed, not the server's default
//! repeatable read. Under repeatable read a locking read that finds no row
//! takes a gap lock, and gap locks do not conflict with each other but do
//! block an insert into the gap: concurrent first publishes whose keys fall in
//! one gap all read, then all wait on each other's gap at their `INSERT`, and
//! the deadlock detector aborts all but one per round - more rounds than the
//! retry budget once a burst outgrows it. Read committed takes record locks
//! only, so a first-publish race ends as on `PostgreSQL`: the later `INSERT`
//! fails on the primary key once the earlier one commits, and the attempt
//! runs again. Nothing here needs a repeatable snapshot: every head read is a
//! locking read, which reads the latest committed row at any level. The
//! ingest outbox registration already runs its writes at read committed, so
//! this asks nothing new of the binary log format. `PostgreSQL` keeps its
//! default read committed, which takes no gap locks, and `SQLite` is
//! serializable whatever it is told.
//!
//! A batch locks heads in request order (the order of each run's first
//! event), not in a sorted one, because the `412` names the first violating
//! run in request order (feature 0001 §5.2 "Outcome"): reading in that order
//! decides each run as its head is read and stops at the first violation,
//! where a sorted order would have to read every head before deciding any.
//! Two concurrent batches touching the same heads in opposite orders can
//! deadlock on `PostgreSQL` and `MySQL`; the database detects it and aborts
//! one, which is contention and is retried like any other, from a fresh
//! transaction. Producers do not normally create that case (one producer id
//! is one writer), so the retry covers it.
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
use toolkit_db::outbox::{Outbox, OutboxError, Records, RecordsBuilder, Wake};
use toolkit_db::secure::{
    ScopeError, SecureEntityExt, SecureUpdateExt, TxConfig, TxIsolationLevel, secure_insert,
};
use toolkit_db::{Db, DbError, DbTx};
use toolkit_gts::GtsInstanceId;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::idempotency::{
    ChainHead, ChainRun, ProducerIdempotencyOutcome, PublishEnqueue, RunDecision,
};
use crate::infra::storage::entity::{producer, producer_sequence};

/// Attempts one publish or batch gets at the chain transaction, the first
/// included. A first head lost to a concurrent first insert is answered by
/// the retry (that writer committed, so the re-read head decides); further
/// attempts cover database contention - opposite-order batches deadlocking,
/// `SQLite` busy - under sustained load on the same heads.
const ATTEMPTS: u32 = 5;

/// How the chain transaction ended without a database failure.
pub(super) enum Enqueued {
    /// Committed: one outcome per request, in request order, each `Accept` or
    /// `DuplicateIgnore`, with the wake of every row enqueued, to fire now
    /// that they are durable.
    Committed {
        outcomes: Vec<ProducerIdempotencyOutcome>,
        wake: Wake,
    },
    /// A run broke its chain (feature 0001 §5.2 R4); the transaction rolled
    /// back, so nothing is enqueued and no head moved. `index` is the request
    /// position of the run's first event, `last_sequence` the committed head.
    Violation {
        index: usize,
        producer_id: Uuid,
        topic: GtsInstanceId,
        last_sequence: i64,
    },
}

/// Why one attempt at the chain transaction failed.
#[derive(Debug, thiserror::Error)]
enum AttemptError {
    #[error("another writer wrote the producer head after this attempt read it")]
    HeadMoved,
    /// Not a failure of the attempt: the error that makes the transaction
    /// roll back what earlier requests of the batch wrote.
    #[error("the run at event {index} does not link to the producer head at {last_sequence}")]
    Violation {
        index: usize,
        producer_id: Uuid,
        topic: GtsInstanceId,
        last_sequence: i64,
    },
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
    requests: Vec<PublishEnqueue>,
    outbox: Arc<Outbox>,
    #[cfg(any(test, feature = "test-utils"))]
    pauses: ChainPauses,
}

/// The two points where a test holds a chained publish inside its
/// transaction: before its head reads (and before the write lock on
/// `SQLite`), and after them, before anything is written.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone)]
pub(super) struct ChainPauses {
    pub(super) before_head_read: crate::test_support::PausePoint,
    pub(super) after_head_read: crate::test_support::PausePoint,
}

/// Runs the chain transaction over `requests` until it commits or a request
/// breaks its chain, retrying a lost compare-and-set and database contention
/// up to [`ATTEMPTS`] times. Any failure that still ends the publish is logged
/// with its cause and answered as an unavailable storage path.
pub(super) async fn check_and_enqueue(
    db: &Db,
    outbox: Arc<Outbox>,
    requests: Vec<PublishEnqueue>,
    #[cfg(any(test, feature = "test-utils"))] pauses: ChainPauses,
) -> Result<Enqueued, DomainError> {
    let backend = db.backend();
    let attempt = Attempt {
        backend,
        requests,
        outbox,
        #[cfg(any(test, feature = "test-utils"))]
        pauses,
    };
    let mut tries = 1;
    loop {
        let err = match attempt.clone().transact(db).await {
            Ok((outcomes, wake)) => return Ok(Enqueued::Committed { outcomes, wake }),
            Err(AttemptError::Violation {
                index,
                producer_id,
                topic,
                last_sequence,
            }) => {
                return Ok(Enqueued::Violation {
                    index,
                    producer_id,
                    topic,
                    last_sequence,
                });
            }
            Err(err) => err,
        };
        let retryable = match &err {
            AttemptError::HeadMoved => true,
            AttemptError::Db(db_err) => {
                toolkit_db::contention::is_retryable_contention(backend, db_err)
            }
            AttemptError::Violation { .. } | AttemptError::Scope(_) | AttemptError::Other(_) => {
                false
            }
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
    /// One attempt as its own transaction, at read committed on `MySQL` (see
    /// the module doc) and at the engine's default elsewhere.
    async fn transact(
        self,
        db: &Db,
    ) -> Result<(Vec<ProducerIdempotencyOutcome>, Wake), AttemptError> {
        if self.backend == DbBackend::MySql {
            let read_committed = TxConfig {
                isolation: Some(TxIsolationLevel::ReadCommitted),
                ..TxConfig::default()
            };
            db.transaction_ref_mapped_with_config(read_committed, move |tx| Box::pin(self.run(tx)))
                .await
        } else {
            db.transaction_ref_mapped(move |tx| Box::pin(self.run(tx)))
                .await
        }
    }

    async fn run(
        self,
        tx: &DbTx<'_>,
    ) -> Result<(Vec<ProducerIdempotencyOutcome>, Wake), AttemptError> {
        let Attempt {
            backend,
            requests,
            outbox,
            #[cfg(any(test, feature = "test-utils"))]
            pauses,
        } = self;
        let runs = ChainRun::split(&requests);
        if let Some(first) = runs.first() {
            // The last point where another writer can still move a head this
            // attempt reads: before the locking reads, and before the write
            // lock on `SQLite`.
            #[cfg(any(test, feature = "test-utils"))]
            pauses.before_head_read.pass().await;
            // `SQLite` takes its write lock once, before the first head read,
            // with the first run's key; every later read then runs under it.
            if backend == DbBackend::Sqlite {
                take_write_lock(tx, first).await?;
            }
        }

        // Feature 0001 §5.2 R4 / R5: each run against its committed head, the
        // heads read in request order. The domain decides; infra reads the
        // heads and performs the writes the decisions dictate.
        let mut duplicate = vec![false; requests.len()];
        let mut head_writes = Vec::with_capacity(runs.len());
        for run in &runs {
            let head = read_head(tx, run).await?;
            let duplicates = match run.decide(head) {
                RunDecision::Advance {
                    duplicates,
                    head: to,
                } => {
                    head_writes.push((run, head, to));
                    duplicates
                }
                RunDecision::Duplicate => run.events.len(),
                RunDecision::SequenceViolation { last_sequence } => {
                    return Err(AttemptError::Violation {
                        index: run.events.first().map_or(0, |event| event.index),
                        producer_id: run.producer_id,
                        topic: run.topic.clone(),
                        last_sequence,
                    });
                }
            };
            for event in run.events.iter().take(duplicates) {
                if let Some(slot) = duplicate.get_mut(event.index) {
                    *slot = true;
                }
            }
        }
        // Every head is read and nothing is written yet: an absent head is
        // still open to a concurrent first insert on `PostgreSQL` and `MySQL`,
        // while on `SQLite` this attempt already holds the write lock.
        #[cfg(any(test, feature = "test-utils"))]
        if !runs.is_empty() {
            pauses.after_head_read.pass().await;
        }

        // Enqueue every non-duplicate row, in request order; a rollback takes
        // its rows with it.
        let (outcomes, wake) = enqueue_admitted(tx, &outbox, requests, &duplicate).await?;

        for (run, read, to) in head_writes {
            let written = match read {
                ChainHead::At(read) => advance_head(tx, run, read, to).await,
                ChainHead::Absent => seed_head(tx, run, to).await,
            };
            if let Err(err) = written {
                wake.discard();
                return Err(err);
            }
        }
        Ok((outcomes, wake))
    }
}

/// The committed head of `run`'s chain. A locking read on `PostgreSQL` and
/// `MySQL`, so no other writer moves the head before this transaction ends;
/// `SQLite` renders no lock clause and is already under its write lock.
async fn read_head(tx: &DbTx<'_>, run: &ChainRun) -> Result<ChainHead, AttemptError> {
    let row = producer_sequence::Entity::find()
        .filter(producer_sequence::Column::ProducerId.eq(run.producer_id))
        .filter(producer_sequence::Column::Topic.eq(run.topic.as_ref()))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .lock_exclusive()
        .one(tx)
        .await?;
    Ok(row.map_or(ChainHead::Absent, |row| ChainHead::At(row.last_sequence)))
}

/// Enqueues every non-duplicate request of one attempt, in request order, and
/// returns the per-request outcomes alongside the combined [`Wake`]. On any
/// error the accumulated wake is discarded, since the transaction rolls back.
///
/// A synchronous publish (every request carries the same trace, set by the
/// ingest service) becomes one traced outbox batch, so the waiting publish is
/// told once every admitted row is stored (feature 0007 §4.1); an all-duplicate
/// batch enqueues nothing - `Records::build` refuses an empty traced batch, and
/// there is nothing to wait on anyway. An asynchronous publish enqueues each row
/// on its own, the byte-for-byte path of every publish before synchronous
/// publish existed.
async fn enqueue_admitted(
    tx: &DbTx<'_>,
    outbox: &Outbox,
    requests: Vec<PublishEnqueue>,
    duplicate: &[bool],
) -> Result<(Vec<ProducerIdempotencyOutcome>, Wake), AttemptError> {
    let trace = requests.iter().find_map(|request| request.trace);
    let count = requests.len();
    let mut outcomes = Vec::with_capacity(count);
    let mut wake = Wake::empty();
    let rows = requests.into_iter().zip(duplicate.iter().copied());

    if let Some(trace) = trace {
        let mut admitted = Vec::with_capacity(count);
        for (request, duplicate) in rows {
            outcomes.push(if duplicate {
                ProducerIdempotencyOutcome::DuplicateIgnore
            } else {
                admitted.push((request.ingest_partition, request.payload));
                ProducerIdempotencyOutcome::Accept
            });
        }
        if !admitted.is_empty() {
            let trace = trace.to_string();
            let mut batch = Records::to(crate::domain::outbox::INGEST_QUEUE_NAME)
                .payload_type(crate::domain::outbox::INGEST_PAYLOAD_TYPE)
                .trace(&trace);
            for (partition, payload) in admitted {
                batch = batch.push(partition, payload);
            }
            match enqueue_traced(tx, outbox, batch).await {
                Ok(enqueued) => wake += enqueued,
                Err(err) => {
                    wake.discard();
                    return Err(err);
                }
            }
        }
    } else {
        for (request, duplicate) in rows {
            if duplicate {
                outcomes.push(ProducerIdempotencyOutcome::DuplicateIgnore);
                continue;
            }
            match enqueue(tx, outbox, request).await {
                Ok(enqueued) => wake += enqueued,
                Err(err) => {
                    wake.discard();
                    return Err(err);
                }
            }
            outcomes.push(ProducerIdempotencyOutcome::Accept);
        }
    }
    Ok((outcomes, wake))
}

/// Validates and enqueues one traced batch of admitted rows.
async fn enqueue_traced(
    tx: &DbTx<'_>,
    outbox: &Outbox,
    batch: RecordsBuilder<'_>,
) -> Result<Wake, AttemptError> {
    let records = batch.build()?;
    Ok(outbox.enqueue_batch(tx, records).await?)
}

/// One request's payload as one ingest outbox row.
async fn enqueue(
    tx: &DbTx<'_>,
    outbox: &Outbox,
    request: PublishEnqueue,
) -> Result<Wake, AttemptError> {
    let record = toolkit_db::outbox::Record::to(
        crate::domain::outbox::INGEST_QUEUE_NAME,
        request.ingest_partition,
    )
    .payload(request.payload, &request.payload_type)
    .build()?;
    Ok(outbox.enqueue(tx, record).await?)
}

/// Makes `SQLite` take its database write lock before the head read. `SQLite`
/// has one writer at a time, and a transaction that read before another
/// connection committed is refused the upgrade to writer at once
/// (`SQLITE_BUSY`), without waiting - under a burst of first publishes that
/// exhausts any retry budget. Writing first makes the transaction wait its
/// turn (`busy_timeout`) instead, and read a head nobody else can move. The
/// statement changes nothing: it rewrites the head, if any, to itself.
async fn take_write_lock(tx: &DbTx<'_>, run: &ChainRun) -> Result<(), AttemptError> {
    producer_sequence::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(producer_sequence::Column::ProducerId.eq(run.producer_id))
                .add(producer_sequence::Column::Topic.eq(run.topic.as_ref())),
        )
        .col_expr(
            producer_sequence::Column::LastSequence,
            Expr::col(producer_sequence::Column::LastSequence),
        )
        .exec(tx)
        .await?;
    Ok(())
}

/// Moves `run`'s head from `read` to `to`, only if it is still `read`.
async fn advance_head(
    tx: &DbTx<'_>,
    run: &ChainRun,
    read: i64,
    to: i64,
) -> Result<(), AttemptError> {
    let moved = producer_sequence::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(producer_sequence::Column::ProducerId.eq(run.producer_id))
                .add(producer_sequence::Column::Topic.eq(run.topic.as_ref()))
                .add(producer_sequence::Column::LastSequence.eq(read)),
        )
        .col_expr(producer_sequence::Column::LastSequence, Expr::value(to))
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

/// Writes `run`'s first head at `to`, unless a concurrent first publish wrote
/// it first.
async fn seed_head(tx: &DbTx<'_>, run: &ChainRun, to: i64) -> Result<(), AttemptError> {
    // The tenant is denormalized from the producer row. A producer deleted
    // after the ingest path resolved it fails the write without a retry: the
    // publish answers `503`, and the logged cause names the producer.
    let producer_row = producer::Entity::find_by_id(run.producer_id)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(tx)
        .await?
        .ok_or_else(|| {
            AttemptError::Other(DbError::Other(anyhow::anyhow!(
                "producer '{}' is not registered",
                run.producer_id
            )))
        })?;
    let am = producer_sequence::ActiveModel {
        producer_id: Set(run.producer_id),
        topic: Set(run.topic.as_ref().to_owned()),
        tenant_id: Set(producer_row.tenant_id),
        last_sequence: Set(to),
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

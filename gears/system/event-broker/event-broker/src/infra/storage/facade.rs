//! `Storage`: the real (gear database + `ClusterCacheV1`) implementation of every
//! `domain::repo` trait. Topics and events have none - topics go through
//! `SpecificationManager` only, events straight to the backend. One struct
//! implements every trait, which is safe because their method names are deliberately distinct
//! (`domain/repo.rs`'s doc comment: identical names across traits in scope on
//! one type are ambiguous at the call site).
//!
//! Namespace-to-engine split: subscription and group-membership state lives
//! only as long as the consumer session, so none of it is durable. Routing
//! markers go to `ClusterCacheV1`; subscriptions themselves are not stored
//! here - they live in `ConsumerGroupCoordinator`. `event_broker_cursor`,
//! `event_broker_consumer_group`, `event_broker_producer` and
//! `event_broker_producer_sequence` go to the gear database (durable; `SQLite`,
//! `PostgreSQL` or `MySQL`/`MariaDB`). `event_broker_cursor` and
//! `event_broker_producer_sequence` rows denormalize `tenant_id` from their
//! owning `event_broker_consumer_group` / `event_broker_producer` row at write
//! time, since neither has an independent tenant of its own.

use crate::domain::model::Sequence;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::{PutRequest, Ttl};
use event_broker_sdk::ConsumerGroupId;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use toolkit_db::DBProvider;
use toolkit_db::outbox::{Outbox, TraceSubscription};
use toolkit_db::secure::{
    SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict, secure_insert,
};
use toolkit_gts::GtsInstanceId;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::idempotency::{IdempotencyGuard, ProducerIdempotencyOutcome, PublishEnqueue};
use crate::domain::ingest::{
    ProducerCursors, ProducerMode, ProducerRecord, ProducerRegistration, ProducerRegistry,
    ProducerTopicCursors, SyncTrace,
};
use crate::domain::model::{ConsumerGroup, ConsumerGroupKind, Cursor};
use crate::domain::producer_reset::ProducerResetScope;
use crate::domain::repo::{ConsumerGroupRepo, CursorRepo, RoutingMarkers};
use crate::domain::specification::SpecificationManager;
use crate::domain::sync_wait::{SyncWait, SyncWaitRegistry};
use crate::infra::storage::entity::{
    consumer_group, cursor, outbox_identity, producer, producer_sequence,
};
use crate::infra::storage::producer_chain::Enqueued;

/// Subscription id -> its consumer group.
fn subscription_marker_key(id: Uuid) -> String {
    format!("subscription-group/{id}")
}

/// Consumer group -> the delivery instance that owns it: the marker's value
/// is the owning instance's id (`docs/DESIGN.md` "Subscription Resolution
/// Cache", Cache 2).
///
/// Keyed by the group's own UUID (an anonymous group's id ends in it), not its
/// id: a GTS id's `.` and `~` are outside the cache's key alphabet.
fn group_marker_key(group: ConsumerGroupId) -> String {
    format!("group-owner/{}", group.as_uuid())
}

fn kind_to_str(kind: ConsumerGroupKind) -> &'static str {
    match kind {
        ConsumerGroupKind::Anonymous => "anonymous",
        ConsumerGroupKind::Named => "named",
    }
}

fn str_to_kind(s: &str) -> Result<ConsumerGroupKind, DomainError> {
    match s {
        "anonymous" => Ok(ConsumerGroupKind::Anonymous),
        "named" => Ok(ConsumerGroupKind::Named),
        other => Err(DomainError::Internal(format!(
            "stored consumer_group.kind '{other}' is neither 'anonymous' nor 'named'"
        ))),
    }
}

fn producer_mode_to_str(mode: ProducerMode) -> &'static str {
    match mode {
        ProducerMode::Chained => "chained",
        ProducerMode::Monotonic => "monotonic",
    }
}

fn str_to_producer_mode(s: &str) -> Result<ProducerMode, DomainError> {
    match s {
        "chained" => Ok(ProducerMode::Chained),
        "monotonic" => Ok(ProducerMode::Monotonic),
        other => Err(DomainError::Internal(format!(
            "stored producer.mode '{other}' is neither 'chained' nor 'monotonic'"
        ))),
    }
}

fn row_to_consumer_group(row: consumer_group::Model) -> Result<ConsumerGroup, DomainError> {
    Ok(ConsumerGroup {
        id: parse_gts_id(&row.id)?,
        kind: str_to_kind(&row.kind)?,
        tenant_id: row.tenant_id,
        owner_principal_id: row.owner_principal_id,
        description: row.description,
        created_at: row.created_at,
    })
}

fn parse_gts_id(raw: &str) -> Result<GtsInstanceId, DomainError> {
    GtsInstanceId::try_new(raw)
        .map_err(|e| DomainError::Internal(format!("stored GTS id '{raw}' is malformed: {e}")))
}

pub struct Storage {
    db: Arc<DBProvider<toolkit_db::DbError>>,
    spec_manager: Arc<dyn SpecificationManager>,
    /// The routing markers' cache, `None` until `EventBrokerModule::serve()`
    /// resolves and wires it via [`Self::set_cache`] (which carries the
    /// ordering rationale). Reads go through [`Self::cache`], never this
    /// field directly, so a request arriving before the wiring gets an
    /// unavailable storage path rather than a panic.
    cache: OnceLock<ClusterCacheV1>,
    /// The ingest outbox pipeline - `None` until `EventBrokerModule::serve()`
    /// starts it (`Outbox::builder(..).start()` needs a running Tokio
    /// runtime and the leased handler's own dependencies, neither available
    /// yet at `Storage::new()` time in `init()`). Set once via
    /// [`Self::set_outbox`]. A publish arriving before then is reported as
    /// an unavailable storage path, never a panic - the listener accepts
    /// traffic as soon as the start phase spawns `serve()`, so the window is
    /// reachable from outside.
    outbox: OnceLock<Arc<Outbox>>,
    /// This process's instance id - what a group marker names as the group's
    /// owner.
    instance_id: Uuid,
    /// Where a test holds a chained publish inside its transaction, to put
    /// another writer's commit there: before the producer-head reads, or
    /// after them.
    #[cfg(any(test, feature = "test-utils"))]
    chain_pauses: super::producer_chain::ChainPauses,
    /// Where a test holds a cursor write inside its transaction, to put
    /// another writer's write of the same cursor there, or to drop the write
    /// part-way.
    #[cfg(any(test, feature = "test-utils"))]
    cursor_pauses: CursorPauses,
}

/// The point where a test holds a cursor write: inside its transaction, right
/// before each cursor's upsert.
#[cfg(any(test, feature = "test-utils"))]
struct CursorPauses {
    before_write: crate::test_support::PausePoint,
}

impl Storage {
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<toolkit_db::DbError>>,
        spec_manager: Arc<dyn SpecificationManager>,
        instance_id: Uuid,
    ) -> Self {
        Self {
            db,
            spec_manager,
            cache: OnceLock::new(),
            outbox: OnceLock::new(),
            instance_id,
            #[cfg(any(test, feature = "test-utils"))]
            chain_pauses: super::producer_chain::ChainPauses {
                before_head_read: crate::test_support::PausePoint::default(),
                after_head_read: crate::test_support::PausePoint::default(),
            },
            #[cfg(any(test, feature = "test-utils"))]
            cursor_pauses: CursorPauses {
                before_write: crate::test_support::PausePoint::default(),
            },
        }
    }

    /// The point a chained publish passes inside its transaction just before
    /// it reads the producer heads (on `SQLite`: before it takes the write
    /// lock).
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn before_head_read_pause(&self) -> &crate::test_support::PausePoint {
        &self.chain_pauses.before_head_read
    }

    /// The point a chained publish passes right after it read the producer
    /// heads, before it writes anything (on `SQLite`: holding the write lock).
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn after_head_read_pause(&self) -> &crate::test_support::PausePoint {
        &self.chain_pauses.after_head_read
    }

    /// The point a cursor write passes inside its transaction right before
    /// each cursor's upsert (on `SQLite` with the default deferred `BEGIN`:
    /// before the first one, holding no lock yet).
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn before_cursor_write_pause(&self) -> &crate::test_support::PausePoint {
        &self.cursor_pauses.before_write
    }

    /// Wires the routing markers' `ClusterCacheV1` in once
    /// `EventBrokerModule::serve()` has resolved it.
    ///
    /// Resolved in `serve()`, not `init()`: a co-located `ClusterGear`
    /// registers its backends into the `ClientHub` only in the platform's
    /// *start* phase (`host_runtime.rs` runs every gear's `init()` first),
    /// whereas `Storage` is built in `init()`. EB reaches the cluster through
    /// `cluster-sdk` with no `deps = [cluster]` edge; `resolve()` binds
    /// against whatever `ClusterClient` is registered - a co-located backend
    /// or a remote handle via `cluster-sdk/grpc-client` - and does not block
    /// on the cluster being up, deferring validation to readiness. This was
    /// found by booting the standalone binary, not by any test: every test
    /// wires the cache directly, bypassing the lifecycle.
    ///
    /// # Panics
    /// Panics if called more than once.
    pub fn set_cache(&self, cache: ClusterCacheV1) {
        assert!(
            self.cache.set(cache).is_ok(),
            "Storage::set_cache called twice"
        );
    }

    fn cache(&self) -> Result<&ClusterCacheV1, DomainError> {
        if self.cache.get().is_none() {
            // Logged here as well as where a request's error becomes a `503`:
            // the background consumer-group sweeper reaches `cache()` too, and
            // this line names the startup window whichever caller hit it.
            tracing::warn!("request refused: the cluster cache is not wired yet");
        }
        self.cache
            .get()
            .ok_or_else(|| DomainError::StorageUnavailable {
                reason: "the cluster cache is not wired yet".to_owned(),
                source: None,
            })
    }

    /// Whether `serve()` has started the ingest outbox. The publish path is
    /// unavailable until it has, which is what the gear's readiness check
    /// reports on.
    #[must_use]
    pub fn outbox_installed(&self) -> bool {
        self.outbox.get().is_some()
    }

    /// Whether `serve()` has resolved the cluster cache. The subscription and
    /// consumer-group routing markers are unavailable until it has.
    #[must_use]
    pub fn cache_installed(&self) -> bool {
        self.cache.get().is_some()
    }

    /// Wires the ingest outbox pipeline in once `EventBrokerModule::serve()`
    /// has started it.
    ///
    /// # Panics
    /// Panics if called more than once.
    pub fn set_outbox(&self, outbox: Arc<Outbox>) {
        assert!(
            self.outbox.set(outbox).is_ok(),
            "Storage::set_outbox called twice"
        );
    }
}

#[async_trait]
impl ConsumerGroupRepo for Storage {
    async fn create_consumer_group(
        &self,
        group: ConsumerGroup,
    ) -> Result<ConsumerGroup, DomainError> {
        let conn = self.db.conn()?;
        let am = consumer_group::ActiveModel {
            id: Set(group.id.as_ref().to_owned()),
            kind: Set(kind_to_str(group.kind).to_owned()),
            tenant_id: Set(group.tenant_id),
            owner_principal_id: Set(group.owner_principal_id),
            description: Set(group.description.clone()),
            created_at: Set(group.created_at),
        };
        secure_insert::<consumer_group::Entity>(
            am,
            &AccessScope::for_tenant(group.tenant_id),
            &conn,
        )
        .await?;
        Ok(group)
    }

    async fn find_consumer_group(
        &self,
        id: &GtsInstanceId,
    ) -> Result<Option<ConsumerGroup>, DomainError> {
        let conn = self.db.conn()?;
        let row = consumer_group::Entity::find_by_id(id.as_ref().to_owned())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?;
        row.map(row_to_consumer_group).transpose()
    }

    /// **Known limitation, deliberately scoped (not silently dropped):**
    /// still fetches every row in one query rather than pushing
    /// `limit`/`cursor` to SQL. A correct pushdown implementation would also
    /// need to move `DeliveryServiceImpl::list_consumer_groups`'s
    /// anonymous-vs-named tenant-visibility partitioning into the SQL `WHERE`
    /// clause, since that logic - not a plain tenant-scoped `AccessScope` - decides which rows
    /// a caller may see; that's a larger, separate piece of work than this
    /// change's storage-layer scope. What *did* change: this now queries a
    /// real gear-database table instead of an in-memory `HashMap`, resolving the
    /// "no persistence" half of the original gap.
    async fn list_consumer_groups(&self) -> Result<Vec<ConsumerGroup>, DomainError> {
        let conn = self.db.conn()?;
        let rows = consumer_group::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await?;
        rows.into_iter().map(row_to_consumer_group).collect()
    }

    async fn delete_consumer_group(&self, id: &GtsInstanceId) -> Result<(), DomainError> {
        let conn = self.db.conn()?;
        consumer_group::Entity::delete_many()
            .filter(consumer_group::Column::Id.eq(id.as_ref()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await?;
        Ok(())
    }
}

/// A cursor row's primary key, `(consumer_group, topic_id, partition)`, with
/// its fields in the key's column order so the derived `Ord` compares them in
/// that order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CursorKey {
    consumer_group: String,
    topic_id: i64,
    partition: i32,
}

/// One cursor row to upsert, with its key and the tenant that scopes it.
#[derive(Debug)]
struct CursorRow {
    key: CursorKey,
    tenant_id: Uuid,
    model: cursor::ActiveModel,
}

impl CursorRow {
    fn new(
        consumer_group: &str,
        topic_id: i64,
        partition: i32,
        tenant_id: Uuid,
        offset: Sequence,
    ) -> Self {
        Self {
            key: CursorKey {
                consumer_group: consumer_group.to_owned(),
                topic_id,
                partition,
            },
            tenant_id,
            model: cursor::ActiveModel {
                consumer_group: Set(consumer_group.to_owned()),
                topic_id: Set(topic_id),
                partition: Set(partition),
                tenant_id: Set(tenant_id),
                offset: Set(offset.as_i64()),
                updated_at: Set(Utc::now()),
            },
        }
    }
}

/// The rows one `put_cursors` call writes, collected in the caller's order.
#[derive(Debug)]
struct CursorRows(Vec<CursorRow>);

impl CursorRows {
    fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    fn push(&mut self, row: CursorRow) {
        self.0.push(row);
    }

    fn in_key_order(mut self) -> Vec<CursorRow> {
        // Two calls writing overlapping rows take their row locks in the same
        // order, so neither can hold a lock the other waits on while waiting
        // on one the other holds. The sort is stable: rows sharing a key keep
        // the caller's order, and the caller's last one is still written last.
        self.0.sort_by(|a, b| a.key.cmp(&b.key));
        self.0
    }
}

#[async_trait]
impl CursorRepo for Storage {
    async fn find_cursor(
        &self,
        consumer_group: &GtsInstanceId,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<Option<Cursor>, DomainError> {
        let topic_id = self.spec_manager.resolve_topic_id(topic).await?;
        let conn = self.db.conn()?;
        let row = cursor::Entity::find()
            .filter(cursor::Column::ConsumerGroup.eq(consumer_group.as_ref()))
            .filter(cursor::Column::TopicId.eq(topic_id))
            .filter(cursor::Column::Partition.eq(partition))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?;
        Ok(row.map(|row| Cursor {
            topic: topic.clone(),
            consumer_group: consumer_group.clone(),
            partition: row.partition,
            offset: Sequence::assigned(row.offset),
        }))
    }

    async fn put_cursors(&self, cursors: &[Cursor]) -> Result<(), DomainError> {
        if cursors.is_empty() {
            return Ok(());
        }
        // Every read happens before the transaction opens, so it holds only
        // the writes: resolving a topic id may read the database, and a
        // connection is refused inside a transaction. Each row denormalizes
        // its owning group's tenant, read once per group.
        let mut tenants: HashMap<&str, Uuid> = HashMap::new();
        let mut rows = CursorRows::with_capacity(cursors.len());
        for wanted in cursors {
            let topic_id = self.spec_manager.resolve_topic_id(&wanted.topic).await?;
            let group_id = wanted.consumer_group.as_ref();
            let tenant_id = if let Some(tenant_id) = tenants.get(group_id) {
                *tenant_id
            } else {
                let group = consumer_group::Entity::find_by_id(group_id.to_owned())
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .one(&self.db.conn()?)
                    .await?
                    .ok_or_else(|| DomainError::NotFound {
                        code: ErrorCode::ConsumerGroupNotFound,
                        message: format!(
                            "consumer group '{}' is not registered",
                            wanted.consumer_group
                        ),
                        resource: wanted.consumer_group.to_string(),
                    })?;
                tenants.insert(group_id, group.tenant_id);
                group.tenant_id
            };
            rows.push(CursorRow::new(
                group_id,
                topic_id,
                wanted.partition,
                tenant_id,
                wanted.offset,
            ));
        }
        let rows = rows.in_key_order();

        #[cfg(any(test, feature = "test-utils"))]
        let before_write = self.cursor_pauses.before_write.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    for CursorRow {
                        tenant_id,
                        model: am,
                        ..
                    } in rows
                    {
                        #[cfg(any(test, feature = "test-utils"))]
                        before_write.pass().await;
                        // A cursor that already exists, including one a
                        // concurrent first write just created, is overwritten:
                        // the last writer wins. On `MySQL` a conflict that
                        // rewrites identical values still counts as one
                        // affected row, which `exec` requires, because sqlx
                        // connects with `CLIENT_FOUND_ROWS`.
                        let on_conflict = SecureOnConflict::<cursor::Entity>::columns([
                            cursor::Column::ConsumerGroup,
                            cursor::Column::TopicId,
                            cursor::Column::Partition,
                        ])
                        .update_columns([cursor::Column::Offset, cursor::Column::UpdatedAt])?;
                        cursor::Entity::insert(am.clone())
                            .secure()
                            .scope_with_model(&AccessScope::for_tenant(tenant_id), &am)?
                            .on_conflict(on_conflict)
                            .exec(tx)
                            .await?;
                    }
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }
}

#[async_trait]
impl IdempotencyGuard for Storage {
    async fn check_and_enqueue(
        &self,
        request: PublishEnqueue,
    ) -> Result<ProducerIdempotencyOutcome, DomainError> {
        match self.run_chain_transaction(vec![request]).await? {
            Enqueued::Committed { outcomes, wake } => {
                // Fired only now the transaction has committed, never inside
                // it: marking a partition dirty before its row is durable lets
                // a sequencer claim the partition, find nothing, and clear the
                // flag before the commit lands, leaving the row to the cold
                // reconciler. A rollback never reaches here, so a wake for rows
                // nobody wrote is dropped with it. Fired before the outcome
                // count is checked, since the rows are durable either way.
                wake.fire();
                let [outcome] = <[_; 1]>::try_from(outcomes).map_err(|outcomes| {
                    DomainError::Internal(format!(
                        "the chain transaction answered {} outcomes for one publish",
                        outcomes.len()
                    ))
                })?;
                Ok(outcome)
            }
            Enqueued::Violation {
                producer_id,
                last_sequence,
                ..
            } => Ok(ProducerIdempotencyOutcome::SequenceViolation {
                producer_id,
                last_sequence,
            }),
        }
    }

    async fn check_and_enqueue_batch(
        &self,
        requests: Vec<PublishEnqueue>,
    ) -> Result<Vec<ProducerIdempotencyOutcome>, DomainError> {
        match self.run_chain_transaction(requests).await? {
            Enqueued::Committed { outcomes, wake } => {
                // After commit, as on the single path.
                wake.fire();
                Ok(outcomes)
            }
            Enqueued::Violation {
                index,
                producer_id,
                topic,
                last_sequence,
            } => Err(DomainError::SequenceViolation {
                index,
                producer_id,
                topic: topic.to_string(),
                last_sequence,
            }),
        }
    }
}

impl Storage {
    /// The producer-chain transaction over `requests`, once the ingest outbox
    /// pipeline is running.
    async fn run_chain_transaction(
        &self,
        requests: Vec<PublishEnqueue>,
    ) -> Result<Enqueued, DomainError> {
        // A request must never be able to panic the process, and this one can
        // genuinely arrive before the pipeline exists: the REST listener is up
        // (and `/healthz`, a liveness probe, answers 200) before the gear's
        // start phase runs, and an instance deployed without the ingest role
        // never starts a pipeline at all. Both are retry-or-route-elsewhere
        // conditions for the caller, so they are reported as an unavailable
        // storage path rather than an internal failure.
        let Some(outbox) = self.outbox.get() else {
            // Its own line although the `503` mapping logs the same reason:
            // this is the one refusal that means the instance was advertised
            // before it could serve, so it reads as such in the log.
            tracing::warn!("publish refused: the ingest outbox pipeline is not running yet");
            return Err(DomainError::StorageUnavailable {
                reason: "the ingest outbox pipeline is not running yet".to_owned(),
                source: None,
            });
        };
        super::producer_chain::check_and_enqueue(
            &self.db.db(),
            Arc::clone(outbox),
            requests,
            #[cfg(any(test, feature = "test-utils"))]
            self.chain_pauses.clone(),
        )
        .await
    }
}

#[async_trait]
impl ProducerRegistry for Storage {
    async fn register(
        &self,
        owner: Uuid,
        tenant_id: Uuid,
        mode: ProducerMode,
        client_agent: String,
    ) -> Result<ProducerRegistration, DomainError> {
        let conn = self.db.conn()?;
        let id = Uuid::new_v4();
        let am = producer::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            owner_id: Set(owner),
            mode: Set(producer_mode_to_str(mode).to_owned()),
            client_agent: Set(client_agent.clone()),
            created_at: Set(Utc::now()),
        };
        secure_insert::<producer::Entity>(am, &AccessScope::for_tenant(tenant_id), &conn).await?;
        Ok(ProducerRegistration {
            id,
            mode,
            client_agent,
        })
    }

    async fn find(&self, producer_id: Uuid) -> Result<Option<ProducerRecord>, DomainError> {
        let conn = self.db.conn()?;
        let row = producer::Entity::find_by_id(producer_id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?;
        row.map(|r| {
            Ok(ProducerRecord {
                owner: r.owner_id,
                tenant: r.tenant_id,
                mode: str_to_producer_mode(&r.mode)?,
            })
        })
        .transpose()
    }

    async fn cursors(&self, producer_id: Uuid) -> Result<Option<ProducerCursors>, DomainError> {
        let conn = self.db.conn()?;
        let Some(producer_row) = producer::Entity::find_by_id(producer_id)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?
        else {
            return Ok(None);
        };

        let sequence_rows = producer_sequence::Entity::find()
            .filter(producer_sequence::Column::ProducerId.eq(producer_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await?;

        Ok(Some(ProducerCursors {
            producer_id,
            client_agent: producer_row.client_agent,
            topics: sequence_rows
                .into_iter()
                .map(|row| {
                    Ok(ProducerTopicCursors {
                        topic: parse_gts_id(&row.topic)?,
                        last_sequence: row.last_sequence,
                    })
                })
                .collect::<Result<Vec<_>, DomainError>>()?,
        }))
    }

    async fn reset(
        &self,
        producer_id: Uuid,
        scope: &ProducerResetScope,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn()?;
        let mut filter =
            Condition::all().add(producer_sequence::Column::ProducerId.eq(producer_id));
        if let ProducerResetScope::Topic { topic } = scope {
            filter = filter.add(producer_sequence::Column::Topic.eq(topic.as_str()));
        }
        producer_sequence::Entity::delete_many()
            .filter(filter)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await?;
        Ok(())
    }
}

impl Storage {
    /// The identity of the ingest outbox group `name` (feature 0005 §2.5):
    /// created once, as random 64 bits, by whichever instance reads it first,
    /// and the same on every instance and restart after that. The high half of
    /// every fence key the group's messages append under.
    ///
    /// # Errors
    /// Returns a `DomainError` if the connection, the read or the insert fails,
    /// or if the stored identity is not 16 lowercase hex digits.
    pub async fn outbox_identity(
        &self,
        name: &str,
    ) -> Result<crate::domain::outbox::OutboxId, DomainError> {
        let conn = self.db.conn()?;
        let read = || async {
            outbox_identity::Entity::find_by_id(name.to_owned())
                .secure()
                .scope_with(&AccessScope::allow_all())
                .one(&conn)
                .await
        };
        let stored = |row: outbox_identity::Model| {
            crate::domain::outbox::OutboxId::parse(&row.outbox_id).ok_or_else(|| {
                DomainError::Internal(format!(
                    "ingest outbox identity for '{name}' is not 16 lowercase hex digits"
                ))
            })
        };
        if let Some(row) = read().await? {
            return stored(row);
        }
        let candidate = crate::domain::outbox::OutboxId::new(rand::random::<u64>());
        let inserted = secure_insert::<outbox_identity::Entity>(
            outbox_identity::ActiveModel {
                name: Set(name.to_owned()),
                outbox_id: Set(candidate.to_string()),
                created_at: Set(Utc::now()),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await;
        // Another instance may have inserted first; its identity is the one.
        match (inserted, read().await?) {
            (_, Some(row)) => stored(row),
            (Err(err), None) => Err(DomainError::Internal(format!(
                "ingest outbox identity for '{name}' could not be stored: {err}"
            ))),
            (Ok(_), None) => Err(DomainError::Internal(format!(
                "ingest outbox identity for '{name}' vanished after it was stored"
            ))),
        }
    }
}

impl Storage {
    /// Delete a producer's registration row (`event_broker_producer`); its
    /// `event_broker_producer_sequence` chain rows stay. After it, the next
    /// publish naming that id is a `404 ProducerNotFound`. There is no
    /// client-facing deregister, and the registration Reaper is not yet
    /// implemented (tracked in #5651), so only `test_support` fault injection
    /// reaches this.
    ///
    /// # Errors
    ///
    /// Returns a `DomainError` if the connection or the delete fails.
    pub async fn delete_producer_registration(&self, producer_id: Uuid) -> Result<(), DomainError> {
        let conn = self.db.conn()?;
        producer::Entity::delete_many()
            .filter(producer::Column::Id.eq(producer_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl RoutingMarkers for Storage {
    async fn mark_subscription(
        &self,
        subscription_id: Uuid,
        group: &GtsInstanceId,
    ) -> Result<(), DomainError> {
        // Indefinite: a marker is removed with the member it points at, never
        // aged out from under a member that is still here.
        self.cache()?
            .put(PutRequest {
                key: &subscription_marker_key(subscription_id),
                value: group.as_ref().as_bytes(),
                ttl: Ttl::Indefinite,
            })
            .await?;
        Ok(())
    }

    async fn unmark_subscription(&self, subscription_id: Uuid) -> Result<(), DomainError> {
        self.cache()?
            .delete(&subscription_marker_key(subscription_id))
            .await?;
        Ok(())
    }

    async fn mark_group(&self, group: ConsumerGroupId) -> Result<(), DomainError> {
        self.cache()?
            .put(PutRequest {
                key: &group_marker_key(group),
                value: self.instance_id.to_string().as_bytes(),
                ttl: Ttl::Indefinite,
            })
            .await?;
        Ok(())
    }

    async fn unmark_group(&self, group: ConsumerGroupId) -> Result<(), DomainError> {
        self.cache()?.delete(&group_marker_key(group)).await?;
        Ok(())
    }
}

impl SyncWaitRegistry for Storage {
    fn subscribe(&self, trace: SyncTrace) -> Box<dyn SyncWait> {
        // The ingest outbox owns the trace machinery. If it is not wired yet
        // the publish's own `check_and_enqueue` fails with a `503` before the
        // wait is ever awaited, so an unsubscribed waiter is only ever dropped.
        let subscription = self
            .outbox
            .get()
            .and_then(|outbox| outbox.subscribe(&trace.id().to_string()).ok());
        Box::new(TraceWaiter { subscription })
    }
}

/// A [`SyncWait`] over an outbox [`TraceSubscription`]: awaiting it holds until
/// the traced batch completes or the wait elapses.
struct TraceWaiter {
    /// `None` when the outbox was not wired (so nothing could subscribe); the
    /// wait then resolves at once to the honest `202`.
    subscription: Option<TraceSubscription>,
}

#[async_trait]
impl SyncWait for TraceWaiter {
    async fn stored(self: Box<Self>, wait: Duration) -> bool {
        let Some(subscription) = self.subscription else {
            // Nothing subscribed (the outbox was not wired): the honest `202`.
            return false;
        };
        // A completion within the wait means the traced batch was stored. Ingest
        // dead-letters nothing an ingest row can hit (feature 0007 §4.1 retries
        // forever), so a completion is always clean today; the `is_clean` guard
        // keeps a batch that ended with a failure from being reported stored,
        // should an ingest row ever become dead-letterable. A `None` (the outbox
        // stopped) or an elapsed wait is the honest `202`.
        matches!(
            tokio::time::timeout(wait, subscription.completion()).await,
            Ok(Some(outcome)) if outcome.is_clean()
        )
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage, Partitions};

    use super::*;
    use crate::test_support::gear_db::ScratchSqlite;
    use crate::test_support::type_registry::registry_holding;

    const TEST_INSTANCE_ID: Uuid = Uuid::from_u128(0x5eed);
    use crate::domain::idempotency::ProducerChainCheck;
    use crate::domain::model::ConsumerGroupKind;
    use crate::infra::specification::TypesRegistrySpecificationManager;

    const TOPIC_ID: &str =
        toolkit_gts::gts_id!("cf.core.events.topic.v1~example.eb.storage.topic.v1");

    /// How long a test waits for a held write to reach its pause point, or
    /// for a write to finish.
    const BOUND: Duration = Duration::from_secs(10);

    /// A `LeasedMessageHandler` that always acknowledges - `Storage`'s own
    /// unit tests exercise `check_and_enqueue`'s transactional
    /// producer-state + outbox-insert behavior, not the drain side (that's
    /// `infra::outbox::IngestOutboxHandler`'s own test module).
    struct NoopLeasedHandler;

    #[async_trait]
    impl LeasedMessageHandler for NoopLeasedHandler {
        async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
            MessageResult::Ok
        }
    }

    /// A fresh temp-file `SQLite` database, and the DSN that reaches it.
    ///
    /// The DSN is there so a test can open its own second connection: the
    /// outbox's bookkeeping tables are not reachable through `DBProvider`
    /// (`toolkit-db` deliberately hands out no raw `SeaORM` executor), and a
    /// test that needs to see where a row was routed has to read them.
    async fn test_db() -> ScratchSqlite {
        ScratchSqlite::open("cf-eb-storage-test").await
    }

    /// A `Storage` wired against a fresh temp-file `SQLite` DB, as
    /// [`storage_on`] wires it; and that database, whose DSN lets a test read
    /// the outbox's own bookkeeping tables directly.
    async fn test_storage() -> (Storage, ScratchSqlite) {
        let scratch = test_db().await;
        (storage_on(scratch.db()).await, scratch)
    }

    /// A `Storage` on `db` with a real (`standalone`-provider-backed)
    /// `ClusterCacheV1`, with one topic (`TOPIC_ID`) already bulk-loaded into
    /// `SpecificationManager` so `CursorRepo` tests can resolve it.
    async fn storage_on(db: &Arc<DBProvider<toolkit_db::DbError>>) -> Storage {
        let db = Arc::clone(db);
        let client: Arc<dyn types_registry_sdk::PlatformTypesRegistryApi> = registry_holding(
            &[serde_json::json!({
                "id": TOPIC_ID,
                "description": "a topic this test keeps cursors for",
            })],
            &[],
        );
        crate::infra::specification::bulk_load(
            &client,
            &db,
            &crate::test_support::StaticTypesRegistry::empty_config(),
        )
        .await
        .expect("bulk_load");
        let spec_manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));

        let (hub, _cluster) = crate::test_support::standalone_event_broker_cluster().await;
        let cache = crate::domain::cluster::EventBrokerCluster::resolve(&hub)
            .await
            .expect("cluster resolves")
            .cache;

        let storage = Storage::new(Arc::clone(&db), Arc::new(spec_manager), TEST_INSTANCE_ID);
        storage.set_cache(cache);

        // `check_and_enqueue` needs a started outbox pipeline to insert
        // into - a no-op drain handler is enough for these tests, which
        // exercise the enqueue transaction, not the drain side. The
        // returned handle is intentionally dropped without `.stop()`: per
        // `OutboxHandle`'s own doc comment, dropping it cancels the
        // pipeline's background workers immediately - but `enqueue()` is a
        // plain DB insert that doesn't depend on those workers running, so
        // these tests (which never assert on a row actually draining) are
        // unaffected. Contrast `test_support::harness`'s `EventBrokerHarness`,
        // which keeps its handle alive for its whole lifetime because its
        // tests do rely on real draining.
        let handle = Outbox::builder(db.db())
            .table_prefix(crate::domain::outbox::OUTBOX_TABLE_PREFIX)
            .expect("the default group's prefix is valid")
            .queue(crate::domain::outbox::INGEST_QUEUE_NAME, Partitions::of(4))
            .leased(NoopLeasedHandler)
            .start()
            .await
            .expect("outbox start");
        storage.set_outbox(Arc::clone(handle.outbox()));
        storage
    }

    /// How many rows each registered ingest outbox partition holds, ascending
    /// by partition. A partition at `0` never received anything.
    ///
    /// Read from the staging table rather than from
    /// the partitions table's `sequence`, because these tests deliberately
    /// drop the pipeline handle (see [`test_storage`]) and so the
    /// sequencer that fills that column never runs. Enqueue's own routing
    /// decision is already final here: the row names its partition the moment
    /// it is staged.
    async fn outbox_partition_row_counts(dsn: &str) -> Vec<(i32, i64)> {
        use sea_orm::ConnectionTrait;
        use sea_orm::sea_query::{Alias, Expr, ExprTrait, JoinType, Order, Query};

        let prefix = crate::domain::outbox::OUTBOX_TABLE_PREFIX;
        let (p, i) = (Alias::new("p"), Alias::new("i"));
        let partition = Expr::col((p.clone(), Alias::new("partition")));
        let conn = sea_orm::Database::connect(dsn).await.expect("connect");
        let rows = conn
            .query_all(
                Query::select()
                    .expr_as(partition.clone(), "partition")
                    .expr_as(
                        Expr::col((i.clone(), Alias::new("id"))).count(),
                        "rows_staged",
                    )
                    .from_as(Alias::new(format!("{prefix}_partitions")), p.clone())
                    .join_as(
                        JoinType::LeftJoin,
                        Alias::new(format!("{prefix}_incoming")),
                        i.clone(),
                        Expr::col((i, Alias::new("partition_id")))
                            .equals((p.clone(), Alias::new("id"))),
                    )
                    .and_where(
                        Expr::col((p, Alias::new("queue")))
                            .eq(crate::domain::outbox::INGEST_QUEUE_NAME),
                    )
                    .add_group_by([partition.clone()])
                    .order_by_expr(partition, Order::Asc),
            )
            .await
            .expect("read outbox partitions");
        rows.into_iter()
            .map(|row| {
                (
                    row.try_get::<i32>("", "partition").expect("partition"),
                    row.try_get::<i64>("", "rows_staged").expect("rows_staged"),
                )
            })
            .collect()
    }

    /// Stateless publishes - the common case, no producer `meta` at all - land
    /// in the ingest outbox partition they are given, so every
    /// sequencer/processor slot carries work.
    #[tokio::test]
    async fn stateless_publishes_reach_every_outbox_partition() {
        let (storage, scratch) = test_storage().await;
        let dsn = scratch.dsn().to_owned();

        for ingest_partition in [0, 1, 2, 3, 0, 1, 2, 3] {
            let outcome = storage
                .check_and_enqueue(
                    PublishEnqueue::builder(
                        ingest_partition,
                        b"payload".to_vec(),
                        "application/json",
                    )
                    .build(),
                )
                .await
                .expect("check_and_enqueue must succeed");
            assert_eq!(outcome, ProducerIdempotencyOutcome::Accept);
        }

        assert_eq!(
            outbox_partition_row_counts(&dsn).await,
            vec![(0, 2), (1, 2), (2, 2), (3, 2)]
        );
    }

    /// A publish can genuinely arrive before the outbox pipeline exists - the
    /// REST listener is up before the gear's start phase runs - and used to
    /// panic the request task. It must be a retryable answer instead.
    #[tokio::test]
    async fn publish_before_the_pipeline_starts_is_reported_as_unavailable() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        let spec_manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
        // No `set_outbox`: exactly the state `Storage` is in between `init()`
        // and `serve()`.
        let storage = Storage::new(db, Arc::new(spec_manager), TEST_INSTANCE_ID);

        let err = storage
            .check_and_enqueue(
                PublishEnqueue::builder(0, b"payload".to_vec(), "application/json").build(),
            )
            .await
            .expect_err("a publish before the pipeline starts must not succeed");

        assert!(
            matches!(&err, DomainError::StorageUnavailable { reason, .. }
                if reason == "the ingest outbox pipeline is not running yet"),
            "expected a retryable StorageUnavailable, got {err:?}"
        );
    }

    /// The routing markers sit in the same startup window as the publish path
    /// above. They must report it the same way - a retryable status the caller
    /// can act on, never a panicked request task.
    #[tokio::test]
    async fn marking_a_subscription_before_the_cache_is_wired_is_reported_as_unavailable() {
        let scratch = test_db().await;
        let db = Arc::clone(scratch.db());
        let spec_manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
        // No `set_cache`: the state `Storage` is in between `init()` and the
        // point `serve()` resolves the cluster.
        let storage = Storage::new(db, Arc::new(spec_manager), TEST_INSTANCE_ID);

        let err = storage
            .mark_subscription(
                Uuid::now_v7(),
                &GtsInstanceId::try_new(toolkit_gts::gts_id!(
                    "cf.core.events.consumer_group.v1~example.eb.storage.group.v1"
                ))
                .unwrap(),
            )
            .await
            .expect_err("marking before the cache is wired must not succeed");

        assert!(
            matches!(&err, DomainError::StorageUnavailable { reason, .. }
                if reason == "the cluster cache is not wired yet"),
            "expected a retryable StorageUnavailable, got {err:?}"
        );
    }

    fn test_consumer_group(id: &str, tenant_id: Uuid) -> ConsumerGroup {
        ConsumerGroup {
            id: GtsInstanceId::try_new(id).unwrap(),
            kind: ConsumerGroupKind::Anonymous,
            tenant_id,
            owner_principal_id: Uuid::new_v4(),
            description: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn consumer_group_create_find_list_delete_round_trip() {
        let (storage, _scratch) = test_storage().await;
        let tenant_id = Uuid::new_v4();
        let group = test_consumer_group(
            toolkit_gts::gts_id!("cf.core.events.consumer_group.v1~example.eb.storage.cg1.v1"),
            tenant_id,
        );

        let created = storage
            .create_consumer_group(group.clone())
            .await
            .expect("create must succeed");
        assert_eq!(created.id, group.id);

        let found = storage
            .find_consumer_group(&group.id)
            .await
            .expect("find must succeed")
            .expect("group must exist");
        assert_eq!(found.tenant_id, tenant_id);
        assert_eq!(found.kind, ConsumerGroupKind::Anonymous);

        let listed = storage
            .list_consumer_groups()
            .await
            .expect("list must succeed");
        assert!(listed.iter().any(|g| g.id == group.id));

        storage
            .delete_consumer_group(&group.id)
            .await
            .expect("delete must succeed");
        assert!(
            storage
                .find_consumer_group(&group.id)
                .await
                .expect("find after delete must succeed")
                .is_none()
        );
    }

    /// A subscription marker names the subscription's group, is removed on
    /// unmark, and carries no TTL - it lives exactly as long as the member.
    #[tokio::test]
    async fn subscription_marker_round_trip() {
        let (storage, _scratch) = test_storage().await;
        let id = Uuid::new_v4();
        let group = GtsInstanceId::try_new(toolkit_gts::gts_id!(
            "cf.core.events.consumer_group.v1~example.eb.storage.cg2.v1"
        ))
        .unwrap();
        let key = subscription_marker_key(id);
        assert!(
            storage
                .cache()
                .expect("the test wired the cache")
                .get(&key)
                .await
                .expect("get")
                .is_none()
        );

        storage
            .mark_subscription(id, &group)
            .await
            .expect("mark must succeed");
        let entry = storage
            .cache()
            .expect("the test wired the cache")
            .get(&key)
            .await
            .expect("get")
            .expect("the marker must exist");
        assert_eq!(entry.value, group.as_ref().as_bytes());

        storage
            .unmark_subscription(id)
            .await
            .expect("unmark must succeed");
        assert!(
            storage
                .cache()
                .expect("the test wired the cache")
                .get(&key)
                .await
                .expect("get")
                .is_none()
        );
    }

    #[tokio::test]
    async fn cursor_find_put_round_trip_denormalizes_tenant() {
        let (storage, _scratch) = test_storage().await;
        let tenant_id = Uuid::new_v4();
        let group = test_consumer_group(
            toolkit_gts::gts_id!("cf.core.events.consumer_group.v1~example.eb.storage.cg3.v1"),
            tenant_id,
        );
        storage
            .create_consumer_group(group.clone())
            .await
            .expect("create must succeed");

        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        assert!(
            storage
                .find_cursor(&group.id, &topic, 0)
                .await
                .expect("find_cursor must succeed")
                .is_none()
        );

        let cursor = Cursor {
            topic: topic.clone(),
            consumer_group: group.id.clone(),
            partition: 0,
            offset: Sequence::assigned(42),
        };
        storage
            .put_cursors(std::slice::from_ref(&cursor))
            .await
            .expect("put_cursors must succeed");

        let found = storage
            .find_cursor(&group.id, &topic, 0)
            .await
            .expect("find_cursor must succeed")
            .expect("cursor must exist");
        assert_eq!(found.offset, Sequence::assigned(42));

        // Update in place - same (consumer_group, topic, partition) key.
        let updated = Cursor {
            offset: Sequence::assigned(99),
            ..cursor
        };
        storage
            .put_cursors(std::slice::from_ref(&updated))
            .await
            .expect("put_cursors update must succeed");
        let found = storage
            .find_cursor(&group.id, &topic, 0)
            .await
            .expect("find_cursor must succeed")
            .expect("cursor must exist");
        assert_eq!(
            found.offset,
            Sequence::assigned(99),
            "second write must update, not duplicate"
        );

        // One call writing two cursors writes both: partition 0 is updated
        // and partition 1 is created.
        storage
            .put_cursors(&[
                Cursor {
                    offset: Sequence::assigned(100),
                    ..updated.clone()
                },
                Cursor {
                    partition: 1,
                    offset: Sequence::assigned(101),
                    ..updated
                },
            ])
            .await
            .expect("put_cursors of two cursors must succeed");
        let offsets = (
            storage
                .find_cursor(&group.id, &topic, 0)
                .await
                .expect("find_cursor must succeed")
                .map(|found| found.offset),
            storage
                .find_cursor(&group.id, &topic, 1)
                .await
                .expect("find_cursor must succeed")
                .map(|found| found.offset),
        );
        assert_eq!(
            offsets,
            (Some(Sequence::assigned(100)), Some(Sequence::assigned(101)))
        );

        // Each row carries its group's tenant.
        let mut tenants: Vec<(i32, Uuid)> = cursor::Entity::find()
            .filter(cursor::Column::ConsumerGroup.eq(group.id.as_ref()))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&storage.db.conn().expect("a connection"))
            .await
            .expect("reading the cursor rows must succeed")
            .into_iter()
            .map(|row| (row.partition, row.tenant_id))
            .collect();
        tenants.sort_unstable();
        assert_eq!(tenants, vec![(0, tenant_id), (1, tenant_id)]);
    }

    /// One call's rows come out in primary-key order whatever order the
    /// caller gave, and rows sharing a key keep the caller's order, so the
    /// caller's last row for a key is still the one written last.
    #[test]
    fn cursor_rows_are_written_in_primary_key_order() {
        const GROUP: &str = "cf.core.events.consumer_group.v1~example.eb.storage.cg9.v1";
        const TOPIC_A: i64 = 7;
        const TOPIC_B: i64 = 11;
        let tenant_id = Uuid::new_v4();
        let mut rows = CursorRows::with_capacity(5);
        for (topic_id, partition, offset) in [
            (TOPIC_B, 1, 10),
            (TOPIC_A, 1, 20),
            (TOPIC_B, 0, 30),
            (TOPIC_A, 0, 40),
            (TOPIC_A, 0, 50),
        ] {
            rows.push(CursorRow::new(
                GROUP,
                topic_id,
                partition,
                tenant_id,
                Sequence::assigned(offset),
            ));
        }

        let written: Vec<(String, i64, i32, i64)> = rows
            .in_key_order()
            .into_iter()
            .map(|row| {
                (
                    row.key.consumer_group,
                    row.key.topic_id,
                    row.key.partition,
                    row.model.offset.unwrap(),
                )
            })
            .collect();
        assert_eq!(
            written,
            vec![
                (GROUP.to_owned(), TOPIC_A, 0, 40),
                (GROUP.to_owned(), TOPIC_A, 0, 50),
                (GROUP.to_owned(), TOPIC_A, 1, 20),
                (GROUP.to_owned(), TOPIC_B, 0, 30),
                (GROUP.to_owned(), TOPIC_B, 1, 10),
            ]
        );
    }

    /// A cursor write dropped after its first cursor's upsert, as a seek
    /// dropped by a disconnecting client is, rolls back: neither cursor is
    /// written.
    #[tokio::test]
    async fn put_cursors_cancelled_after_its_first_write_leaves_none() {
        let (storage, _scratch) = test_storage().await;
        let tenant_id = Uuid::from_u128(0x0190_f1a2_0000_7000_a000_0000_0000_0c04);
        let group = test_consumer_group(
            toolkit_gts::gts_id!("cf.core.events.consumer_group.v1~example.eb.storage.dropped.v1"),
            tenant_id,
        );
        storage
            .create_consumer_group(group.clone())
            .await
            .expect("create must succeed");
        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let cursors = [0, 1].map(|partition| Cursor {
            topic: topic.clone(),
            consumer_group: group.id.clone(),
            partition,
            offset: Sequence::assigned(5),
        });
        let pause = storage.before_cursor_write_pause().clone();

        {
            let write = storage.put_cursors(&cursors);
            tokio::pin!(write);

            // Hold the first cursor's upsert.
            pause.arm();
            tokio::time::timeout(BOUND, async {
                tokio::select! {
                    () = pause.reached() => {}
                    result = write.as_mut() => panic!(
                        "the write finished before its first upsert was held: {result:?}"
                    ),
                }
            })
            .await
            .expect("the first upsert reaches the pause point");

            // Let it run and hold the second: the held write disarmed the
            // point on arrival, so arming again targets the next upsert.
            pause.arm();
            pause.release();
            tokio::time::timeout(BOUND, async {
                tokio::select! {
                    () = pause.reached() => {}
                    result = write.as_mut() => panic!(
                        "the write finished before its second upsert was held: {result:?}"
                    ),
                }
            })
            .await
            .expect("the second upsert reaches the pause point");
            // The write is dropped here, with its first upsert done.
        }

        for partition in [0, 1] {
            let found =
                tokio::time::timeout(BOUND, storage.find_cursor(&group.id, &topic, partition))
                    .await
                    .expect("the cursor read answers within the bound")
                    .expect("find_cursor must succeed");
            assert!(
                found.is_none(),
                "partition {partition}: the dropped write left {found:?}"
            );
        }
    }

    /// Two members seeding the same partition at once: one write is held
    /// inside its transaction before its upsert while the other creates the
    /// row and commits. The held write is not refused: it commits later, so
    /// its offset stands, in the one row the cursor has. The database has a
    /// production-sized pool, because the held write's transaction keeps its
    /// connection while the other write runs on a second one.
    #[tokio::test]
    async fn two_first_writes_of_one_cursor_both_succeed() {
        let gear_db = crate::test_support::gear_db::GearDb::Sqlite
            .migrated()
            .await;
        let storage = Arc::new(storage_on(gear_db.db()).await);
        let tenant_id = Uuid::from_u128(0x0190_f1a2_0000_7000_a000_0000_0000_0c03);
        let group = test_consumer_group(
            toolkit_gts::gts_id!("cf.core.events.consumer_group.v1~example.eb.storage.seeders.v1"),
            tenant_id,
        );
        storage
            .create_consumer_group(group.clone())
            .await
            .expect("create must succeed");
        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let cursor_at = |offset| Cursor {
            topic: topic.clone(),
            consumer_group: group.id.clone(),
            partition: 2,
            offset: Sequence::assigned(offset),
        };

        let pause = storage.before_cursor_write_pause().clone();
        pause.arm();
        let held = {
            let storage = Arc::clone(&storage);
            let cursor = cursor_at(5);
            tokio::spawn(async move { storage.put_cursors(std::slice::from_ref(&cursor)).await })
        };
        tokio::time::timeout(BOUND, pause.reached())
            .await
            .expect("the first write reaches the pause point");

        tokio::time::timeout(BOUND, storage.put_cursors(&[cursor_at(7)]))
            .await
            .expect("the second write finishes")
            .expect("the second write succeeds");
        pause.release();
        let held = tokio::time::timeout(BOUND, held)
            .await
            .expect("the held write finishes")
            .expect("the held write's task");
        assert_eq!(held.map_err(|err| format!("{err:?}")), Ok(()));

        let found = storage
            .find_cursor(&group.id, &topic, 2)
            .await
            .expect("find_cursor must succeed")
            .map(|found| {
                (
                    found.topic,
                    found.consumer_group,
                    found.partition,
                    found.offset,
                )
            });
        assert_eq!(
            found,
            Some((topic.clone(), group.id.clone(), 2, Sequence::assigned(5)))
        );
        let topic_id = storage
            .spec_manager
            .resolve_topic_id(&topic)
            .await
            .expect("the topic resolves");
        let rows = cursor::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&storage.db.conn().expect("a connection"))
            .await
            .expect("read the cursor rows");
        assert_eq!(
            rows.into_iter()
                .map(|row| (
                    row.consumer_group,
                    row.topic_id,
                    row.partition,
                    row.tenant_id,
                    row.offset
                ))
                .collect::<Vec<_>>(),
            vec![(group.id.as_ref().to_owned(), topic_id, 2, tenant_id, 5)]
        );
    }

    /// A group marker is keyed by the group's own UUID, under a key the
    /// cluster cache admits, and its value names this instance as the owner.
    #[tokio::test]
    async fn a_group_marker_is_keyed_by_the_groups_own_uuid() {
        let (storage, _scratch) = test_storage().await;
        let group = ConsumerGroupId::new(Uuid::from_u128(0xabc));
        let cache = storage.cache().expect("the test wired the cache");
        // Test-only use of `to_uuid`: it returns the UUID an id ends in, so a
        // key derived from the GTS id equals this key for every anonymous group.
        assert_eq!(
            gts::GtsId::try_new(&group.to_gts()).unwrap().to_uuid(),
            group.as_uuid()
        );

        storage.mark_group(group).await.expect("mark must succeed");
        let entry = cache
            .get("group-owner/00000000-0000-0000-0000-000000000abc")
            .await
            .expect("get")
            .expect("the marker must exist");
        assert_eq!(entry.value, TEST_INSTANCE_ID.to_string().as_bytes());

        storage
            .unmark_group(group)
            .await
            .expect("unmark must succeed");
        assert!(
            cache
                .get("group-owner/00000000-0000-0000-0000-000000000abc")
                .await
                .expect("get")
                .is_none()
        );
    }

    /// A JOIN marks its group's owner under the UUID the group's id ends in:
    /// the one the broker minted when it created the group.
    #[tokio::test]
    async fn a_join_marks_the_group_under_the_uuid_its_id_ends_in() {
        let harness = crate::test_support::EventBrokerHarness::builder()
            .build()
            .await;
        let ctx = harness.security_context();
        let group = harness
            .broker()
            .create_consumer_group(
                ctx,
                event_broker_sdk::models::CreateConsumerGroupRequest {
                    client_agent: "marker-test".to_owned(),
                    description: None,
                },
            )
            .await
            .expect("the group is created")
            .id;

        harness
            .broker()
            .join(
                ctx,
                event_broker_sdk::api::JoinRequest {
                    consumer_group: group,
                    client_agent: "marker-test".to_owned(),
                    interests: Vec::new(),
                    session_timeout: None,
                },
            )
            .await
            .expect("the join succeeds");

        let cache = harness.repo().cache().expect("the harness wired the cache");
        let key = format!("group-owner/{}", group.as_uuid());
        assert_eq!(
            cache.scan_prefix("group-owner/").await.expect("scan"),
            vec![key.clone()]
        );
        let entry = cache
            .get(&key)
            .await
            .expect("get")
            .expect("the marker must exist");
        assert_eq!(
            entry.value,
            harness.repo().instance_id.to_string().as_bytes()
        );
    }

    /// A JOIN of a group whose id does not end in a UUID is refused before
    /// any marker is written, and no member is created.
    #[tokio::test]
    async fn a_join_of_a_group_id_without_a_uuid_writes_no_marker() {
        const GROUP: &str = "gts.cf.core.events.consumer_group.v1~x.eb.storage.unmarked.v1";
        let harness = crate::test_support::EventBrokerHarness::builder()
            .build()
            .await;
        let ctx = harness.security_context();
        harness
            .repo()
            .create_consumer_group(ConsumerGroup {
                id: GtsInstanceId::try_new(GROUP).unwrap(),
                kind: ConsumerGroupKind::Anonymous,
                tenant_id: ctx.subject_tenant_id(),
                owner_principal_id: ctx.subject_id(),
                description: None,
                created_at: DateTime::parse_from_rfc3339("2026-10-01T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            })
            .await
            .expect("the group is seeded");

        let result = harness
            .delivery()
            .join(
                ctx,
                crate::domain::delivery::JoinRequest {
                    consumer_group: GtsInstanceId::try_new(GROUP).unwrap(),
                    client_agent: "marker-test".to_owned(),
                    interests: Vec::new(),
                    session_timeout: Duration::from_secs(30),
                },
            )
            .await;

        let message = match result {
            Err(DomainError::Internal(message)) => message,
            Err(other) => panic!("expected Internal, got {other:?}"),
            Ok(_) => panic!("the JOIN must be refused"),
        };
        assert_eq!(
            message,
            "consumer group 'gts.cf.core.events.consumer_group.v1~x.eb.storage.unmarked.v1' \
             has no owner marker: its id does not end in a UUID (named consumer groups are \
             tracked in #5702)"
        );
        let cache = harness.repo().cache().expect("the harness wired the cache");
        assert_eq!(
            cache
                .scan_prefix("subscription-group/")
                .await
                .expect("scan"),
            Vec::<String>::new()
        );
        assert_eq!(
            cache.scan_prefix("group-owner/").await.expect("scan"),
            Vec::<String>::new()
        );
        let members = harness.groups().list();
        assert!(members.is_empty(), "expected no member, got {members:?}");
    }

    /// Clearing the markers of the member that emptied its group removes the
    /// group's owner marker, found by the UUID the group's id ends in.
    #[tokio::test]
    async fn clearing_an_emptied_group_removes_its_owner_marker() {
        let (storage, _scratch) = test_storage().await;
        let group = ConsumerGroupId::new(Uuid::from_u128(0xabd));
        let cache = storage.cache().expect("the test wired the cache");
        storage.mark_group(group).await.expect("mark must succeed");
        assert!(
            cache
                .get("group-owner/00000000-0000-0000-0000-000000000abd")
                .await
                .expect("get")
                .is_some()
        );

        crate::domain::consumer_group_coordinator::sweeper::clear_markers(
            &storage,
            &crate::domain::consumer_group_coordinator::Removal {
                subscription_id: Uuid::from_u128(0x5ab),
                group: GtsInstanceId::try_new(&group.to_gts()).unwrap(),
                group_emptied: true,
            },
        )
        .await;

        assert!(
            cache
                .get("group-owner/00000000-0000-0000-0000-000000000abd")
                .await
                .expect("get")
                .is_none()
        );
    }

    /// A group id that does not end in a UUID has no owner marker to clear:
    /// the clear warns and still removes the member's subscription marker.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn clearing_an_emptied_group_without_a_uuid_warns_and_clears_its_subscription_marker() {
        let (storage, _scratch) = test_storage().await;
        let group = GtsInstanceId::try_new(toolkit_gts::gts_id!(
            "cf.core.events.consumer_group.v1~example.eb.storage.cg4.v1"
        ))
        .unwrap();
        let subscription_id = Uuid::from_u128(0x5ac);
        let cache = storage.cache().expect("the test wired the cache");
        storage
            .mark_subscription(subscription_id, &group)
            .await
            .expect("mark must succeed");

        crate::domain::consumer_group_coordinator::sweeper::clear_markers(
            &storage,
            &crate::domain::consumer_group_coordinator::Removal {
                subscription_id,
                group,
                group_emptied: true,
            },
        )
        .await;

        assert_eq!(
            cache
                .scan_prefix("subscription-group/")
                .await
                .expect("scan"),
            Vec::<String>::new()
        );
        assert!(logs_contain(
            "no group marker to clear: the group id does not end in a UUID"
        ));
        assert!(!logs_contain("failed to clear"));
    }

    #[tokio::test]
    async fn producer_register_and_idempotency_round_trip() {
        let (storage, _scratch) = test_storage().await;
        let owner = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        let registration = storage
            .register(owner, tenant_id, ProducerMode::Chained, "agent".to_owned())
            .await
            .expect("register must succeed");
        let record = storage
            .find(registration.id)
            .await
            .expect("find lookup must succeed")
            .expect("registered producer must be found");
        assert_eq!(record.owner, owner);
        assert_eq!(record.tenant, tenant_id);
        assert_eq!(record.mode, ProducerMode::Chained);

        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let enqueue = |previous, sequence| {
            PublishEnqueue::builder(0, b"payload".to_vec(), "application/json")
                .chain(ProducerChainCheck {
                    producer_id: registration.id,
                    topic: topic.clone(),
                    previous,
                    sequence,
                    mode: ProducerMode::Chained,
                })
                .build()
        };

        // First accept: no prior state, any (previous, sequence) is accepted.
        let outcome = storage
            .check_and_enqueue(enqueue(0, 1))
            .await
            .expect("check_and_enqueue must succeed");
        assert_eq!(outcome, ProducerIdempotencyOutcome::Accept);

        // Retry of the same call: duplicate, ignored.
        let outcome = storage
            .check_and_enqueue(enqueue(0, 1))
            .await
            .expect("check_and_enqueue must succeed");
        assert_eq!(outcome, ProducerIdempotencyOutcome::DuplicateIgnore);

        // Correct next step in the chain: accepted.
        let outcome = storage
            .check_and_enqueue(enqueue(1, 2))
            .await
            .expect("check_and_enqueue must succeed");
        assert_eq!(outcome, ProducerIdempotencyOutcome::Accept);

        // A gap in the chain is accepted: `previous` (2) still links to the head
        // (2), and the sequence space tolerates gaps, so jumping to 10 advances
        // the head to 10.
        let outcome = storage
            .check_and_enqueue(enqueue(2, 10))
            .await
            .expect("check_and_enqueue must succeed");
        assert_eq!(outcome, ProducerIdempotencyOutcome::Accept);

        // A genuine chain break: `previous` (3) does not link to the head (10),
        // so this is a sequence violation carrying the current head.
        let outcome = storage
            .check_and_enqueue(enqueue(3, 11))
            .await
            .expect("check_and_enqueue must succeed");
        assert_eq!(
            outcome,
            ProducerIdempotencyOutcome::SequenceViolation {
                producer_id: registration.id,
                last_sequence: 10,
            }
        );

        let cursors = storage
            .cursors(registration.id)
            .await
            .expect("cursors lookup must succeed")
            .expect("producer must exist");
        assert_eq!(cursors.topics.len(), 1);
        assert_eq!(cursors.topics[0].last_sequence, 10);

        storage
            .reset(registration.id, &ProducerResetScope::All)
            .await
            .expect("reset must succeed");
        let cursors = storage
            .cursors(registration.id)
            .await
            .expect("cursors lookup must succeed")
            .expect("producer must still exist after reset");
        assert!(
            cursors.topics.is_empty(),
            "reset must clear all sequence state"
        );
    }

    /// The identity row of the ingest outbox group `name`, read raw, so a test
    /// sees exactly what was stored.
    async fn identity_row(dsn: &str, name: &str) -> Option<(String, String, DateTime<Utc>)> {
        use sea_orm::ConnectionTrait;
        use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};

        let conn = sea_orm::Database::connect(dsn).await.expect("connect");
        conn.query_one(
            Query::select()
                .columns([
                    Alias::new("name"),
                    Alias::new("outbox_id"),
                    Alias::new("created_at"),
                ])
                .from(Alias::new("event_broker__outbox_identity"))
                .and_where(Expr::col(Alias::new("name")).eq(name)),
        )
        .await
        .expect("read the identity table")
        .map(|row| {
            (
                row.try_get::<String>("", "name").expect("name"),
                row.try_get::<String>("", "outbox_id").expect("outbox_id"),
                row.try_get::<DateTime<Utc>>("", "created_at")
                    .expect("created_at"),
            )
        })
    }

    /// An outbox group's identity is minted once - 16 lowercase hex digits and
    /// the moment it was minted - and every later read, a restart included,
    /// gets the same one (feature 0005 §2.5).
    #[tokio::test]
    async fn an_outbox_identity_is_minted_once_and_reused() {
        let scratch = test_db().await;
        let (db, dsn) = (Arc::clone(scratch.db()), scratch.dsn().to_owned());
        let storage = Storage::new(
            Arc::clone(&db),
            Arc::new(TypesRegistrySpecificationManager::new(Arc::clone(&db))),
            TEST_INSTANCE_ID,
        );

        let before = Utc::now();
        let minted = storage.outbox_identity("default").await.expect("minted");
        let after = Utc::now();
        let (name, outbox_id, created_at) = identity_row(&dsn, "default")
            .await
            .expect("the row is stored");
        assert_eq!(
            (name.as_str(), outbox_id.as_str()),
            ("default", minted.to_string().as_str())
        );
        assert_eq!(
            crate::domain::outbox::OutboxId::parse(&outbox_id),
            Some(minted)
        );
        assert!(
            before <= created_at && created_at <= after,
            "minted during the call: {before} <= {created_at} <= {after}"
        );

        let restarted = Storage::new(
            Arc::clone(&db),
            Arc::new(TypesRegistrySpecificationManager::new(Arc::clone(&db))),
            TEST_INSTANCE_ID,
        );
        assert_eq!(
            restarted.outbox_identity("default").await.expect("read"),
            minted
        );
        assert_eq!(
            identity_row(&dsn, "default").await,
            Some((name, outbox_id, created_at)),
            "a second read writes nothing"
        );
    }

    /// A stored identity becomes the high half of every fence key, so one of
    /// a shape this gear never writes is refused rather than reinterpreted.
    #[tokio::test]
    async fn a_malformed_stored_outbox_identity_is_refused() {
        use sea_orm::ConnectionTrait;
        use sea_orm::sea_query::{Alias, Query};

        let scratch = test_db().await;
        let (db, dsn) = (Arc::clone(scratch.db()), scratch.dsn().to_owned());
        let conn = sea_orm::Database::connect(&dsn).await.expect("connect");
        conn.execute(
            Query::insert()
                .into_table(Alias::new("event_broker__outbox_identity"))
                .columns([
                    Alias::new("name"),
                    Alias::new("outbox_id"),
                    Alias::new("created_at"),
                ])
                .values_panic([
                    "default".into(),
                    "0123456789ABCDEF".into(),
                    DateTime::parse_from_rfc3339("2026-10-28T00:00:00.000Z")
                        .expect("RFC 3339")
                        .with_timezone(&Utc)
                        .into(),
                ]),
        )
        .await
        .expect("seed a malformed row");
        drop(conn);
        let storage = Storage::new(
            Arc::clone(&db),
            Arc::new(TypesRegistrySpecificationManager::new(Arc::clone(&db))),
            TEST_INSTANCE_ID,
        );

        let err = storage
            .outbox_identity("default")
            .await
            .expect_err("not an identity this gear wrote");
        assert_eq!(
            err.to_string(),
            DomainError::Internal(
                "ingest outbox identity for 'default' is not 16 lowercase hex digits".to_owned()
            )
            .to_string()
        );
    }
}

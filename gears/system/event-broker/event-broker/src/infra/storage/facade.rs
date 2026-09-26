//! `Storage`: the real (`SQLite` + `ClusterCacheV1`) implementation of every
//! `domain::repo` trait. Topics and events have none - topics go through
//! `SpecificationManager` only, events straight to the backend
//! (eb-single-process-implementation D1/D3). One struct implements every
//! trait, which is safe because their method names are deliberately distinct
//! (`domain/repo.rs`'s doc comment: identical names across traits in scope on
//! one type are ambiguous at the call site).
//!
//! Namespace-to-engine split (design.md D2, corrected mid-design against
//! DESIGN.md's own "why subscription/group state is ephemeral" invariant):
//! routing markers -> `ClusterCacheV1` (subscriptions themselves are not
//! stored here - they live in `ConsumerGroupCoordinator`); `cursor`/
//! `consumer_group`/`producer_state` -> `SQLite` (durable). `cursor` and
//! `producer_sequence` denormalize `tenant_id` from their owning
//! `consumer_group`/`producer` row at write time, since neither has an
//! independent tenant of its own.

use crate::domain::model::Sequence;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use cluster_sdk::ClusterCacheV1;
use cluster_sdk::cache::{PutRequest, Ttl};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use toolkit_db::DBProvider;
use toolkit_db::outbox::Outbox;
use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_gts::GtsInstanceId;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::idempotency::{IdempotencyGuard, ProducerIdempotencyOutcome, PublishEnqueue};
use crate::domain::ingest::{
    ProducerCursors, ProducerMode, ProducerPartitionCursor, ProducerRecord, ProducerRegistration,
    ProducerRegistry, ProducerResetScope, ProducerTopicCursors,
};
use crate::domain::model::{ConsumerGroup, ConsumerGroupKind, Cursor};
use crate::domain::notify::{DeliveryNotifier, NOTIFICATION_PREFIX};
use crate::domain::repo::{ConsumerGroupRepo, CursorRepo, RoutingMarkers};
use crate::domain::specification::SpecificationManager;
use crate::infra::storage::entity::{consumer_group, cursor, producer, producer_sequence};
use crate::infra::storage::error::db_err_from_scope;

/// Subscription id -> its consumer group.
fn subscription_marker_key(id: Uuid) -> String {
    format!("subscription-group/{id}")
}

/// Consumer group -> the delivery instance that owns it (`DESIGN.md`'s
/// `evbk.group.endpoint:{consumer_group}` routing cache).
///
/// Keyed by the group's deterministic UUID, not its id: a GTS id's `.` and `~`
/// are outside the cache's key alphabet. The same encoding
/// `domain::notify::notification_key` uses for a topic.
fn group_marker_key(group: &GtsInstanceId) -> Result<String, DomainError> {
    let uuid = gts::GtsId::try_new(group.as_ref())
        .map_err(|e| DomainError::Internal(format!("consumer group id is not a GTS id: {e}")))?
        .to_uuid();
    Ok(format!("group-owner/{uuid}"))
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
    /// `None` until `EventBrokerModule::serve()` resolves it. `ClusterGear`
    /// (a `RunnableCapability`) only registers its backends into the
    /// `ClientHub` during the platform's *start* phase - which runs after
    /// *every* gear's `init()` (`host_runtime.rs`'s phase order: `init` ->
    /// ... -> REST -> start). `Storage` is built in `init()` (needed there
    /// for `spec_manager`/`db` wiring), so at that point
    /// `EventBrokerCluster::resolve()` would always fail - discovered by
    /// actually booting the standalone binary, not by any test (every
    /// existing test wires the cluster cache directly via
    /// `standalone_event_broker_cluster()`, bypassing the real gear
    /// lifecycle entirely). Set once via [`Self::set_cache`], called from
    /// `serve()` after the start phase has begun (so `cluster`'s own
    /// `start()`, which the topo-sorted dep order runs first, has already
    /// completed). A request can still arrive before that: the listener
    /// accepts traffic as soon as the start phase *spawns* `serve()`, so
    /// every reader goes through [`Self::cache`] and reports an unavailable
    /// storage path rather than assuming the wiring is done.
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
        }
    }

    /// Wires the routing markers' `ClusterCacheV1` in once
    /// `EventBrokerModule::serve()` has resolved it.
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
            // Same reason the publish path logs its refusal: without a line
            // here, a request served in the startup window leaves no server-
            // side trace of why it was turned away.
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
    /// anonymous-vs-named tenant-visibility partitioning
    /// (`eb-tenant-isolation-fix`) into the SQL `WHERE` clause, since that
    /// logic - not a plain tenant-scoped `AccessScope` - decides which rows
    /// a caller may see; that's a larger, separate piece of work than this
    /// change's storage-layer scope. What *did* change: this now queries a
    /// real `SQLite` table instead of an in-memory `HashMap`, resolving the
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

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), DomainError> {
        let topic_id = self.spec_manager.resolve_topic_id(&cursor.topic).await?;
        let conn = self.db.conn()?;

        let update_result = crate::infra::storage::entity::cursor::Entity::update_many()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(
                        crate::infra::storage::entity::cursor::Column::ConsumerGroup
                            .eq(cursor.consumer_group.as_ref()),
                    )
                    .add(crate::infra::storage::entity::cursor::Column::TopicId.eq(topic_id))
                    .add(
                        crate::infra::storage::entity::cursor::Column::Partition
                            .eq(cursor.partition),
                    ),
            )
            .col_expr(
                crate::infra::storage::entity::cursor::Column::Offset,
                Expr::value(cursor.offset.as_i64()),
            )
            .col_expr(
                crate::infra::storage::entity::cursor::Column::UpdatedAt,
                Expr::value(Utc::now()),
            )
            .exec(&conn)
            .await?;

        if update_result.rows_affected > 0 {
            return Ok(());
        }

        // First write for this (consumer_group, topic, partition) - resolve
        // the owning consumer_group's tenant to denormalize onto the row
        // (decision log entry 28).
        let group = consumer_group::Entity::find_by_id(cursor.consumer_group.as_ref().to_owned())
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ConsumerGroupNotFound,
                message: format!(
                    "consumer group '{}' is not registered",
                    cursor.consumer_group
                ),
                resource: cursor.consumer_group.to_string(),
            })?;

        let am = crate::infra::storage::entity::cursor::ActiveModel {
            consumer_group: Set(cursor.consumer_group.as_ref().to_owned()),
            topic_id: Set(topic_id),
            partition: Set(cursor.partition),
            tenant_id: Set(group.tenant_id),
            offset: Set(cursor.offset.as_i64()),
            updated_at: Set(Utc::now()),
        };
        secure_insert::<crate::infra::storage::entity::cursor::Entity>(
            am,
            &AccessScope::for_tenant(group.tenant_id),
            &conn,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl IdempotencyGuard for Storage {
    async fn check_and_enqueue(
        &self,
        request: PublishEnqueue,
    ) -> Result<ProducerIdempotencyOutcome, DomainError> {
        // A request must never be able to panic the process, and this one can
        // genuinely arrive before the pipeline exists: the REST listener is up
        // (and `/healthz`, a liveness probe, answers 200) before the gear's
        // start phase runs, and an instance deployed without the ingest role
        // never starts a pipeline at all. Both are retry-or-route-elsewhere
        // conditions for the caller, so they are reported as an unavailable
        // storage path rather than an internal failure.
        let Some(outbox) = self.outbox.get() else {
            // Worth a line even though the caller gets a `503`: this is the
            // one refusal that means the instance was advertised before it
            // could serve, and it is otherwise invisible from the server side.
            tracing::warn!("publish refused: the ingest outbox pipeline is not running yet");
            return Err(DomainError::StorageUnavailable {
                reason: "the ingest outbox pipeline is not running yet".to_owned(),
                source: None,
            });
        };
        let outbox = Arc::clone(outbox);
        let PublishEnqueue {
            chain,
            topic_partition,
            payload,
            payload_type,
        } = request;
        // Every event of one topic partition has to reach the same outbox
        // partition to keep its relative order, and the topic's partition
        // count is unrelated to the outbox's - see `outbox_partition_for`.
        let partition_for_outbox = crate::domain::outbox::outbox_partition_for(topic_partition);

        let (outcome, wake) = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    let outcome = match &chain {
                        None => ProducerIdempotencyOutcome::Accept,
                        Some(c) => {
                            let existing = producer_sequence::Entity::find()
                                .filter(producer_sequence::Column::ProducerId.eq(c.producer_id))
                                .filter(producer_sequence::Column::Topic.eq(c.topic.as_ref()))
                                .filter(producer_sequence::Column::Partition.eq(c.partition))
                                .secure()
                                .scope_with(&AccessScope::allow_all())
                                .one(tx)
                                .await
                                .map_err(|e| db_err_from_scope(&e))?;

                            // The domain owns the sequencing decision; infra only
                            // read the head above and performs the write it
                            // dictates (seed on first publish, advance the head on
                            // accept, nothing on duplicate/violation).
                            let outcome = c.decide(existing.as_ref().map(|row| row.last_sequence));

                            if matches!(outcome, ProducerIdempotencyOutcome::Accept) {
                                match existing {
                                    None => {
                                        let producer_row =
                                            producer::Entity::find_by_id(c.producer_id)
                                                .secure()
                                                .scope_with(&AccessScope::allow_all())
                                                .one(tx)
                                                .await
                                                .map_err(|e| db_err_from_scope(&e))?
                                                .ok_or_else(|| {
                                                    toolkit_db::DbError::Other(anyhow::anyhow!(
                                                        "producer '{}' is not registered",
                                                        c.producer_id
                                                    ))
                                                })?;
                                        let am = producer_sequence::ActiveModel {
                                            producer_id: Set(c.producer_id),
                                            topic: Set(c.topic.as_ref().to_owned()),
                                            partition: Set(c.partition),
                                            tenant_id: Set(producer_row.tenant_id),
                                            last_sequence: Set(c.sequence),
                                            updated_at: Set(Utc::now()),
                                        };
                                        secure_insert::<producer_sequence::Entity>(
                                            am,
                                            &AccessScope::for_tenant(producer_row.tenant_id),
                                            tx,
                                        )
                                        .await
                                        .map_err(|e| db_err_from_scope(&e))?;
                                    }
                                    Some(_) => {
                                        producer_sequence::Entity::update_many()
                                            .secure()
                                            .scope_with(&AccessScope::allow_all())
                                            .filter(
                                                Condition::all()
                                                    .add(
                                                        producer_sequence::Column::ProducerId
                                                            .eq(c.producer_id),
                                                    )
                                                    .add(
                                                        producer_sequence::Column::Topic
                                                            .eq(c.topic.as_ref()),
                                                    )
                                                    .add(
                                                        producer_sequence::Column::Partition
                                                            .eq(c.partition),
                                                    ),
                                            )
                                            .col_expr(
                                                producer_sequence::Column::LastSequence,
                                                Expr::value(c.sequence),
                                            )
                                            .col_expr(
                                                producer_sequence::Column::UpdatedAt,
                                                Expr::value(Utc::now()),
                                            )
                                            .exec(tx)
                                            .await
                                            .map_err(|e| db_err_from_scope(&e))?;
                                    }
                                }
                            }

                            outcome
                        }
                    };

                    let wake = if matches!(outcome, ProducerIdempotencyOutcome::Accept) {
                        let record = toolkit_db::outbox::Record::to(
                            crate::domain::outbox::INGEST_QUEUE_NAME,
                            partition_for_outbox,
                        )
                        .payload(payload, &payload_type)
                        .build()
                        .map_err(|e| {
                            toolkit_db::DbError::Other(anyhow::anyhow!("outbox enqueue: {e}"))
                        })?;
                        outbox.enqueue(tx, record).await.map_err(|e| {
                            toolkit_db::DbError::Other(anyhow::anyhow!("outbox enqueue: {e}"))
                        })?
                    } else {
                        toolkit_db::outbox::Wake::empty()
                    };

                    Ok((outcome, wake))
                })
            })
            .await?;
        // Fired here, never inside the transaction: marking a partition dirty
        // before its row is durable lets a sequencer claim the partition, find
        // nothing, and clear the flag before the commit lands, leaving the row
        // to the cold reconciler. A rollback propagates above instead, so the
        // wake that would wake a sequencer for rows nobody wrote is dropped
        // with it. Empty on a duplicate or a violation, where `fire` is a
        // no-op.
        wake.fire();
        Ok(outcome)
    }
}

#[async_trait]
impl DeliveryNotifier for Storage {
    async fn wait_for_notification(&self, timeout: std::time::Duration) {
        // One shared deadline for both steps (subscribing, then waiting for
        // the first event) - `wait_for_notification`'s contract is "wait up
        // to `timeout` total", not up to `timeout` per step.
        let deadline = tokio::time::Instant::now() + timeout;
        let Ok(cache) = self.cache() else {
            // Same degradation the trait documents for a backend failure: the
            // delivery loop's own re-query decides what is new, so a missed
            // wake costs one iteration. Returning early instead would spin it.
            tokio::time::sleep_until(deadline).await;
            return;
        };
        let mut watch = match tokio::time::timeout_at(
            deadline,
            cache.watch_prefix(NOTIFICATION_PREFIX),
        )
        .await
        {
            Ok(Ok(watch)) => watch,
            Ok(Err(err)) => {
                tracing::warn!(
                    ?err,
                    "notification watch_prefix failed; falling back to timeout"
                );
                return;
            }
            Err(_elapsed) => return,
        };
        // A `Lagged`/`Reset` event still just means "something changed, go
        // re-check", same as a real `Event`, and an elapsed deadline just
        // means "nothing changed in time" - either way there's nothing
        // further to do with the result.
        match tokio::time::timeout_at(deadline, watch.recv()).await {
            Ok(_) | Err(_) => {}
        }
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

        let mut by_topic: std::collections::HashMap<String, Vec<ProducerPartitionCursor>> =
            std::collections::HashMap::new();
        for row in sequence_rows {
            by_topic
                .entry(row.topic)
                .or_default()
                .push(ProducerPartitionCursor {
                    partition: row.partition,
                    last_sequence: row.last_sequence,
                });
        }

        Ok(Some(ProducerCursors {
            producer_id,
            client_agent: producer_row.client_agent,
            topics: by_topic
                .into_iter()
                .map(|(topic, partitions)| {
                    Ok(ProducerTopicCursors {
                        topic: parse_gts_id(&topic)?,
                        partitions,
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
        if let ProducerResetScope::TopicPartition { topic, partition } = scope {
            filter = filter
                .add(producer_sequence::Column::Topic.eq(topic.as_str()))
                .add(producer_sequence::Column::Partition.eq(*partition));
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
    /// Delete a producer's registration row (`evbk_producer`), cascading to its
    /// `evbk_producer_state` rows. This is the deletion the registration Reaper
    /// performs on age-out (`DESIGN.md:1165`; the worker itself is a future
    /// ticket), and the only path that makes an existing `producer_id` resolve
    /// to nothing - after it, the next publish naming that id is a
    /// `404 ProducerNotFound`. There is no client-facing deregister, so this is
    /// reached only from the reaper and from `test_support` fault injection.
    ///
    /// # Errors
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

    async fn mark_group(&self, group: &GtsInstanceId) -> Result<(), DomainError> {
        self.cache()?
            .put(PutRequest {
                key: &group_marker_key(group)?,
                value: self.instance_id.to_string().as_bytes(),
                ttl: Ttl::Indefinite,
            })
            .await?;
        Ok(())
    }

    async fn unmark_group(&self, group: &GtsInstanceId) -> Result<(), DomainError> {
        self.cache()?.delete(&group_marker_key(group)?).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage, Partitions};
    use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

    use super::*;

    const TEST_INSTANCE_ID: Uuid = Uuid::from_u128(0x5eed);
    use crate::domain::idempotency::ProducerChainCheck;
    use crate::domain::model::ConsumerGroupKind;
    use crate::infra::specification::TypesRegistrySpecificationManager;
    use crate::infra::storage::migrations::Migrator;

    const TOPIC_ID: &str = "gts.cf.core.events.topic.v1~example.eb.storage.topic.v1";

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
    /// The DSN comes back so a test can open its own second connection: the
    /// outbox's bookkeeping tables are not reachable through `DBProvider`
    /// (`toolkit-db` deliberately hands out no raw `SeaORM` executor), and a
    /// test that needs to see where a row was routed has to read them.
    async fn test_db() -> (Arc<DBProvider<toolkit_db::DbError>>, String) {
        let mut path = std::env::temp_dir();
        path.push(format!("cf-eb-storage-test-{}.db", Uuid::now_v7().simple()));
        let mut file = path.to_string_lossy().replace('\\', "/");
        if !file.starts_with('/') {
            file.insert(0, '/');
        }
        let dsn = format!("sqlite://{file}?mode=rwc");
        let opts = toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = toolkit_db::connect_db(&dsn, opts)
            .await
            .expect("connect sqlite");
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            <Migrator as sea_orm_migration::MigratorTrait>::migrations(),
        )
        .await
        .expect("migrations");
        (Arc::new(DBProvider::new(db)), dsn)
    }

    /// A `Storage` wired against a fresh temp-file `SQLite` DB and a real
    /// (`standalone`-provider-backed) `ClusterCacheV1`, with one topic
    /// (`TOPIC_ID`) already bulk-loaded into `SpecificationManager` so
    /// `CursorRepo` tests can resolve it.
    async fn test_storage() -> Storage {
        test_storage_with_db().await.0
    }

    /// As [`test_storage`], but hands back the `DBProvider` too, so a test can
    /// read the outbox's own bookkeeping tables directly.
    async fn test_storage_with_db() -> (Storage, String) {
        let (db, dsn) = test_db().await;
        let instance = make_test_instance(
            TOPIC_ID,
            serde_json::json!({
                "id": TOPIC_ID,
                "description": "a topic this test keeps cursors for",
            }),
        );
        let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
            Arc::new(MockTypesRegistryClient::new().with_instances(vec![instance]));
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
            .queue(crate::domain::outbox::INGEST_QUEUE_NAME, Partitions::of(4))
            .leased(NoopLeasedHandler)
            .start()
            .await
            .expect("outbox start");
        storage.set_outbox(Arc::clone(handle.outbox()));

        (storage, dsn)
    }

    /// How many rows each registered ingest outbox partition holds, ascending
    /// by partition. A partition at `0` never received anything.
    ///
    /// Read from the staging table rather than from
    /// `toolkit_outbox_partitions.sequence`, because these tests deliberately
    /// drop the pipeline handle (see [`test_storage_with_db`]) and so the
    /// sequencer that fills that column never runs. Enqueue's own routing
    /// decision is already final here: the row names its partition the moment
    /// it is staged.
    async fn outbox_partition_row_counts(dsn: &str) -> Vec<(i32, i64)> {
        use sea_orm::{ConnectionTrait, Statement};

        let conn = sea_orm::Database::connect(dsn).await.expect("connect");
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT p.partition AS partition, COUNT(i.id) AS rows_staged \
                 FROM toolkit_outbox_partitions AS p \
                 LEFT JOIN toolkit_outbox_incoming AS i ON i.partition_id = p.id \
                 WHERE p.queue = ? GROUP BY p.partition ORDER BY p.partition",
                [crate::domain::outbox::INGEST_QUEUE_NAME.into()],
            ))
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

    /// Stateless publishes - the common case, no producer `meta` at all - used
    /// to be enqueued onto outbox partition `0` unconditionally, leaving every
    /// other sequencer/processor slot permanently idle.
    #[tokio::test]
    async fn stateless_publishes_reach_every_outbox_partition() {
        let (storage, dsn) = test_storage_with_db().await;

        // One publish per topic partition of an eight-partition topic. Eight
        // is deliberately more than the outbox has, so this also covers the
        // partitions that used to address slots that do not exist.
        for topic_partition in 0..8 {
            let outcome = storage
                .check_and_enqueue(
                    PublishEnqueue::builder(
                        topic_partition,
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
            vec![(0, 2), (1, 2), (2, 2), (3, 2)],
            "eight topic partitions over a four-partition outbox must land two \
             apiece, not eight on partition 0"
        );
    }

    /// A topic with more partitions than the outbox has used to hand
    /// `toolkit-db` a partition index it had never registered, which failed
    /// the whole enqueue transaction and rejected the publish.
    #[tokio::test]
    async fn chained_publish_beyond_the_outbox_partition_count_is_accepted() {
        let (storage, dsn) = test_storage_with_db().await;
        let owner = Uuid::new_v4();
        let registration = storage
            .register(
                owner,
                Uuid::new_v4(),
                ProducerMode::Chained,
                "agent".to_owned(),
            )
            .await
            .expect("register must succeed");
        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();

        // Partition 7 of an eight-partition topic: past the outbox's four.
        let outcome = storage
            .check_and_enqueue(
                PublishEnqueue::builder(7, b"payload".to_vec(), "application/json")
                    .chain(ProducerChainCheck {
                        producer_id: registration.id,
                        topic,
                        partition: 7,
                        previous: 0,
                        sequence: 1,
                        mode: ProducerMode::Chained,
                    })
                    .build(),
            )
            .await
            .expect("a chained publish past the outbox partition count must be accepted");
        assert_eq!(outcome, ProducerIdempotencyOutcome::Accept);

        assert_eq!(
            outbox_partition_row_counts(&dsn).await,
            vec![(0, 0), (1, 0), (2, 0), (3, 1)],
            "topic partition 7 must land on outbox partition 3"
        );
    }

    /// A publish can genuinely arrive before the outbox pipeline exists - the
    /// REST listener is up before the gear's start phase runs - and used to
    /// panic the request task. It must be a retryable answer instead.
    #[tokio::test]
    async fn publish_before_the_pipeline_starts_is_reported_as_unavailable() {
        let (db, _dsn) = test_db().await;
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
        let (db, _dsn) = test_db().await;
        let spec_manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
        // No `set_cache`: the state `Storage` is in between `init()` and the
        // point `serve()` resolves the cluster.
        let storage = Storage::new(db, Arc::new(spec_manager), TEST_INSTANCE_ID);

        let err = storage
            .mark_subscription(
                Uuid::now_v7(),
                &GtsInstanceId::try_new(
                    "gts.cf.core.events.consumer_group.v1~example.eb.storage.group.v1",
                )
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
        let storage = test_storage().await;
        let tenant_id = Uuid::new_v4();
        let group = test_consumer_group(
            "gts.cf.core.events.consumer_group.v1~example.eb.storage.cg1.v1",
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
        let storage = test_storage().await;
        let id = Uuid::new_v4();
        let group = GtsInstanceId::try_new(
            "gts.cf.core.events.consumer_group.v1~example.eb.storage.cg2.v1",
        )
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
        let storage = test_storage().await;
        let tenant_id = Uuid::new_v4();
        let group = test_consumer_group(
            "gts.cf.core.events.consumer_group.v1~example.eb.storage.cg3.v1",
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
            .put_cursor(&cursor)
            .await
            .expect("put_cursor must succeed");

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
            .put_cursor(&updated)
            .await
            .expect("put_cursor update must succeed");
        let found = storage
            .find_cursor(&group.id, &topic, 0)
            .await
            .expect("find_cursor must succeed")
            .expect("cursor must exist");
        assert_eq!(
            found.offset,
            Sequence::assigned(99),
            "second put_cursor must update, not duplicate"
        );
    }

    /// A group marker names this instance as the group's owner.
    #[tokio::test]
    async fn group_marker_names_this_instance() {
        let storage = test_storage().await;
        let group = GtsInstanceId::try_new(
            "gts.cf.core.events.consumer_group.v1~example.eb.storage.cg4.v1",
        )
        .unwrap();
        let key = group_marker_key(&group).expect("a consumer group id is a GTS id");

        storage.mark_group(&group).await.expect("mark must succeed");
        let entry = storage
            .cache()
            .expect("the test wired the cache")
            .get(&key)
            .await
            .expect("get")
            .expect("the marker must exist");
        assert_eq!(entry.value, TEST_INSTANCE_ID.to_string().as_bytes());

        storage
            .unmark_group(&group)
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
    async fn producer_register_and_idempotency_round_trip() {
        let storage = test_storage().await;
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
        assert_eq!(record.mode, ProducerMode::Chained);

        let topic = GtsInstanceId::try_new(TOPIC_ID).unwrap();
        let enqueue = |previous, sequence| {
            PublishEnqueue::builder(0, b"payload".to_vec(), "application/json")
                .chain(ProducerChainCheck {
                    producer_id: registration.id,
                    topic: topic.clone(),
                    partition: 0,
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
            ProducerIdempotencyOutcome::SequenceViolation { last_sequence: 10 }
        );

        let cursors = storage
            .cursors(registration.id)
            .await
            .expect("cursors lookup must succeed")
            .expect("producer must exist");
        assert_eq!(cursors.topics.len(), 1);
        assert_eq!(cursors.topics[0].partitions[0].last_sequence, 10);

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
}

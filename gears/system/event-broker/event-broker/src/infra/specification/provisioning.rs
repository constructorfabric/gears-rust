//! The specification manager's provisioning state machine (feature 0006
//! §3.1): one record per topic in `event_broker__topic_binding`, written only
//! here, and the `provision` calls that move it from `provisioning` to
//! `provisioned`.
//!
//! ```text
//!               insert wins                       provision -> Ready
//!  absent ────────────────────▶ provisioning ────────────────────────▶ provisioned
//!                                │    ▲
//!    Pending, LogConflict, or    │    │ no instance acted for attempt_timeout:
//!    live attempt continued      └────┘ replaced by CAS, new log id
//! ```
//!
//! Every comparison of `attempt_at` and every refresh of it uses the
//! database's clock, so clock skew between ingest hosts never decides whether
//! an attempt is abandoned.
//!
//! A `provisioned` topic is provisioned again only when its resolved
//! retention differs from the retention its record holds, or when its record
//! is unconfirmed, and then with a `Held` claim that never creates the log
//! (feature 0006 §4.1). The record is marked unconfirmed before the call and
//! confirmed only by the answer's compare-and-set, so a call dropped
//! mid-flight, one whose answer is not seen and one whose record write loses
//! all leave it unconfirmed, and instances that resolved different retentions
//! during a rollout converge once they resolve the same one.

use std::collections::HashMap;

use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{
    Backend, BackendError, LogClaim, LogId, Provisioning, Replacing, Retention, TopicSpec,
};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter, Set};
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_gts::GtsInstanceId;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{TypesRegistrySpecificationManager, deserialize_row};
use crate::domain::backend::BackendResolver;
use crate::domain::error::DomainError;
use crate::domain::model::Topic;
use crate::domain::specification::{ProvisioningLimits, TopicState};
use crate::infra::storage::entity::spec_cache::SpecKind;
use crate::infra::storage::entity::topic_binding::{self, Column};

const PROVISIONING: &str = "provisioning";
const PROVISIONED: &str = "provisioned";

/// The database's clock `seconds_ago` seconds back, as SQL producing the
/// 24-character RFC 3339 text `attempt_at` holds (`YYYY-MM-DDTHH:MM:SS.sssZ`).
/// Every engine renders the same width, so `attempt_at` compares as text.
fn db_clock(backend: DbBackend, seconds_ago: u64) -> Result<String, DomainError> {
    match backend {
        DbBackend::Sqlite => Ok(format!(
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{seconds_ago} seconds')"
        )),
        DbBackend::Postgres => Ok(format!(
            "to_char((now() - interval '{seconds_ago} seconds') AT TIME ZONE 'UTC', \
             'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        )),
        // `%f` is six digits of microseconds; at precision 3 the last three
        // are zeros, so the first 23 characters end in the milliseconds.
        DbBackend::MySql => Ok(format!(
            "CONCAT(LEFT(DATE_FORMAT(UTC_TIMESTAMP(3) - INTERVAL {seconds_ago} SECOND, \
             '%Y-%m-%dT%H:%i:%s.%f'), 23), 'Z')"
        )),
        other => Err(DomainError::Internal(format!(
            "topic provisioning has no clock for database backend {other:?}"
        ))),
    }
}

impl TypesRegistrySpecificationManager {
    pub(super) async fn read_topic_state(
        &self,
        id: &GtsInstanceId,
    ) -> Result<Option<TopicState>, DomainError> {
        self.binding(binding_key(id))
            .await?
            .as_ref()
            .map(state_of)
            .transpose()
    }

    pub(super) async fn provisioned_partitions(&self) -> Result<HashMap<String, i64>, DomainError> {
        let conn = self.db.conn()?;
        let rows = topic_binding::Entity::find()
            .filter(Column::State.eq(PROVISIONED))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.partitions.map(|partitions| (row.topic_id, partitions)))
            .collect())
    }

    /// One pass over every registered topic; the number left unprovisioned.
    pub(super) async fn provision_all(
        &self,
        backends: &dyn BackendResolver,
        limits: ProvisioningLimits,
    ) -> usize {
        let topics = self.resolved_topics().await.unwrap_or_else(|err| {
            tracing::warn!(%err, "spec cache list failed while provisioning topics");
            Vec::new()
        });
        let mut pending = 0;
        for topic in topics {
            if let Err(err) = self.provision_one(&topic, backends, limits).await {
                tracing::warn!(topic = %topic.id, %err, "topic provisioning pass failed; retried on the next pass");
            }
            if !matches!(
                self.read_topic_state(&topic.id).await,
                Ok(Some(TopicState::Provisioned { .. }))
            ) {
                pending += 1;
            }
        }
        pending
    }

    /// Every registered topic with the settings configuration resolved for
    /// it, not the partition count its backend holds: provisioning compares
    /// the two (feature 0006 §4.1), and listing lays the backend's count over
    /// it.
    pub(super) async fn resolved_topics(&self) -> Result<Vec<Topic>, DomainError> {
        Ok(self
            .list_rows(SpecKind::Topic)
            .await?
            .iter()
            .filter_map(deserialize_row)
            .collect())
    }

    async fn provision_one(
        &self,
        topic: &Topic,
        backends: &dyn BackendResolver,
        limits: ProvisioningLimits,
    ) -> Result<(), DomainError> {
        let spec = topic_spec(topic);
        let attempt = Attempt {
            topic,
            backend: backends.resolve(topic),
            retention: StoredRetention::of(&spec.retention),
            spec,
            key: binding_key(&topic.id),
            limits,
        };
        match self.binding(attempt.key).await? {
            None => self.start(&attempt).await,
            Some(row) if row.state == PROVISIONED => self.refresh(&attempt, &row).await,
            Some(row) => {
                self.continue_or_replace(&attempt, LogId::from_uuid(row.log_id))
                    .await
            }
        }
    }

    /// A topic with no record: insert an attempt and drive it, unless another
    /// instance's insert won.
    async fn start(&self, attempt: &Attempt<'_>) -> Result<(), DomainError> {
        let log = LogId::new_v4();
        if !self.insert_attempt(attempt.topic, attempt.key, log).await? {
            return Ok(());
        }
        self.drive(
            attempt,
            LogClaim {
                replacing: Replacing::Nothing,
                new: log,
            },
        )
        .await
    }

    /// A provisioned topic. A partition count that differs from the recorded
    /// one is logged on every pass, and the recorded count stays in force.
    /// Only a resolved retention that differs from the recorded one, or any
    /// retention on an unconfirmed record, reaches the backend, through
    /// [`Self::re_provision`]; until a call records and confirms it, every
    /// pass calls again. The record is marked unconfirmed before the call, so
    /// a call dropped mid-flight leaves it unconfirmed; a pass whose mark finds
    /// the record written since its read leaves the topic to the next pass.
    async fn refresh(
        &self,
        attempt: &Attempt<'_>,
        row: &topic_binding::Model,
    ) -> Result<(), DomainError> {
        if row.partitions != Some(i64::from(attempt.spec.partitions)) {
            log_partition_mismatch(attempt.topic, attempt.spec.partitions, row.partitions);
        }
        if row.retention_confirmed && StoredRetention::recorded(row) == attempt.retention {
            return Ok(());
        }
        let partitions = row
            .partitions
            .and_then(|partitions| u32::try_from(partitions).ok())
            .ok_or_else(|| {
                DomainError::Internal(format!(
                    "topic binding holds partition count {:?}",
                    row.partitions
                ))
            })?;
        let log = LogId::from_uuid(row.log_id);
        if !self
            .claim_re_provision(attempt.key, log, row.retention_version)
            .await?
        {
            tracing::debug!(topic = %attempt.topic.id, log_id = %log, "topic record written since it was read; re-provisioned on a later pass");
            return Ok(());
        }
        self.re_provision(attempt, log, partitions, row.retention_version + 1)
            .await
    }

    /// Hands a provisioned topic its resolved retention under its log id and
    /// its recorded partition count, as a `Held` claim: the backend applies
    /// the settings to the log it holds and never creates one. Records the
    /// retention on `Ready` if the record is still at `version`, the
    /// `retention_version` [`Self::claim_re_provision`] wrote. `Pending`, a
    /// timeout and any other failure mark the record unconfirmed again, so a
    /// write that read before the mark cannot confirm the record over a call
    /// whose answer was not seen. A backend holding no log for the topic (it
    /// was routed elsewhere since) answers `NotProvisioned`, and one holding
    /// another log id for it answers `LogConflict`; both refusals leave the
    /// record unconfirmed, break the single-holder invariant and are logged at
    /// error level on every pass.
    async fn re_provision(
        &self,
        attempt: &Attempt<'_>,
        log: LogId,
        partitions: u32,
        version: i64,
    ) -> Result<(), DomainError> {
        let spec = TopicSpec {
            partitions,
            ..attempt.spec.clone()
        };
        let claim = LogClaim {
            replacing: Replacing::Held,
            new: log,
        };
        let problem = match attempt.call(&spec, &claim).await {
            Ok(Provisioning::Ready { .. }) => {
                return self.retention_re_provisioned(attempt, log, version).await;
            }
            Ok(Provisioning::Pending) => "the backend has not finished".to_owned(),
            Err(CallProblem::Backend(BackendError::NotProvisioned { .. })) => {
                log_single_holder_refusal(attempt.topic, log, SingleHolderRefusal::NoLog);
                return Ok(());
            }
            Err(CallProblem::Backend(BackendError::LogConflict { .. })) => {
                log_single_holder_refusal(attempt.topic, log, SingleHolderRefusal::AnotherLog);
                return Ok(());
            }
            Err(problem) => problem.to_string(),
        };
        self.mark_retention_unconfirmed(attempt.key, log).await?;
        tracing::warn!(topic = %attempt.topic.id, %problem, "re-provisioning a provisioned topic failed; retried on the next pass");
        Ok(())
    }

    async fn retention_re_provisioned(
        &self,
        attempt: &Attempt<'_>,
        log: LogId,
        version: i64,
    ) -> Result<(), DomainError> {
        if self
            .record_retention(attempt.key, log, version, attempt.retention)
            .await?
        {
            tracing::info!(topic = %attempt.topic.id, log_id = %log, "topic retention re-provisioned");
        } else if self.mark_retention_unconfirmed(attempt.key, log).await? {
            tracing::info!(topic = %attempt.topic.id, log_id = %log, "topic retention re-provisioned concurrently; confirmed on a later pass");
        }
        Ok(())
    }

    /// A live attempt is continued under its log id; one nobody acted on for
    /// `attempt_timeout` is replaced by a new log id, unless someone replaced
    /// it first.
    async fn continue_or_replace(
        &self,
        attempt: &Attempt<'_>,
        log: LogId,
    ) -> Result<(), DomainError> {
        if !self
            .attempt_older_than(attempt.key, attempt.limits.attempt_timeout)
            .await?
        {
            return self
                .drive(
                    attempt,
                    LogClaim {
                        replacing: Replacing::Log(log),
                        new: log,
                    },
                )
                .await;
        }
        let new = LogId::new_v4();
        if !self.replace_attempt(attempt.key, log, new).await? {
            return Ok(());
        }
        self.drive(
            attempt,
            LogClaim {
                replacing: Replacing::Log(log),
                new,
            },
        )
        .await
    }

    /// Hands the topic's resolved spec to the backend under `claim`. `Pending`,
    /// a timeout and any failure but a `LogConflict` refusal, which applies
    /// nothing, leave the backend free to apply this call later, so when
    /// another instance driving the same attempt has recorded the topic
    /// meanwhile, they mark that record unconfirmed.
    async fn drive(&self, attempt: &Attempt<'_>, claim: LogClaim) -> Result<(), DomainError> {
        match attempt.call(&attempt.spec, &claim).await {
            Ok(Provisioning::Ready { partitions }) => self.ready(attempt, claim, partitions).await,
            Ok(Provisioning::Pending) => {
                self.mark_retention_unconfirmed(attempt.key, claim.new)
                    .await?;
                self.touch(attempt.key, claim.new).await
            }
            Err(CallProblem::Backend(BackendError::LogConflict { .. })) => {
                self.conflict(attempt, claim).await
            }
            Err(problem) => {
                self.mark_retention_unconfirmed(attempt.key, claim.new)
                    .await?;
                tracing::warn!(topic = %attempt.topic.id, %problem, "topic provisioning failed; retried on the next pass");
                Ok(())
            }
        }
    }

    /// The backend holds the log: record it as provisioned with the
    /// partition count the backend reported. When another instance driving
    /// the same attempt recorded it first, the backend may hold this call's
    /// retention over the recorded one, so the record is marked unconfirmed.
    async fn ready(
        &self,
        attempt: &Attempt<'_>,
        claim: LogClaim,
        partitions: u32,
    ) -> Result<(), DomainError> {
        if partitions != attempt.spec.partitions {
            log_partition_mismatch(
                attempt.topic,
                attempt.spec.partitions,
                Some(i64::from(partitions)),
            );
        }
        if self
            .mark_provisioned(attempt.key, claim.new, partitions, attempt.retention)
            .await?
        {
            tracing::info!(topic = %attempt.topic.id, log_id = %claim.new, partitions, "topic provisioned");
        } else if self
            .mark_retention_unconfirmed(attempt.key, claim.new)
            .await?
        {
            tracing::info!(topic = %attempt.topic.id, log_id = %claim.new, "topic provisioned concurrently; its retention is confirmed on a later pass");
        }
        Ok(())
    }

    /// The backend holds another log id: the attempt keeps its id and stays
    /// live, so it is retried rather than replaced (ADR-0008 owns the reaction).
    async fn conflict(&self, attempt: &Attempt<'_>, claim: LogClaim) -> Result<(), DomainError> {
        self.touch(attempt.key, claim.new).await?;
        tracing::error!(
            topic = %attempt.topic.id,
            log_id = %claim.new,
            "topic provisioning refused: the backend holds another log id"
        );
        Ok(())
    }

    async fn binding(&self, key: Uuid) -> Result<Option<topic_binding::Model>, DomainError> {
        let conn = self.db.conn()?;
        Ok(topic_binding::Entity::find_by_id(key)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?)
    }

    /// Starts an attempt for a topic with no record. `false` when another
    /// instance's insert won.
    async fn insert_attempt(
        &self,
        topic: &Topic,
        key: Uuid,
        log: LogId,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn()?;
        let inserted = secure_insert::<topic_binding::Entity>(
            topic_binding::ActiveModel {
                topic_uuid: Set(key),
                topic_id: Set(topic.id.to_string()),
                log_id: Set(log.uuid()),
                state: Set(PROVISIONING.to_owned()),
                // Replaced by the database's clock right below.
                attempt_at: Set(chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string()),
                partitions: Set(None),
                retention_max_age_ms: Set(None),
                retention_max_bytes: Set(None),
                retention_version: Set(0),
                retention_confirmed: Set(false),
            },
            &AccessScope::allow_all(),
            &conn,
        )
        .await;
        if inserted.is_err() {
            if self.binding(key).await?.is_some() {
                return Ok(false);
            }
            inserted?;
        }
        self.touch(key, log).await?;
        Ok(true)
    }

    async fn attempt_older_than(
        &self,
        key: Uuid,
        timeout: std::time::Duration,
    ) -> Result<bool, DomainError> {
        let cutoff = db_clock(self.db.db().backend(), timeout.as_secs())?;
        let conn = self.db.conn()?;
        Ok(topic_binding::Entity::find()
            .filter(
                Condition::all()
                    .add(Column::TopicUuid.eq(key))
                    .add(Expr::col(Column::AttemptAt).lt(Expr::cust(cutoff))),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await?
            .is_some())
    }

    /// Compare-and-set `(old, provisioning)` -> `(new, provisioning)`. `false`
    /// when someone replaced the attempt first.
    async fn replace_attempt(
        &self,
        key: Uuid,
        old: LogId,
        new: LogId,
    ) -> Result<bool, DomainError> {
        self.update_attempt(key, old, |update, now| {
            update
                .col_expr(Column::LogId, Expr::value(new.uuid()))
                .col_expr(Column::AttemptAt, now)
        })
        .await
    }

    /// Compare-and-set `(log, provisioning)` -> `(log, provisioned)` with
    /// `partitions` and a confirmed `retention`. `false` when the attempt was
    /// recorded or replaced first.
    async fn mark_provisioned(
        &self,
        key: Uuid,
        log: LogId,
        partitions: u32,
        retention: StoredRetention,
    ) -> Result<bool, DomainError> {
        self.update_attempt(key, log, |update, now| {
            update
                .col_expr(Column::State, Expr::value(PROVISIONED))
                .col_expr(Column::Partitions, Expr::value(i64::from(partitions)))
                .col_expr(Column::RetentionMaxAgeMs, Expr::value(retention.max_age_ms))
                .col_expr(Column::RetentionMaxBytes, Expr::value(retention.max_bytes))
                .col_expr(
                    Column::RetentionVersion,
                    Expr::col(Column::RetentionVersion).add(1),
                )
                .col_expr(Column::RetentionConfirmed, Expr::value(true))
                .col_expr(Column::AttemptAt, now)
        })
        .await
    }

    /// Compare-and-set the `provisioned` record of `key` under `log` on
    /// `version`, the `retention_version` the caller read: marks it
    /// unconfirmed and bumps the version before a re-provision call. `false`
    /// when another write changed the record since that read.
    async fn claim_re_provision(
        &self,
        key: Uuid,
        log: LogId,
        version: i64,
    ) -> Result<bool, DomainError> {
        self.update_record(key, log, PROVISIONED, |update, _| {
            update
                .col_expr(
                    Column::RetentionVersion,
                    Expr::col(Column::RetentionVersion).add(1),
                )
                .col_expr(Column::RetentionConfirmed, Expr::value(false))
                .filter(Column::RetentionVersion.eq(version))
        })
        .await
    }

    /// Compare-and-set the retention of the `provisioned` record of `key`
    /// under `log` on `version`, the `retention_version` the caller's
    /// [`Self::claim_re_provision`] wrote: records `retention`, bumps the
    /// version and confirms the record. `false` when another write changed the
    /// record since.
    async fn record_retention(
        &self,
        key: Uuid,
        log: LogId,
        version: i64,
        retention: StoredRetention,
    ) -> Result<bool, DomainError> {
        self.update_record(key, log, PROVISIONED, |update, _| {
            update
                .col_expr(Column::RetentionMaxAgeMs, Expr::value(retention.max_age_ms))
                .col_expr(Column::RetentionMaxBytes, Expr::value(retention.max_bytes))
                .col_expr(
                    Column::RetentionVersion,
                    Expr::col(Column::RetentionVersion).add(1),
                )
                .col_expr(Column::RetentionConfirmed, Expr::value(true))
                .filter(Column::RetentionVersion.eq(version))
        })
        .await
    }

    /// Marks the `provisioned` record of `key` under `log` unconfirmed. The
    /// version bump makes every compare-and-set on a version read before the
    /// mark lose, so none of them confirms the record over the mark. `false`
    /// when no record matches `key`, `log` and `provisioned`.
    async fn mark_retention_unconfirmed(&self, key: Uuid, log: LogId) -> Result<bool, DomainError> {
        self.update_record(key, log, PROVISIONED, |update, _| {
            update
                .col_expr(
                    Column::RetentionVersion,
                    Expr::col(Column::RetentionVersion).add(1),
                )
                .col_expr(Column::RetentionConfirmed, Expr::value(false))
        })
        .await
    }

    /// Records that the attempt under `log` acted now.
    async fn touch(&self, key: Uuid, log: LogId) -> Result<(), DomainError> {
        self.update_attempt(key, log, |update, now| {
            update.col_expr(Column::AttemptAt, now)
        })
        .await
        .map(|_| ())
    }

    /// Applies `set` to the `provisioning` record of `key` under `log`; `true`
    /// when it matched. `set` gets the database's clock to stamp `attempt_at`
    /// with.
    async fn update_attempt(
        &self,
        key: Uuid,
        log: LogId,
        set: impl FnOnce(
            sea_orm::UpdateMany<topic_binding::Entity>,
            Expr,
        ) -> sea_orm::UpdateMany<topic_binding::Entity>,
    ) -> Result<bool, DomainError> {
        self.update_record(key, log, PROVISIONING, set).await
    }

    /// Applies `set` to the record of `key` under `log` in `state`; `true`
    /// when it matched. `set` gets the database's clock.
    async fn update_record(
        &self,
        key: Uuid,
        log: LogId,
        state: &str,
        set: impl FnOnce(
            sea_orm::UpdateMany<topic_binding::Entity>,
            Expr,
        ) -> sea_orm::UpdateMany<topic_binding::Entity>,
    ) -> Result<bool, DomainError> {
        let now = Expr::cust(db_clock(self.db.db().backend(), 0)?);
        let conn = self.db.conn()?;
        let result = set(topic_binding::Entity::update_many(), now)
            .filter(
                Condition::all()
                    .add(Column::TopicUuid.eq(key))
                    .add(Column::LogId.eq(log.uuid()))
                    .add(Column::State.eq(state)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await?;
        Ok(result.rows_affected == 1)
    }
}

struct Attempt<'a> {
    topic: &'a Topic,
    backend: std::sync::Arc<dyn Backend>,
    /// `spec.retention` in the units the record stores.
    retention: StoredRetention,
    spec: TopicSpec,
    key: Uuid,
    limits: ProvisioningLimits,
}

/// A retention in the units the binding record stores it in; `None` is
/// unbounded. Retentions are compared in these units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredRetention {
    max_age_ms: Option<i64>,
    max_bytes: Option<i64>,
}

impl StoredRetention {
    /// A bound past what the record's columns hold is stored as `i64::MAX`:
    /// no event lives that long or grows that big, so the saturated bound
    /// limits nothing the original would.
    fn of(retention: &Retention) -> Self {
        Self {
            max_age_ms: retention
                .max_age
                .map(|age| i64::try_from(age.as_millis()).unwrap_or(i64::MAX)),
            max_bytes: retention
                .max_bytes
                .map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX)),
        }
    }

    fn recorded(row: &topic_binding::Model) -> Self {
        Self {
            max_age_ms: row.retention_max_age_ms,
            max_bytes: row.retention_max_bytes,
        }
    }
}

impl Attempt<'_> {
    async fn call(&self, spec: &TopicSpec, claim: &LogClaim) -> Result<Provisioning, CallProblem> {
        tokio::time::timeout(
            self.limits.call_timeout,
            self.backend.provision(spec, claim),
        )
        .await
        .map_err(|_| CallProblem::TimedOut)?
        .map_err(CallProblem::Backend)
    }
}

/// Why a `provision` call produced no answer.
#[derive(Debug)]
enum CallProblem {
    TimedOut,
    Backend(BackendError),
}

impl std::fmt::Display for CallProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("the provision call timed out"),
            Self::Backend(err) => err.fmt(f),
        }
    }
}

/// Why the backend does not hold a provisioned topic's log under its
/// recorded log id.
#[derive(Debug, Clone, Copy)]
enum SingleHolderRefusal {
    /// `NotProvisioned`: the backend holds no log for the topic.
    NoLog,
    /// `LogConflict`: the backend holds another log id for the topic.
    AnotherLog,
}

/// Logs at error level a re-provision the backend refused because it does
/// not hold the topic's log under `log`: at most one backend holds the
/// recorded log id (0006 §4.1), so this needs an operator.
fn log_single_holder_refusal(topic: &Topic, log: LogId, refusal: SingleHolderRefusal) {
    match refusal {
        SingleHolderRefusal::NoLog => tracing::error!(
            topic = %topic.id,
            log_id = %log,
            "re-provisioning a provisioned topic refused: the backend this topic is routed to holds no log for it; retried on the next pass"
        ),
        SingleHolderRefusal::AnotherLog => tracing::error!(
            topic = %topic.id,
            log_id = %log,
            "re-provisioning a provisioned topic refused: the backend holds another log id for it; retried on the next pass"
        ),
    }
}

fn log_partition_mismatch(topic: &Topic, configured: u32, provisioned: Option<i64>) {
    tracing::error!(
        topic = %topic.id,
        configured,
        provisioned = ?provisioned,
        "topic partition count differs from the count its backend holds; the backend's count is in force"
    );
}

fn binding_key(id: &GtsInstanceId) -> Uuid {
    topic_uuid(id)
}

fn state_of(row: &topic_binding::Model) -> Result<TopicState, DomainError> {
    let log = LogId::from_uuid(row.log_id);
    match (row.state.as_str(), row.partitions) {
        (PROVISIONED, Some(partitions)) => Ok(TopicState::Provisioned {
            log,
            partitions: u32::try_from(partitions).map_err(|_| {
                DomainError::Internal(format!("topic binding holds partition count {partitions}"))
            })?,
        }),
        (PROVISIONING, _) => Ok(TopicState::Provisioning { log }),
        (state, _) => Err(DomainError::Internal(format!(
            "topic binding holds unknown state '{state}'"
        ))),
    }
}

fn topic_spec(topic: &Topic) -> TopicSpec {
    let retention = topic.settings.retention().value();
    TopicSpec {
        topic: topic.id.clone(),
        partitions: Ord::max(*topic.settings.partitions().value(), 1).cast_unsigned(),
        retention: Retention {
            max_age: Some(retention.duration),
            max_bytes: retention.size_bytes,
        },
    }
}

#[cfg(test)]
#[path = "provisioning_tests.rs"]
mod provisioning_tests;

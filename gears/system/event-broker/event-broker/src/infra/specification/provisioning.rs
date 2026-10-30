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

use super::TypesRegistrySpecificationManager;
use crate::domain::backend::BackendResolver;
use crate::domain::error::DomainError;
use crate::domain::model::Topic;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager, TopicState};
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
        self.binding(&binding_key(id))
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
        let mut pending = 0;
        for topic in self.list_topics().await {
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

    async fn provision_one(
        &self,
        topic: &Topic,
        backends: &dyn BackendResolver,
        limits: ProvisioningLimits,
    ) -> Result<(), DomainError> {
        let attempt = Attempt {
            topic,
            backend: backends.resolve(topic),
            spec: topic_spec(topic),
            key: binding_key(&topic.id),
            limits,
        };
        match self.binding(&attempt.key).await? {
            None => self.start(&attempt).await,
            Some(row) if row.state == PROVISIONED => {
                self.refresh(&attempt, &row).await;
                Ok(())
            }
            Some(row) => {
                self.continue_or_replace(&attempt, parse_log(&row.log_id)?)
                    .await
            }
        }
    }

    /// A topic with no record: insert an attempt and drive it, unless another
    /// instance's insert won.
    async fn start(&self, attempt: &Attempt<'_>) -> Result<(), DomainError> {
        let log = LogId::new_v4();
        if !self
            .insert_attempt(attempt.topic, &attempt.key, log)
            .await?
        {
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

    /// A provisioned topic: hand the backend its current settings under the
    /// same log id, so it adopts changed retention; nothing else changes.
    async fn refresh(&self, attempt: &Attempt<'_>, row: &topic_binding::Model) {
        if row.partitions != Some(i64::from(attempt.spec.partitions)) {
            log_partition_mismatch(attempt.topic, attempt.spec.partitions, row.partitions);
        }
        let Ok(log) = parse_log(&row.log_id) else {
            tracing::warn!(topic = %attempt.topic.id, "topic binding holds a malformed log id");
            return;
        };
        let claim = LogClaim {
            replacing: Replacing::Log(log),
            new: log,
        };
        if let Err(problem) = attempt.call(&claim).await {
            tracing::warn!(topic = %attempt.topic.id, %problem, "re-provisioning a provisioned topic failed");
        }
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
            .attempt_older_than(&attempt.key, attempt.limits.attempt_timeout)
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
        if !self.replace_attempt(&attempt.key, log, new).await? {
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

    async fn drive(&self, attempt: &Attempt<'_>, claim: LogClaim) -> Result<(), DomainError> {
        match attempt.call(&claim).await {
            Ok(Provisioning::Ready { partitions }) => self.ready(attempt, claim, partitions).await,
            Ok(Provisioning::Pending) => self.touch(&attempt.key, claim.new).await,
            Err(CallProblem::Backend(BackendError::LogConflict { .. })) => {
                self.conflict(attempt, claim).await
            }
            Err(problem) => {
                tracing::warn!(topic = %attempt.topic.id, %problem, "topic provisioning failed; retried on the next pass");
                Ok(())
            }
        }
    }

    /// The backend holds the log: record it as provisioned with the
    /// partition count the backend reported.
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
        self.mark_provisioned(&attempt.key, claim.new, partitions)
            .await?;
        tracing::info!(topic = %attempt.topic.id, log_id = %claim.new, partitions, "topic provisioned");
        Ok(())
    }

    /// The backend holds another log id: the attempt keeps its id and stays
    /// live, so it is retried rather than replaced (ADR-0008 owns the reaction).
    async fn conflict(&self, attempt: &Attempt<'_>, claim: LogClaim) -> Result<(), DomainError> {
        self.touch(&attempt.key, claim.new).await?;
        tracing::error!(
            topic = %attempt.topic.id,
            log_id = %claim.new,
            "topic provisioning refused: the backend holds another log id"
        );
        Ok(())
    }

    async fn binding(&self, key: &str) -> Result<Option<topic_binding::Model>, DomainError> {
        let conn = self.db.conn()?;
        Ok(topic_binding::Entity::find_by_id(key.to_owned())
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
        key: &str,
        log: LogId,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn()?;
        let inserted = secure_insert::<topic_binding::Entity>(
            topic_binding::ActiveModel {
                topic_uuid: Set(key.to_owned()),
                topic_id: Set(topic.id.to_string()),
                log_id: Set(log.to_string()),
                state: Set(PROVISIONING.to_owned()),
                // Replaced by the database's clock right below.
                attempt_at: Set(chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string()),
                partitions: Set(None),
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
        key: &str,
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
        key: &str,
        old: LogId,
        new: LogId,
    ) -> Result<bool, DomainError> {
        self.update_attempt(key, old, |update, now| {
            update
                .col_expr(Column::LogId, Expr::value(new.to_string()))
                .col_expr(Column::AttemptAt, now)
        })
        .await
    }

    async fn mark_provisioned(
        &self,
        key: &str,
        log: LogId,
        partitions: u32,
    ) -> Result<(), DomainError> {
        self.update_attempt(key, log, |update, now| {
            update
                .col_expr(Column::State, Expr::value(PROVISIONED))
                .col_expr(Column::Partitions, Expr::value(i64::from(partitions)))
                .col_expr(Column::AttemptAt, now)
        })
        .await
        .map(|_| ())
    }

    /// Records that the attempt under `log` acted now.
    async fn touch(&self, key: &str, log: LogId) -> Result<(), DomainError> {
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
        key: &str,
        log: LogId,
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
                    .add(Column::LogId.eq(log.to_string()))
                    .add(Column::State.eq(PROVISIONING)),
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
    spec: TopicSpec,
    key: String,
    limits: ProvisioningLimits,
}

impl Attempt<'_> {
    async fn call(&self, claim: &LogClaim) -> Result<Provisioning, CallProblem> {
        tokio::time::timeout(
            self.limits.call_timeout,
            self.backend.provision(&self.spec, claim),
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

fn log_partition_mismatch(topic: &Topic, configured: u32, provisioned: Option<i64>) {
    tracing::error!(
        topic = %topic.id,
        configured,
        provisioned = ?provisioned,
        "topic partition count differs from the count its backend holds; the backend's count is in force"
    );
}

fn binding_key(id: &GtsInstanceId) -> String {
    topic_uuid(id).hyphenated().to_string()
}

fn parse_log(raw: &str) -> Result<LogId, DomainError> {
    Uuid::parse_str(raw)
        .map(LogId::from_uuid)
        .map_err(|e| DomainError::Internal(format!("topic binding holds a malformed log id: {e}")))
}

fn state_of(row: &topic_binding::Model) -> Result<TopicState, DomainError> {
    let log = parse_log(&row.log_id)?;
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

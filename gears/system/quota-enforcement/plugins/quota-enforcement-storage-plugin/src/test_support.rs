//! Test doubles and harness recipes shared by the plugin's test modules.

#![allow(clippy::expect_used)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gts::GtsTypeId;
use quota_enforcement_sdk::testing::quota_draft;
use quota_enforcement_sdk::{
    ActiveQuotaCounts, DeactivateOutcome, EventId, NotificationEvent, NotificationEventKind,
    PageRequest, PageResult, PolicyDraft, PolicyId, PolicySchemaSnapshot, PolicyScope,
    PolicyUpdate, PolicyVersion, PolicyVersionMeta, PolicyVersionState, ProjectionBinding, Quota,
    QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, StorageError, SubjectRef, TenantId,
    TransitionOutcome,
};
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{OutboxHandle, OutboxMessageId};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::ports::{Actor, PolicyStore, QuotaStore, StoreError};
use crate::infra::outbox::{EnqueueError, NotificationEnqueuer, QeOutbox, start_outbox};
use crate::infra::storage::Migrator;

/// The `llm_gateway` user projection the gear's fixtures use.
pub const USER_PROJECTION: &str = "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~";
/// A metric instance id.
pub const METRIC_TOKENS: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_tokens_input.v1";
/// Another metric instance id.
pub const METRIC_REQUESTS: &str = "gts.cf.qe.metric.type.v1~cf.qe.metric.ai_requests.v1";

/// One in-memory `SQLite` database with every plugin migration applied. One
/// connection: a second connection would be a second database.
pub async fn test_db() -> Db {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..ConnectOpts::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("apply storage migrations");
    db
}

/// The outbox pipeline on `db` with the notification queue registered, and
/// a bound handle over it. Stop the handle at the end of the test.
pub async fn bound_outbox(db: &Db) -> (OutboxHandle, Arc<QeOutbox>) {
    let handle = start_outbox(db.clone()).await.expect("start outbox");
    let outbox = Arc::new(QeOutbox::new());
    outbox.bind(Arc::clone(handle.outbox())).expect("bind once");
    (handle, outbox)
}

/// The test tenant.
pub fn tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x00ac_ce55))
}

/// Another tenant.
pub fn other_tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x0bad))
}

/// A scope covering exactly `tenant`.
pub fn scope_for(tenant: TenantId) -> AccessScope {
    AccessScope::for_tenant(tenant.as_uuid())
}

/// The test actor.
pub fn actor() -> Actor {
    Actor {
        subject_id: Uuid::from_u128(0x5eed),
        subject_type: Some("user".to_owned()),
    }
}

/// A user subject.
pub fn user(id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::try_new(USER_PROJECTION).expect("type id"),
        subject_id: id.to_owned(),
    }
}

/// A consumption draft for `tenant` on the tokens metric.
pub fn draft(tenant: TenantId, subject_id: &str, cap: Option<u64>) -> QuotaDraft {
    let mut draft = quota_draft(user(subject_id), cap);
    draft.tenant_id = tenant;
    draft.metric = quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric");
    draft
}

/// A `quota-changed` event for `tenant` with no Quota id yet.
pub fn quota_changed(tenant: TenantId) -> NotificationEvent {
    NotificationEvent {
        event_id: EventId::generate(),
        kind: NotificationEventKind::QuotaChanged,
        scope: quota_enforcement_sdk::NotificationScope::Tenant { tenant_id: tenant },
        quota_id: None,
        policy_id: None,
        subject: None,
        payload: json!({ "change_kind": "created" }),
        emitted_at: OffsetDateTime::now_utc(),
    }
}

/// Probe over the outbox body table: what was enqueued.
mod outbox_body {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "qe_outbox_body")]
    #[secure(no_tenant, no_resource, no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub payload: Vec<u8>,
        pub payload_type: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl sea_orm::ActiveModelBehavior for ActiveModel {}
}

/// One enqueued message.
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct EnqueuedMessage {
    pub payload: Vec<u8>,
    pub payload_type: String,
}

/// Every message in the outbox body table, oldest first.
pub async fn enqueued_messages(db: &Db) -> Vec<EnqueuedMessage> {
    use sea_orm::{QueryOrder, QuerySelect};
    let conn = db.conn().expect("connection");
    outbox_body::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .project_all(&conn, |q| {
            q.select_only()
                .column(outbox_body::Column::Payload)
                .column(outbox_body::Column::PayloadType)
                .order_by_asc(outbox_body::Column::Id)
                .into_model::<EnqueuedMessage>()
        })
        .await
        .expect("read outbox body")
}

/// Rows of a scopable entity under `allow_all`.
pub async fn count_rows<E: EntityTrait + toolkit_db::secure::ScopableEntity>(db: &Db) -> usize {
    let conn = db.conn().expect("connection");
    E::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("read rows")
        .len()
}

/// An enqueuer that fails every call, for atomicity tests.
pub struct FailingEnqueuer;

#[async_trait]
impl NotificationEnqueuer for FailingEnqueuer {
    async fn enqueue_all(
        &self,
        _runner: &(dyn DBRunner + Sync),
        _events: &[NotificationEvent],
    ) -> Result<Vec<OutboxMessageId>, EnqueueError> {
        Err(EnqueueError::Serialize("injected".to_owned()))
    }
}

/// A Quota store double that answers with one configured error, or with
/// empty successes, and records what it was asked.
#[derive(Default)]
pub struct FakeQuotaStore {
    fail: Mutex<Option<StoreError>>,
    actors: Mutex<Vec<Actor>>,
}

impl FakeQuotaStore {
    /// A store that fails every call with `err`.
    pub fn failing(err: StoreError) -> Self {
        Self {
            fail: Mutex::new(Some(err)),
            actors: Mutex::new(Vec::new()),
        }
    }

    /// The actors of every mutation call.
    pub fn actors(&self) -> Vec<Actor> {
        self.actors.lock().expect("lock").clone()
    }

    fn check(&self, actor: Option<&Actor>) -> Result<(), StoreError> {
        if let Some(actor) = actor {
            self.actors.lock().expect("lock").push(actor.clone());
        }
        match self.fail.lock().expect("lock").clone() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl QuotaStore for FakeQuotaStore {
    async fn create_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        _draft: QuotaDraft,
        _events: &[NotificationEvent],
    ) -> Result<QuotaId, StoreError> {
        self.check(Some(actor))?;
        Ok(QuotaId::generate())
    }

    async fn update_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        quota_id: QuotaId,
        _patch: QuotaPatch,
        _events: &[NotificationEvent],
    ) -> Result<Quota, StoreError> {
        self.check(Some(actor))?;
        Err(StoreError::QuotaNotFound { id: quota_id })
    }

    async fn deactivate_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        _quota_id: QuotaId,
        _events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StoreError> {
        self.check(Some(actor))?;
        Ok(DeactivateOutcome::default())
    }

    async fn read_quotas(
        &self,
        _scope: &AccessScope,
        _filter: QuotaFilter,
        _page: PageRequest,
    ) -> Result<PageResult<Quota>, StoreError> {
        self.check(None)?;
        Ok(PageResult::empty())
    }

    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StoreError> {
        self.check(None)?;
        Ok(HashSet::new())
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StoreError> {
        self.check(None)?;
        Ok(ActiveQuotaCounts::default())
    }
}

/// A policy store double that records seeding attempts and can pretend the
/// global scope is already occupied, so bootstrap's idempotence is testable
/// without a database.
#[derive(Default)]
pub struct FakePolicyStore {
    occupied: Mutex<Option<PolicyVersion>>,
    creates: Mutex<Vec<PolicyDraft>>,
    fail: Mutex<Option<StorageError>>,
}

impl FakePolicyStore {
    /// A store whose global scope already holds `existing`.
    pub fn holding(existing: PolicyVersion) -> Self {
        Self {
            occupied: Mutex::new(Some(existing)),
            ..Self::default()
        }
    }

    /// A store whose create loses the seeding race with `error`.
    pub fn refusing(error: StorageError) -> Self {
        Self {
            fail: Mutex::new(Some(error)),
            ..Self::default()
        }
    }

    /// Every draft a create was asked to persist.
    pub fn creates(&self) -> Vec<PolicyDraft> {
        self.creates.lock().expect("lock").clone()
    }
}

#[async_trait]
impl PolicyStore for FakePolicyStore {
    async fn create_policy(
        &self,
        _ctx: &SecurityContext,
        draft: PolicyDraft,
        _events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        self.creates.lock().expect("lock").push(draft.clone());
        if let Some(error) = self.fail.lock().expect("lock").clone() {
            return Err(error);
        }
        Ok(seeded_version(draft))
    }

    async fn update_policy(
        &self,
        _ctx: &SecurityContext,
        policy_id: PolicyId,
        _update: PolicyUpdate,
        _events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        Err(StorageError::PolicyNotFound { policy_id })
    }

    async fn rollback_policy(
        &self,
        _ctx: &SecurityContext,
        policy_id: PolicyId,
        _target_version: u32,
        _comment: Option<String>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        Err(StorageError::PolicyNotFound { policy_id })
    }

    async fn delete_policy(
        &self,
        _ctx: &SecurityContext,
        policy_id: PolicyId,
        _comment: Option<String>,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        Err(StorageError::PolicyNotFound { policy_id })
    }

    async fn read_policy(
        &self,
        _scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Ok(self.occupied.lock().expect("lock").clone())
    }

    async fn read_active_policy_by_id(
        &self,
        _policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Ok(self.occupied.lock().expect("lock").clone())
    }

    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        Ok(self
            .occupied
            .lock()
            .expect("lock")
            .clone()
            .into_iter()
            .collect())
    }

    async fn read_policy_version(
        &self,
        _policy_id: &PolicyId,
        _version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        Ok(None)
    }

    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        _page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        Err(StorageError::PolicyNotFound {
            policy_id: policy_id.clone(),
        })
    }
}

/// The version a seeding create would have written.
pub fn seeded_version(draft: PolicyDraft) -> PolicyVersion {
    PolicyVersion {
        policy_id: PolicyId::global(),
        version: 1,
        scope: draft.scope,
        engine_id: draft.engine_id,
        engine_config: draft.engine_config,
        timeout_ms: draft.timeout_ms,
        description: draft.description,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::now_utc(),
        created_by: draft.created_by,
        comment: draft.comment,
        schema_snapshot: draft.schema_snapshot,
    }
}

/// The `most-restrictive-wins` global policy the gear seeds at bootstrap.
pub fn global_policy_draft() -> PolicyDraft {
    PolicyDraft {
        scope: PolicyScope::Global,
        engine_id: "most-restrictive-wins".to_owned(),
        engine_config: json!({}),
        timeout_ms: None,
        description: Some("platform default resolution policy".to_owned()),
        comment: Some("seeded at bootstrap".to_owned()),
        created_by: String::new(),
        schema_snapshot: PolicySchemaSnapshot::default(),
    }
}

/// A consumption store that refuses every call.
///
/// The bootstrap and Quota-lifecycle tests never reach a consumption
/// primitive; they need a fourth store only because the plugin binds one.
#[derive(Default)]
pub struct FakeConsumptionStore;

#[async_trait]
impl crate::domain::ConsumptionStore for FakeConsumptionStore {
    async fn apply_debit_plan(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _mutation: &quota_enforcement_sdk::EvaluatedMutation<'_>,
        _events: &[NotificationEvent],
    ) -> Result<
        quota_enforcement_sdk::TransitionOutcome<quota_enforcement_sdk::EvaluatedDebit>,
        StorageError,
    > {
        Err(unreached())
    }

    async fn apply_credit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _quota_id: quota_enforcement_sdk::QuotaId,
        _amount: u64,
        _idempotency: &quota_enforcement_sdk::PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<
        quota_enforcement_sdk::TransitionOutcome<quota_enforcement_sdk::AppliedMutation>,
        StorageError,
    > {
        Err(unreached())
    }

    async fn apply_rollback(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _target: &quota_enforcement_sdk::RollbackTarget,
        _idempotency: &quota_enforcement_sdk::IdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<
        quota_enforcement_sdk::TransitionOutcome<quota_enforcement_sdk::AppliedMutation>,
        StorageError,
    > {
        Err(unreached())
    }

    async fn read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _applicable: &quota_enforcement_sdk::ApplicableQuotas,
    ) -> Result<Vec<quota_enforcement_sdk::QuotaSnapshot>, StorageError> {
        Err(unreached())
    }

    async fn lookup_idempotency(
        &self,
        _scope_of: &quota_enforcement_sdk::IdempotencyScope,
    ) -> Result<Option<quota_enforcement_sdk::IdempotencyRecord>, StorageError> {
        Err(unreached())
    }

    async fn reclaim_expired_idempotency(
        &self,
        _batch_size: u32,
        _before: time::OffsetDateTime,
    ) -> Result<u64, StorageError> {
        Err(unreached())
    }

    async fn reclaim_operation_log(
        &self,
        _batch_size: u32,
        _before: time::OffsetDateTime,
    ) -> Result<u64, StorageError> {
        Err(unreached())
    }
}

fn unreached() -> StorageError {
    StorageError::Internal("the consumption store is not part of this test".to_owned())
}

/// A lease store that answers nothing, for the bootstrap and CRUD tests that
/// construct a plugin but never take a hold. The lease paths have their own
/// suites against the real adapter.
#[derive(Default)]
pub struct FakeLeaseStore;

#[async_trait]
impl crate::domain::ports::LeaseStore for FakeLeaseStore {
    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _mutation: &quota_enforcement_sdk::EvaluatedMutation<'_>,
        _ttl: std::time::Duration,
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::EvaluatedLease>, StorageError> {
        Err(StorageError::Internal(
            "no lease store in this test".to_owned(),
        ))
    }

    async fn commit_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        token: quota_enforcement_sdk::LeaseToken,
        _actual_amount: Option<u64>,
        _idempotency: &quota_enforcement_sdk::PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::AppliedMutation>, StorageError> {
        Err(StorageError::LeaseNotFound { token })
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        token: quota_enforcement_sdk::LeaseToken,
        _idempotency: &quota_enforcement_sdk::PartialIdempotencyWrite,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<quota_enforcement_sdk::AppliedMutation>, StorageError> {
        Err(StorageError::LeaseNotFound { token })
    }

    async fn reclaim_expired_leases(
        &self,
        _batch_size: u32,
        _before: time::OffsetDateTime,
    ) -> Result<Vec<quota_enforcement_sdk::ExpiredLease>, StorageError> {
        Ok(Vec::new())
    }

    async fn count_expired_unreclaimed_leases(
        &self,
        _before: time::OffsetDateTime,
    ) -> Result<Vec<(quota_enforcement_sdk::MetricId, u64)>, StorageError> {
        Ok(Vec::new())
    }
}

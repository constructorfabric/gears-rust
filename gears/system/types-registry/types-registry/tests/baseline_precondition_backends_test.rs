//! A revision must be compared against the version named by its precondition.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{PausePoint, TestDir, TestStores, allow_all, stores, test_db_file};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::Stores;
use types_registry::infra::storage::entity::{entity, type_schema, type_schema_revision};

type Provider = Arc<DBProvider<DbError>>;
const ID: &str = gts_id!("cf.core.precondition.range.v1~");
const NOW: OffsetDateTime = datetime!(2026-09-10 12:00:00 UTC);

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn schema(maximum: i32, title: &str) -> Value {
    json!({
        "$id": format!("gts://{ID}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "integer", "maximum": maximum, "title": title,
    })
}

async fn submit(db: &Provider, key: &str, body: Value, expected: Option<i64>) -> Uuid {
    accept(
        &stores(),
        &DBProvider::<AcceptanceError>::new(db.db()),
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &(Arc::new(NoDispatch) as Arc<dyn OperationDispatch>),
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind: OperationKind::Registration,
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: ID.to_owned(),
                content: Some(body),
                expected_resource_version: expected,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .expect("acceptance reads no entity version")
    .operation_id
}

async fn run(db: &Provider, ports: &Arc<dyn Stores>, operation_id: Uuid) -> OperationOutcome {
    run_operation(
        ports,
        &DBProvider::<WorkerError>::new(db.db()),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: false,
        },
        operation_id,
        NOW,
    )
    .await
    .expect("worker infrastructure")
}

async fn admit(db: &Provider, key: &str, body: Value, expected: Option<i64>) -> OperationOutcome {
    run(db, &stores(), submit(db, key, body, expected).await).await
}

fn assert_refused(outcome: &OperationOutcome, reason: &AdmissionFailureReason) {
    assert_eq!(outcome.items[0].status, OperationItemStatus::Failed);
    assert_eq!(&outcome.items[0].failure.as_ref().unwrap().reason, reason);
    assert_eq!(outcome.items[0].resource_version, None);
    assert_eq!(outcome.items[0].revision_no, None);
}

async fn assert_future_precondition_cannot_commit_an_unchecked_baseline(db: &Provider) {
    let initial = admit(db, "initial", schema(10, "initial"), None).await;
    assert_eq!(initial.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(initial.items[0].resource_version, Some(1));

    // A future version is a valid acceptance input, but not a valid baseline.
    let operation_id = submit(db, "future", schema(10, "candidate"), Some(2)).await;
    let (paused, reached, resume) = TestStores::pausing(PausePoint::BeforeEntityWriteOrderClaim);
    // Retain the ports here so an early refusal cannot close the pause channel.
    let ports: Arc<dyn Stores> = paused;
    let task_ports = Arc::clone(&ports);
    let task_db = Arc::clone(db);
    let mut pass = tokio::spawn(async move { run(&task_db, &task_ports, operation_id).await });
    let early_refusal = tokio::select! {
        outcome = &mut pass => Some(outcome.expect("worker task")),
        signal = reached => {
            signal.expect("evaluation completed before the write-order claim");
            None
        }
    };

    // The broken implementation reaches the pause after comparing against v1.
    // The fixed implementation refuses during evaluation. In either case, let a
    // complete independent worker advance the database to the requested v2.
    let widened = admit(db, "widen", schema(20, "wider"), Some(1)).await;
    assert_eq!(widened.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(widened.items[0].resource_version, Some(2));

    // This is the comparison the old implementation silently skipped.
    let fresh = admit(db, "fresh-control", schema(10, "candidate"), Some(2)).await;
    assert_refused(&fresh, &AdmissionFailureReason::IncompatibleWithBaseline);

    let outcome = if let Some(outcome) = early_refusal {
        outcome
    } else {
        resume.send(()).expect("resume the paused worker");
        pass.await.expect("worker task")
    };
    assert_refused(&outcome, &AdmissionFailureReason::PreconditionFailed);

    // A retry of the accepted operation must retain the terminal refusal even
    // though its originally future precondition now matches the database.
    let replay = run(db, &stores(), operation_id).await;
    assert!(replay.already_terminal);
    assert_refused(&replay, &AdmissionFailureReason::PreconditionFailed);

    assert_only_widening_was_persisted(db).await;
}

async fn assert_only_widening_was_persisted(db: &Provider) {
    let conn = db.conn().expect("connection");
    let row = entity::Entity::find()
        .filter(entity::Column::GtsId.eq(ID))
        .secure()
        .scope_with(&allow_all())
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.resource_version, 2);
    let current = type_schema::Entity::find_by_id(row.id)
        .secure()
        .scope_with(&allow_all())
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.revision_no, 2);
    let resolved: Value = serde_json::from_str(&current.resolved_schema).unwrap();
    assert_eq!(resolved["maximum"], json!(20));
    let revisions = type_schema_revision::Entity::find()
        .filter(type_schema_revision::Column::EntityId.eq(row.id))
        .order_by_asc(type_schema_revision::Column::RevisionNo)
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .unwrap();
    assert_eq!(
        revisions.len(),
        2,
        "no incompatible third revision was written"
    );
}

#[tokio::test]
async fn future_precondition_cannot_commit_an_unchecked_baseline_on_sqlite() {
    let dir = TestDir::new("types-registry-baseline-precondition");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    assert_future_precondition_cannot_commit_an_unchecked_baseline(&db).await;
}

#[tokio::test]
async fn stale_precondition_is_refused_before_comparing_an_incompatible_candidate() {
    let db = common::test_db().await;
    let initial = admit(&db, "initial", schema(10, "initial"), None).await;
    assert_eq!(initial.items[0].status, OperationItemStatus::Succeeded);
    let widened = admit(&db, "widen", schema(20, "wider"), Some(1)).await;
    assert_eq!(widened.items[0].resource_version, Some(2));

    let stale = admit(&db, "stale", schema(10, "candidate"), Some(1)).await;
    assert_refused(&stale, &AdmissionFailureReason::PreconditionFailed);
}

#[tokio::test]
async fn absent_and_mismatched_baselines_refuse_before_schema_validation() {
    let db = common::test_db().await;
    let mut invalid = schema(10, "invalid");
    invalid["type"] = json!("not_a_json_schema_type");
    let absent = admit(&db, "absent", invalid.clone(), Some(1)).await;
    assert_refused(&absent, &AdmissionFailureReason::PreconditionFailed);

    let initial = admit(&db, "initial", schema(10, "initial"), None).await;
    assert_eq!(initial.items[0].status, OperationItemStatus::Succeeded);
    let mismatch = admit(&db, "mismatch", invalid.clone(), Some(2)).await;
    assert_refused(&mismatch, &AdmissionFailureReason::PreconditionFailed);

    let matching = admit(&db, "matching-control", invalid, Some(1)).await;
    assert_refused(&matching, &AdmissionFailureReason::InvalidSchema);
}

#[cfg(feature = "integration")]
mod backends {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

    use super::*;

    #[tokio::test]
    async fn future_precondition_cannot_commit_an_unchecked_baseline_on_postgres() {
        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let host = container.get_host().await.unwrap();
        let db = common::provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
        assert_future_precondition_cannot_commit_an_unchecked_baseline(&db).await;
    }

    #[tokio::test]
    async fn future_precondition_cannot_commit_an_unchecked_baseline_on_mysql() {
        let container = test_containers::mysql().start().await.expect("start mysql");
        let port = container.get_host_port_ipv4(3306).await.unwrap();
        let host = container.get_host().await.unwrap();
        let db = common::provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
        assert_future_precondition_cannot_commit_an_unchecked_baseline(&db).await;
    }
}

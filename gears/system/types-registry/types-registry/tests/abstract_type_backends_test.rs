//! Abstract-type transitions preserve live Instance validity under concurrent admission.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use uuid::Uuid;

use common::{PausePoint, TestDir, TestStores, allow_all, stores, test_db, test_db_file};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums::{LifecycleStatus, OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{CurrentTypeSchemaRow, EntityRow, Stores};
use types_registry::infra::storage::repo::{EntityRepo, TypeSchemaRepo};

type Provider = Arc<DBProvider<DbError>>;
const NOW: OffsetDateTime = datetime!(2026-09-10 12:00:00 UTC);

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn schema(id: &str, is_abstract: bool) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-abstract": is_abstract,
    })
}

fn instance_id(type_id: &str) -> String {
    format!("{type_id}cf.core.abstract_check.value.v1")
}

async fn submit(db: &Provider, id: &str, content: Value, version: Option<i64>) -> Uuid {
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
            idempotency_key: Uuid::new_v4().to_string(),
            kind: OperationKind::Registration,
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: id.to_owned(),
                content: Some(content),
                expected_resource_version: version,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .expect("acceptance")
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

async fn admit(db: &Provider, id: &str, content: Value, version: Option<i64>) -> OperationOutcome {
    run(db, &stores(), submit(db, id, content, version).await).await
}

fn succeeded(outcome: &OperationOutcome) {
    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Succeeded,
        "{outcome:?}"
    );
}

async fn current(db: &Provider, id: &str) -> (EntityRow, CurrentTypeSchemaRow) {
    let conn = db.conn().unwrap();
    let entity = EntityRepo::find_by_gts_id(&conn, &allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    let current = TypeSchemaRepo::find_current(&conn, &allow_all(), entity.id)
        .await
        .unwrap()
        .unwrap();
    (entity, current)
}

async fn assert_abstract(db: &Provider, id: &str, expected: bool, version: i64) {
    let (entity, current) = current(db, id).await;
    assert_eq!(entity.resource_version, version);
    let resolved: Value = serde_json::from_str(&current.resolved_schema).unwrap();
    assert_eq!(resolved["x-gts-abstract"], json!(expected));
}

async fn assert_deleted_instance_does_not_block(db: &Provider) {
    let base = "gts.cf.core.abstract_check.deleted.v1~";
    let instance = instance_id(base);
    succeeded(&admit(db, base, schema(base, false), None).await);
    succeeded(&admit(db, &instance, json!({}), None).await);
    let conn = db.conn().unwrap();
    let entity = EntityRepo::find_by_gts_id(&conn, &allow_all(), &instance)
        .await
        .unwrap()
        .unwrap();
    // Seed the supported storage lifecycle state; retain the edge and authored
    // value so the existence query must distinguish a tombstone from a live Instance.
    assert_eq!(
        EntityRepo::mark_deleted(&conn, &allow_all(), entity.id, 1, NOW)
            .await
            .unwrap(),
        Some(2)
    );
    succeeded(&admit(db, base, schema(base, true), Some(1)).await);
    assert_abstract(db, base, true, 2).await;
    let deleted = EntityRepo::find_by_gts_id(&conn, &allow_all(), &instance)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleted.lifecycle_status, LifecycleStatus::Deleted);
}

async fn assert_derived_and_unrelated_instances_do_not_block(db: &Provider) {
    let base = "gts.cf.core.abstract_check.parent.v1~";
    let derived = "gts.cf.core.abstract_check.parent.v1~cf.core.abstract_check.child.v1~";
    let unrelated = "gts.cf.core.abstract_check.unrelated.v1~";
    for id in [base, derived, unrelated] {
        succeeded(&admit(db, id, schema(id, false), None).await);
    }
    for id in [derived, unrelated] {
        succeeded(&admit(db, &instance_id(id), json!({}), None).await);
    }
    succeeded(&admit(db, base, schema(base, true), Some(1)).await);
    assert_abstract(db, base, true, 2).await;
    // An Instance of the concrete derived type remains admissible after refresh.
    let next = format!("{derived}cf.core.abstract_check.next.v1");
    succeeded(&admit(db, &next, json!({}), None).await);
    assert_abstract(db, derived, false, 1).await;
}

#[derive(Clone, Copy)]
enum FirstCommit {
    Instance,
    AbstractType,
}

async fn assert_race_preserves_instance_validity(db: &Provider, first: FirstCommit) {
    let (base, reason) = match first {
        FirstCommit::Instance => (
            "gts.cf.core.abstract_check.instance_first.v1~",
            AdmissionFailureReason::DependentInvalid,
        ),
        FirstCommit::AbstractType => (
            "gts.cf.core.abstract_check.abstract_first.v1~",
            AdmissionFailureReason::InvalidValue,
        ),
    };
    let instance = instance_id(base);
    succeeded(&admit(db, base, schema(base, false), None).await);
    let before = current(db, base).await;
    let operation_id = match first {
        FirstCommit::Instance => submit(db, base, schema(base, true), Some(1)).await,
        FirstCommit::AbstractType => submit(db, &instance, json!({}), None).await,
    };
    let (paused, reached, resume) = TestStores::pausing(PausePoint::BeforeEntityWriteOrderClaim);
    let ports: Arc<dyn Stores> = paused;
    let task_db = Arc::clone(db);
    let pass = tokio::spawn(async move { run(&task_db, &ports, operation_id).await });
    reached
        .await
        .expect("the losing worker evaluated before either commit");

    let winning = match first {
        FirstCommit::Instance => admit(db, &instance, json!({}), None).await,
        FirstCommit::AbstractType => admit(db, base, schema(base, true), Some(1)).await,
    };
    succeeded(&winning);
    resume.send(()).expect("resume the losing worker");
    let refused = pass.await.expect("worker task");
    assert_eq!(
        refused.items[0].status,
        OperationItemStatus::Failed,
        "{refused:?}"
    );
    assert_eq!(refused.items[0].failure.as_ref().unwrap().reason, reason);
    assert_eq!(refused.items[0].resource_version, None);
    let replay = run(db, &stores(), operation_id).await;
    assert!(replay.already_terminal);
    assert_eq!(replay.items[0].failure.as_ref().unwrap().reason, reason);

    assert_only_winner_committed(db, first, base, &instance, before).await;
}

async fn assert_only_winner_committed(
    db: &Provider,
    first: FirstCommit,
    base: &str,
    instance: &str,
    before: (EntityRow, CurrentTypeSchemaRow),
) {
    let conn = db.conn().unwrap();
    let stored_instance = EntityRepo::find_by_gts_id(&conn, &allow_all(), instance)
        .await
        .unwrap();
    match first {
        FirstCommit::Instance => {
            assert_eq!(
                current(db, base).await,
                before,
                "refused abstract transition changed nothing"
            );
            let instance = stored_instance.expect("the winner is still a live Instance");
            assert_eq!(instance.lifecycle_status, LifecycleStatus::Active);
            assert_eq!(instance.resource_version, 1);
        }
        FirstCommit::AbstractType => {
            assert_abstract(db, base, true, 2).await;
            assert!(
                stored_instance.is_none(),
                "a stale validation must not insert an Instance"
            );
        }
    }
}

#[tokio::test]
async fn deleted_direct_instance_does_not_block_abstract_transition() {
    assert_deleted_instance_does_not_block(&test_db().await).await;
}

#[tokio::test]
async fn derived_and_unrelated_instances_do_not_block_abstract_transition() {
    assert_derived_and_unrelated_instances_do_not_block(&test_db().await).await;
}

#[tokio::test]
async fn instance_committed_after_evaluation_blocks_abstract_transition() {
    let dir = TestDir::new("types-registry-instance-first");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    assert_race_preserves_instance_validity(&db, FirstCommit::Instance).await;
}

#[tokio::test]
async fn abstract_transition_committed_after_evaluation_blocks_instance_creation() {
    let dir = TestDir::new("types-registry-abstract-first");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    assert_race_preserves_instance_validity(&db, FirstCommit::AbstractType).await;
}

#[cfg(feature = "integration")]
mod backends {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

    use super::*;

    async fn assert_backend(db: &Provider) {
        assert_deleted_instance_does_not_block(db).await;
        assert_derived_and_unrelated_instances_do_not_block(db).await;
        assert_race_preserves_instance_validity(db, FirstCommit::Instance).await;
        assert_race_preserves_instance_validity(db, FirstCommit::AbstractType).await;
    }

    #[tokio::test]
    async fn abstract_transition_and_instance_races_on_postgres() {
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
        assert_backend(&db).await;
    }

    #[tokio::test]
    async fn abstract_transition_and_instance_races_on_mysql() {
        let container = test_containers::mysql().start().await.expect("start mysql");
        let port = container.get_host_port_ipv4(3306).await.unwrap();
        let host = container.get_host().await.unwrap();
        let db = common::provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
        assert_backend(&db).await;
    }
}

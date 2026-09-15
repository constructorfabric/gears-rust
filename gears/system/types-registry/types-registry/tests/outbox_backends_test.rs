//! Outbox delivery on PostgreSQL and MySQL, with SQLite as a control.
//!
//! Exercise dialect-specific claim, acknowledgement and cursor SQL, including
//! MySQL ID allocation hidden by SQLite's single-writer model.
//!
//! `assert_single_admission_under_two_pipelines` runs two pipelines over one
//! database and asserts the candidate is admitted once. It pins the outcome, not
//! the mechanism: it neither forces the two attempts to overlap nor counts handler
//! entries, so it does not show whether the lease or the item CAS refused the
//! second worker. Distinguishing them needs a handler wrapper that holds the first
//! pass and observes whether the second is admitted at all.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;

use common::{await_delivery, metrics, provider_for_with_outbox, stores};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind, OperationStatus};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{AdmissionMode, EntityKey, RegistryService};
use types_registry::infra::outbox::OutboxDispatch;

const NOW: OffsetDateTime = datetime!(2026-09-14 12:00:00 UTC);
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

const TARGET: &str = gts_id!("cf.core.obxback.target.v1~");

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// Verify registration delivery through the outbox without calling the worker.
async fn assert_delivery(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let dispatch = Arc::new(OutboxDispatch::new());
    let registry = Arc::new(RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        AdmissionMode::Outbox,
        metrics(),
    ));
    let handle = types_registry::infra::outbox::start(db.db(), &registry, &dispatch)
        .await
        .unwrap_or_else(|e| panic!("{backend}: start the admission outbox: {e}"));

    let request = SubmitRequest {
        idempotency_key: "backends-key".to_owned(),
        kind: OperationKind::Registration,
        dry_run: false,
        candidates: vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: Some(schema(TARGET)),
            expected_resource_version: None,
            force: false,
        }],
    };
    let accepted = registry
        .submit(&request, NOW)
        .await
        .unwrap_or_else(|e| panic!("{backend}: accept: {e}"));
    assert_eq!(
        accepted.status,
        OperationStatus::Pending,
        "{backend}: a dispatched submission must not admit in the caller's task",
    );

    let operation = await_delivery(&format!("{backend}: registration"), || async {
        let record = registry
            .operation(accepted.operation_id)
            .await
            .unwrap_or_else(|e| panic!("{backend}: read the operation: {e}"))
            .unwrap_or_else(|| panic!("{backend}: the operation exists"));
        match record.status {
            OperationStatus::Completed => Some(record),
            OperationStatus::Pending | OperationStatus::Running => None,
        }
    })
    .await;

    assert_eq!(
        operation.items[0].status,
        OperationItemStatus::Succeeded,
        "{backend}: {:?}",
        operation.items,
    );
    let entity = registry
        .entity(&EntityKey::GtsId(TARGET.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{backend}: read the entity: {e}"))
        .unwrap_or_else(|| panic!("{backend}: the admitted entity is readable"));
    assert_eq!(entity.resource_version, 1, "{backend}");

    handle.stop().await;
}

/// Two pipelines, one database/message: admit once, with no second revision or
/// resource-version bump, whether the lease or item CAS excludes the second worker.
async fn assert_single_admission_under_two_pipelines(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let gts_id = gts_id!("cf.core.obxback.contended.v1~");

    // Two independent services over the same `Db`, so two leased workers compete
    // for the same partition. Only the first gets to enqueue.
    let dispatch = Arc::new(OutboxDispatch::new());
    let submitter = Arc::new(RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        AdmissionMode::Outbox,
        metrics(),
    ));
    let second_dispatch = Arc::new(OutboxDispatch::new());
    let second = Arc::new(RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&second_dispatch) as Arc<dyn OperationDispatch>,
        AdmissionMode::Outbox,
        metrics(),
    ));
    let first_handle = types_registry::infra::outbox::start(db.db(), &submitter, &dispatch)
        .await
        .unwrap_or_else(|e| panic!("{backend}: start the first pipeline: {e}"));
    let second_handle = types_registry::infra::outbox::start(db.db(), &second, &second_dispatch)
        .await
        .unwrap_or_else(|e| panic!("{backend}: start the second pipeline: {e}"));

    let accepted = submitter
        .submit(
            &SubmitRequest {
                idempotency_key: "contended-key".to_owned(),
                kind: OperationKind::Registration,
                dry_run: false,
                candidates: vec![Candidate {
                    gts_id: gts_id.to_owned(),
                    content: Some(schema(gts_id)),
                    expected_resource_version: None,
                    force: false,
                }],
            },
            NOW,
        )
        .await
        .unwrap_or_else(|e| panic!("{backend}: accept: {e}"));

    let operation = await_delivery(&format!("{backend}: contended registration"), || async {
        let record = submitter
            .operation(accepted.operation_id)
            .await
            .unwrap_or_else(|e| panic!("{backend}: read the operation: {e}"))
            .unwrap_or_else(|| panic!("{backend}: the operation exists"));
        match record.status {
            OperationStatus::Completed => Some(record),
            OperationStatus::Pending | OperationStatus::Running => None,
        }
    })
    .await;

    assert_eq!(
        operation.items.len(),
        1,
        "{backend}: two workers must not double the items: {:?}",
        operation.items,
    );
    assert_eq!(
        operation.items[0].status,
        OperationItemStatus::Succeeded,
        "{backend}: {:?}",
        operation.items,
    );
    let entity = submitter
        .entity(&EntityKey::GtsId(gts_id.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{backend}: read the entity: {e}"))
        .unwrap_or_else(|| panic!("{backend}: the admitted entity is readable"));
    assert_eq!(
        entity.resource_version, 1,
        "{backend}: a second admission would have bumped the version",
    );

    first_handle.stop().await;
    second_handle.stop().await;
}

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};

    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn delivery_behaves_on_sqlite_control() {
    let db = common::test_db_with_outbox().await;
    assert_delivery(&db, "sqlite").await;
    assert_single_admission_under_two_pipelines(&db, "sqlite").await;
}

#[tokio::test]
async fn delivery_behaves_on_postgres() {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = request.start().await.expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres port");
    let host = container
        .get_host()
        .await
        .expect("postgres host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;

    let db = provider_for_with_outbox(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
    assert_delivery(&db, "postgres").await;
    assert_single_admission_under_two_pipelines(&db, "postgres").await;
}

#[tokio::test]
async fn delivery_behaves_on_mysql() {
    let container = test_containers::mysql()
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    let host = container.get_host().await.expect("mysql host").to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;

    let db = provider_for_with_outbox(&format!("mysql://root@{host}:{port}/test"), 4).await;
    assert_delivery(&db, "mysql").await;
    assert_single_admission_under_two_pipelines(&db, "mysql").await;
}

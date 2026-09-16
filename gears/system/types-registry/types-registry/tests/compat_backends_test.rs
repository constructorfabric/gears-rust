//! T17/T18 admission behavior on PostgreSQL and MySQL.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::EntityTrait;
use serde_json::{Value, json};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{allow_all, provider_for, stores};
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
use types_registry::infra::storage::entity::{entity, instance_revision, type_schema_revision};

const NOW: OffsetDateTime = datetime!(2026-09-09 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-09 10:20:40 UTC);
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const DRAFT_2020: &str = "https://json-schema.org/draft/2020-12/schema";

const CLOSED: &str = gts_id!("cf.core.compat_backend.closed.v1~");
const OPEN: &str = gts_id!("cf.core.compat_backend.open.v1~");
const PARTIAL: &str = gts_id!("cf.core.compat_backend.partial.v1~");
const MINOR_0: &str = gts_id!("cf.core.compat_backend.forced.v2.0~");
const MINOR_1: &str = gts_id!("cf.core.compat_backend.forced.v2.1~");
const UNSTABLE: &str = gts_id!("cf.core.compat_backend.unstable.v0~");
const DERIVED: &str =
    gts_id!("cf.core.compat_backend.unstable.v0~cf.core.compat_backend.derived.v1~");
const REFERRER: &str = gts_id!("cf.core.compat_backend.referrer.v1~");
const INSTANCE: &str =
    gts_id!("cf.core.compat_backend.unstable.v0~cf.core.compat_backend.first.v1");
const STABLE_INSTANCE: &str =
    gts_id!("cf.core.compat_backend.closed.v1~cf.core.compat_backend.first.v1");
const DIALECT: &str = gts_id!("cf.core.compat_backend.dialect.v1~");

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Level {
    Closed,
    Open,
    Partial,
}

fn schema(gts_id: &str, level: Level, second_property: bool) -> Value {
    let mut document = json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "properties": { "a": { "type": "string" } },
    });
    if second_property {
        document["properties"]["b"] = json!({ "type": "string" });
    }
    match level {
        Level::Closed => document["additionalProperties"] = json!(false),
        Level::Open => {}
        Level::Partial => {
            document["patternProperties"] = json!({ "^b": { "type": "string" } });
            document["additionalProperties"] = json!(false);
        }
    }
    document
}

fn referencing(gts_id: &str, target: &str) -> Value {
    let mut document = schema(gts_id, Level::Open, false);
    document["properties"]["target"] = json!({ "$ref": format!("gts://{target}") });
    document
}

async fn admit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
    force: bool,
) -> OperationOutcome {
    let config = TypesRegistryConfig {
        allow_compatibility_force: force,
        ..Default::default()
    };
    let provider = DBProvider::<AcceptanceError>::new(db.db());
    let operation_id = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &config,
            metrics: &common::metrics(),
        },
        &(Arc::new(NoDispatch) as Arc<dyn OperationDispatch>),
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind: OperationKind::Registration,
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version,
                force,
            }],
        },
        NOW,
    )
    .await
    .expect("the request reaches the worker")
    .operation_id;

    run_operation(
        &stores(),
        &DBProvider::<WorkerError>::new(db.db()),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: force,
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the admission pass completes")
}

fn assert_succeeded(outcome: &OperationOutcome) {
    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Succeeded,
        "{:?}",
        outcome.items[0].failure,
    );
}

fn assert_refused(outcome: &OperationOutcome, reason: &AdmissionFailureReason) {
    assert_eq!(
        (
            outcome.items[0].status,
            outcome.items[0]
                .failure
                .as_ref()
                .map(|failure| &failure.reason),
        ),
        (OperationItemStatus::Failed, Some(reason)),
        "{:?}",
        outcome.items[0].failure,
    );
}

async fn assert_compatibility_matrix(db: &Arc<DBProvider<DbError>>) {
    for (key, id, level, reason) in [
        ("closed", CLOSED, Level::Closed, None),
        (
            "open",
            OPEN,
            Level::Open,
            Some(AdmissionFailureReason::IncompatibleWithBaseline),
        ),
        (
            "partial",
            PARTIAL,
            Level::Partial,
            Some(AdmissionFailureReason::CompatibilityUndecidable),
        ),
    ] {
        assert_succeeded(
            &admit(
                db,
                &format!("{key}-1"),
                id,
                schema(id, level, false),
                None,
                false,
            )
            .await,
        );
        let outcome = admit(
            db,
            &format!("{key}-2"),
            id,
            schema(id, level, true),
            Some(1),
            false,
        )
        .await;
        match reason {
            Some(reason) => assert_refused(&outcome, &reason),
            None => assert_succeeded(&outcome),
        }
    }
}

async fn assert_forced_cross_minor(db: &Arc<DBProvider<DbError>>) {
    assert_succeeded(
        &admit(
            db,
            "minor-0",
            MINOR_0,
            schema(MINOR_0, Level::Open, false),
            None,
            false,
        )
        .await,
    );
    assert_succeeded(
        &admit(
            db,
            "minor-1",
            MINOR_1,
            schema(MINOR_1, Level::Open, true),
            None,
            true,
        )
        .await,
    );
}

async fn assert_quarantine(db: &Arc<DBProvider<DbError>>) {
    assert_succeeded(
        &admit(
            db,
            "unstable",
            UNSTABLE,
            schema(UNSTABLE, Level::Open, false),
            None,
            false,
        )
        .await,
    );
    for (key, id, content, reason) in [
        (
            "derived",
            DERIVED,
            schema(DERIVED, Level::Open, false),
            AdmissionFailureReason::StableDerivesFromMajorZero,
        ),
        (
            "referrer",
            REFERRER,
            referencing(REFERRER, UNSTABLE),
            AdmissionFailureReason::StableRefsMajorZero,
        ),
        (
            "instance",
            INSTANCE,
            json!({ "a": "value" }),
            AdmissionFailureReason::InstanceOfMajorZero,
        ),
    ] {
        assert_refused(&admit(db, key, id, content, None, false).await, &reason);
    }
    assert_succeeded(
        &admit(
            db,
            "stable-instance",
            STABLE_INSTANCE,
            json!({ "a": "value" }),
            None,
            false,
        )
        .await,
    );
}

async fn assert_dialect_pin(db: &Arc<DBProvider<DbError>>) {
    assert_succeeded(
        &admit(
            db,
            "dialect-1",
            DIALECT,
            schema(DIALECT, Level::Open, false),
            None,
            false,
        )
        .await,
    );
    common::restate_stored_dialect(db, DIALECT, DRAFT_2020).await;
    assert_refused(
        &admit(
            db,
            "dialect-2",
            DIALECT,
            schema(DIALECT, Level::Open, false),
            Some(1),
            false,
        )
        .await,
        &AdmissionFailureReason::DialectChanged,
    );
}

async fn assert_provenance(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let conn = db.conn().expect("connection");
    let schema_revisions = type_schema_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("type schema revisions");
    // Identity, not existence: a worker that stamped the item's `force` request
    // onto every revision instead of the post-recheck verdict would satisfy an
    // `any(compat_forced)` assertion, and this is the only backend-level cover
    // for the effective waiver (DESIGN 2185).
    let entities: HashMap<i64, String> = entity::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("entities")
        .into_iter()
        .map(|row| (row.id, row.gts_id))
        .collect();
    let mut waivers: Vec<(&str, i32)> = schema_revisions
        .iter()
        .filter(|revision| revision.compat_forced)
        .map(|revision| {
            let gts_id = entities
                .get(&revision.entity_id)
                .unwrap_or_else(|| panic!("revision {} has no entity row", revision.entity_id));
            (gts_id.as_str(), revision.revision_no)
        })
        .collect();
    waivers.sort_unstable();
    assert_eq!(
        waivers,
        vec![(MINOR_1, 1)],
        "only the forced cross-minor revision may record a waiver on {backend}; \
         every other revision must persist `compat_forced = false`",
    );
    assert!(
        schema_revisions.iter().all(|revision| {
            revision.gts_spec_version == gts::GTS_SPECIFICATION_VERSION
                && revision.gts_impl_version == gts::GTS_IMPLEMENTATION_VERSION
        }),
        "every Type Schema revision must persist GTS provenance on {backend}",
    );
    let instance_revisions = instance_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("instance revisions");
    assert_eq!(
        instance_revisions.len(),
        1,
        "the stable control Instance must be persisted on {backend}",
    );
    assert!(
        instance_revisions.iter().all(|revision| {
            revision.gts_spec_version == gts::GTS_SPECIFICATION_VERSION
                && revision.gts_impl_version == gts::GTS_IMPLEMENTATION_VERSION
        }),
        "every Instance revision must persist GTS provenance on {backend}",
    );
}

async fn assert_t17_t18(db: &Arc<DBProvider<DbError>>, backend: &str) {
    assert_compatibility_matrix(db).await;
    assert_forced_cross_minor(db).await;
    assert_quarantine(db).await;
    assert_dialect_pin(db).await;
    assert_provenance(db, backend).await;
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
async fn compatibility_and_quarantine_behave_on_sqlite_control() {
    let db = common::test_db().await;
    assert_t17_t18(&db, "sqlite").await;
}

#[tokio::test]
async fn compatibility_and_quarantine_behave_on_postgres() {
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

    let db = provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
    assert_t17_t18(&db, "postgres").await;
}

#[tokio::test]
async fn compatibility_and_quarantine_behave_on_mysql() {
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

    let db = provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
    assert_t17_t18(&db, "mysql").await;
}

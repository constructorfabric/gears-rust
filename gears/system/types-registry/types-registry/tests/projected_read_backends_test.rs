//! Projected reads (T22b) against every backend, with the SQL recorded.
//!
//! Properties only a real database shows:
//!
//! * **An unselected column is not in the `SELECT`.** Metadata-only reads never
//!   name `raw_schema`, `canonical_value` or an artifact column, so documents are
//!   neither transferred nor parsed. Selected documents arrive in a bounded number
//!   of statements, all in one snapshot transaction.
//! * **`SeaORM` reads an absent column into `Option` as `None`** on each driver,
//!   while a selected column round-trips as `Some`. The domain turns a *selected*
//!   `None` into corruption, so the two cases cannot be confused.
//! * **A non-canonical key is absent without a lookup**, on every driver.
//!
//! `SQLite` runs unconditionally; `PostgreSQL` and `MySQL` need Docker:
//!
//! ```text
//! cargo test -p cf-gears-types-registry --features integration --test projected_read_backends_test
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::test_support::{QueryKind, QueryRecorder, connect_with_recorder};
use toolkit_db::{ConnectOpts, DBProvider, Db, DbError};
use toolkit_gts::gts_id;

use common::{allow_all, doc, stores};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::OperationKind;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{
    DiscoveryQuery, EntityKey, EntityLookup, MAX_KEY_LEN, RegistryService, ServiceError,
};
use types_registry::domain::selection::FieldSelection;
use types_registry::infra::storage::repo::{EntityRepo, InstanceRepo, TypeSchemaRepo};

const NOW: OffsetDateTime = datetime!(2026-09-23 09:00:00 UTC);
const TYPE: &str = gts_id!("cf.core.projread.type.v1~");
const INSTANCE: &str = gts_id!("cf.core.projread.type.v1~cf.core.projread.first.v1");

/// Every column holding a stored JSON document.
const DOCUMENT_COLUMNS: [&str; 5] = [
    "raw_schema",
    "canonical_value",
    "resolved_schema",
    "effective_traits",
    "effective_traits_schema",
];

/// Two identity reads, then pointer and revision per kind.
const MAX_BATCH_STATEMENTS: usize = 6;

struct Harness {
    service: RegistryService,
    recorder: QueryRecorder,
    db: Arc<DBProvider<DbError>>,
}

async fn harness(dsn: &str, max_conns: u32) -> Harness {
    let opts = ConnectOpts {
        max_conns: Some(max_conns),
        min_conns: Some(1),
        ..Default::default()
    };
    let (db, recorder): (Db, QueryRecorder) =
        connect_with_recorder(dsn, opts).await.expect("connect");
    run_migrations_for_testing(&db, {
        use sea_orm_migration::MigratorTrait;
        types_registry::infra::storage::Migrator::migrations()
    })
    .await
    .expect("migrations");
    let service = RegistryService::new(
        db.clone(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::new(common::NoDispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    );
    let harness = Harness {
        service,
        recorder,
        db: Arc::new(DBProvider::new(db)),
    };
    admit(&harness.service, "type", TYPE, schema()).await;
    admit(
        &harness.service,
        "instance",
        INSTANCE,
        json!({ "name": "first" }),
    )
    .await;
    harness.recorder.clear();
    harness
}

fn schema() -> Value {
    json!({
        "$id": format!("gts://{TYPE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

async fn admit(service: &RegistryService, key: &str, gts_id: &str, content: Value) {
    let accepted = service
        .submit(
            &SubmitRequest {
                idempotency_key: Some(key.to_owned()),
                kind: OperationKind::Registration,
                dry_run: false,
                candidates: vec![Candidate {
                    gts_id: gts_id.to_owned(),
                    content: Some(content),
                    expected_resource_version: None,
                    force: false,
                }],
            },
            NOW,
        )
        .await
        .expect("accepted");
    service
        .admit(accepted.operation_id, NOW)
        .await
        .expect("admitted");
}

fn select(names: &[&str]) -> FieldSelection {
    FieldSelection::parse(names).expect("valid selection")
}

/// Every statement since the last `clear`, asserting the read shape.
fn assert_read_shape(recorder: &QueryRecorder, backend: &str, what: &str) -> Vec<String> {
    let events = recorder.events();
    assert!(!events.is_empty(), "{what} on {backend}: nothing recorded");
    assert!(
        events.iter().all(|e| e.kind == QueryKind::Select),
        "{what} on {backend} only reads: {events:#?}",
    );
    assert!(
        events.len() <= MAX_BATCH_STATEMENTS,
        "{what} on {backend}: {} statements, bounded by {MAX_BATCH_STATEMENTS}, never one \
         per entity: {events:#?}",
        events.len(),
    );
    assert!(
        recorder.all_in_one_transaction(),
        "{what} on {backend} reads one snapshot: {events:#?}",
    );
    events.into_iter().map(|e| e.sql).collect()
}

/// A whole quoted identifier, so `effective_traits` does not match `effective_traits_schema`.
fn named(statements: &[String], column: &str) -> bool {
    let quoted = [format!("\"{column}\""), format!("`{column}`")];
    statements
        .iter()
        .any(|sql| quoted.iter().any(|q| sql.contains(q.as_str())))
}

async fn metadata_only_reads_fetch_no_document(h: &Harness, backend: &str) {
    let keys = [
        EntityKey::GtsId(TYPE.to_owned()),
        EntityKey::GtsId(INSTANCE.to_owned()),
    ];
    for (what, selection) in [
        ("default batch", FieldSelection::default()),
        ("provenance batch", select(&["provenance"])),
    ] {
        h.recorder.clear();
        let results = h
            .service
            .batch_get(&keys, selection)
            .await
            .expect("batch read");
        assert!(
            results
                .iter()
                .all(|(_, lookup)| matches!(lookup, EntityLookup::Found(_)))
        );
        let statements = assert_read_shape(&h.recorder, backend, what);
        for column in DOCUMENT_COLUMNS {
            assert!(
                !named(&statements, column),
                "{what} on {backend} must not select `{column}`: {statements:#?}",
            );
        }
    }

    h.recorder.clear();
    h.service
        .entity(&keys[0], FieldSelection::default())
        .await
        .expect("exact read")
        .expect("found");
    let statements = assert_read_shape(&h.recorder, backend, "default exact read");
    for column in DOCUMENT_COLUMNS {
        assert!(!named(&statements, column), "{backend}: {statements:#?}");
    }
}

async fn selected_documents_are_fetched_and_only_they(h: &Harness, backend: &str) {
    let keys = [
        EntityKey::GtsId(TYPE.to_owned()),
        EntityKey::GtsId(INSTANCE.to_owned()),
    ];
    h.recorder.clear();
    let results = h
        .service
        .batch_get(&keys, select(&["content"]))
        .await
        .expect("batch read");
    let statements = assert_read_shape(&h.recorder, backend, "content batch");
    assert!(named(&statements, "raw_schema"), "{backend}");
    assert!(named(&statements, "canonical_value"), "{backend}");
    for column in ["resolved_schema", "effective_traits"] {
        assert!(!named(&statements, column), "{backend}: {statements:#?}");
    }
    let EntityLookup::Found(schema_record) = &results[0].1 else {
        panic!("type found on {backend}");
    };
    assert_eq!(
        doc(schema_record.content.as_deref()),
        Some(schema()),
        "{backend}"
    );
    assert!(schema_record.resolved_schema.is_none(), "{backend}");

    h.recorder.clear();
    let results = h
        .service
        .batch_get(&keys, select(&["effective_traits"]))
        .await
        .expect("batch read");
    let statements = assert_read_shape(&h.recorder, backend, "traits batch");
    assert!(named(&statements, "effective_traits"), "{backend}");
    for column in [
        "raw_schema",
        "canonical_value",
        "resolved_schema",
        "effective_traits_schema",
    ] {
        assert!(!named(&statements, column), "{backend}: {statements:#?}");
    }
    let EntityLookup::Found(schema_record) = &results[0].1 else {
        panic!("type found on {backend}");
    };
    assert!(schema_record.effective_traits.is_some(), "{backend}");
}

/// The repository contract the domain relies on, per driver.
async fn absent_columns_read_as_none_and_selected_ones_as_some(h: &Harness, backend: &str) {
    let conn = h.db.conn().expect("conn");
    let scope = allow_all();
    let type_id = EntityRepo::find_by_gts_id(&conn, &scope, TYPE)
        .await
        .expect("read")
        .expect("type")
        .id;
    let instance_id = EntityRepo::find_by_gts_id(&conn, &scope, INSTANCE)
        .await
        .expect("read")
        .expect("instance")
        .id;

    let light = TypeSchemaRepo::read_current(&conn, &scope, &[type_id], FieldSelection::default())
        .await
        .expect("light schema read");
    assert_eq!(light.len(), 1, "{backend}");
    assert!(
        light[0].content.is_none()
            && light[0].resolved_schema.is_none()
            && light[0].effective_traits.is_none()
            && light[0].effective_traits_schema.is_none()
            && light[0].provenance.is_none(),
        "absent columns must read as None on {backend}: {:?}",
        light[0],
    );

    let full = TypeSchemaRepo::read_current(&conn, &scope, &[type_id], FieldSelection::full())
        .await
        .expect("full schema read");
    assert!(
        full[0].content.is_some()
            && full[0].resolved_schema.is_some()
            && full[0].effective_traits.is_some()
            && full[0].effective_traits_schema.is_some(),
        "selected columns must read as Some on {backend}: {:?}",
        full[0],
    );
    let provenance = full[0].provenance.as_ref().expect("schema provenance");
    assert_eq!(provenance.compat_forced, Some(false), "{backend}");

    let instance =
        InstanceRepo::read_current(&conn, &scope, &[instance_id], FieldSelection::full())
            .await
            .expect("full instance read");
    assert_eq!(
        instance[0].content.as_deref(),
        Some(r#"{"name":"first"}"#),
        "{backend}"
    );
    let provenance = instance[0]
        .provenance
        .as_ref()
        .expect("instance provenance");
    assert_eq!(
        provenance.compat_forced, None,
        "an Instance has none, on {backend}"
    );
    let light =
        InstanceRepo::read_current(&conn, &scope, &[instance_id], FieldSelection::default())
            .await
            .expect("light instance read");
    assert!(
        light[0].content.is_none() && light[0].provenance.is_none(),
        "{backend}"
    );
}

async fn discovery_fetches_only_selected_documents(h: &Harness, backend: &str) {
    for (selection, fetched) in [
        (FieldSelection::default(), &[][..]),
        (select(&["content"]), &["raw_schema", "canonical_value"][..]),
    ] {
        h.recorder.clear();
        let page = h
            .service
            .discover(&DiscoveryQuery {
                selection,
                ..DiscoveryQuery::default()
            })
            .await
            .expect("discovery");
        assert_eq!(page.items.len(), 2, "{backend}");
        let events = h.recorder.events();
        assert!(
            events.iter().all(|e| e.kind == QueryKind::Select)
                && h.recorder.all_in_one_transaction(),
            "a page is read in one snapshot on {backend}: {events:#?}",
        );
        let statements: Vec<String> = events.into_iter().map(|e| e.sql).collect();
        for column in DOCUMENT_COLUMNS {
            assert_eq!(
                named(&statements, column),
                fetched.contains(&column),
                "`{column}` on {backend} under {}: {statements:#?}",
                selection.canonical(),
            );
        }
    }
}

/// Non-canonical spellings of a stored identifier, and keys that are no identifier.
fn non_canonical_keys() -> [String; 4] {
    [
        format!("{TYPE} "),
        TYPE.replace("projread", "projr\u{e9}ad"),
        "gts.cf.core.projread".to_owned(),
        "a".repeat(MAX_KEY_LEN),
    ]
}

async fn non_canonical_keys_are_absent_without_sql(h: &Harness, backend: &str) {
    for key in non_canonical_keys() {
        let key = EntityKey::GtsId(key);
        let batch = h
            .service
            .batch_get(
                &[EntityKey::GtsId(TYPE.to_owned()), key.clone()],
                FieldSelection::default(),
            )
            .await
            .unwrap_or_else(|e| panic!("batch read of {key:?} on {backend}: {e}"));
        assert!(
            matches!(
                batch[..],
                [(_, EntityLookup::Found(_)), (_, EntityLookup::NotFound)]
            ),
            "{key:?} on {backend}: {batch:?}",
        );
        h.recorder.clear();
        let exact = h
            .service
            .entity(&key, FieldSelection::default())
            .await
            .unwrap_or_else(|e| panic!("exact read of {key:?} on {backend}: {e}"));
        assert!(exact.is_none(), "{key:?} on {backend}: {exact:?}");
        let events = h.recorder.events();
        assert!(events.is_empty(), "{key:?} on {backend}: {events:#?}");
    }
}

/// The domain refuses what the REST handler refuses early, for any adapter.
async fn an_over_long_key_is_refused_by_the_service(h: &Harness, backend: &str) {
    let over = EntityKey::GtsId("a".repeat(MAX_KEY_LEN + 1));
    let selection = FieldSelection::default();
    for result in [
        h.service
            .batch_get(std::slice::from_ref(&over), selection)
            .await
            .map(drop),
        h.service.entity(&over, selection).await.map(drop),
    ] {
        assert!(
            matches!(result, Err(ServiceError::KeyTooLong { len }) if len == MAX_KEY_LEN + 1),
            "{backend}: {result:?}",
        );
    }
}

async fn assert_projected_reads(h: &Harness, backend: &str) {
    non_canonical_keys_are_absent_without_sql(h, backend).await;
    an_over_long_key_is_refused_by_the_service(h, backend).await;
    metadata_only_reads_fetch_no_document(h, backend).await;
    discovery_fetches_only_selected_documents(h, backend).await;
    selected_documents_are_fetched_and_only_they(h, backend).await;
    absent_columns_read_as_none_and_selected_ones_as_some(h, backend).await;
}

#[tokio::test]
async fn projected_reads_behave_on_sqlite() {
    let h = harness("sqlite::memory:", 1).await;
    assert_projected_reads(&h, "sqlite").await;
}

#[cfg(feature = "integration")]
mod containers {
    use std::time::Duration;

    use super::{assert_projected_reads, harness};

    async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::net::TcpStream::connect((host, port)).await.is_err() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for {host}:{port}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn projected_reads_behave_on_postgres() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;

        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let host = container.get_host().await.expect("host").to_string();
        wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;
        let h = harness(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
        assert_projected_reads(&h, "postgres").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn projected_reads_behave_on_mysql() {
        use testcontainers::runners::AsyncRunner;

        let container = test_containers::mysql()
            .start()
            .await
            .expect("start mysql container");
        let port = container.get_host_port_ipv4(3306).await.expect("port");
        let host = container.get_host().await.expect("host").to_string();
        wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;
        let h = harness(&format!("mysql://root@{host}:{port}/test"), 4).await;
        assert_projected_reads(&h, "mysql").await;
    }
}

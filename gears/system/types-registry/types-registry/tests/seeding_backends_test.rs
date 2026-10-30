#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Single-instance SQLite cold/restart/update and concurrent seeding on PostgreSQL,
//! MySQL and MariaDB with independent connection pools and outbox runtimes (T31).

mod common;

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::{Value, json};
use testcontainers::ImageExt;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{EntityKey, RegistryService};
use types_registry::domain::seeding::{SeedSet, seed};
use types_registry::domain::selection::FieldSelection;
use types_registry::infra::outbox::OutboxDispatch;

/// Two registry instances race: a message one sequences into a partition the
/// other's processor leases waits for that processor's idle poll (one minute under
/// the `low_latency` outbox profile), so the budget covers more than one poll.
const BUDGET: Duration = Duration::from_mins(3);
const BASE: &str = "gts.cf.core.seedback.base.v1~";
const CHILD: &str = "gts.cf.core.seedback.base.v1~cf.core.seedback.child.v1~";
const INSTANCE: &str =
    "gts.cf.core.seedback.base.v1~cf.core.seedback.child.v1~cf.core.seedback.one.v1";

struct Registry {
    service: Arc<RegistryService>,
    outbox: toolkit_db::outbox::OutboxHandle,
}

async fn registry(db: &Arc<DBProvider<DbError>>, backend: &str) -> Registry {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .unwrap_or_else(|e| panic!("{backend}: start the admission outbox: {e}"));
    Registry { service, outbox }
}

fn documents(description: &str) -> [Value; 3] {
    [
        json!({
            "$id": format!("gts://{BASE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"id": {"type": "string"}, "name": {"type": "string"}}
        }),
        json!({
            "$id": format!("gts://{CHILD}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "description": description,
            "type": "object",
            "allOf": [{"$ref": format!("gts://{BASE}")}]
        }),
        json!({"id": INSTANCE, "name": "one"}),
    ]
}

fn seeds(description: &str) -> SeedSet {
    SeedSet::collect(
        documents(description),
        &TypesRegistryConfig::default().entity_id_fields,
    )
    .expect("a valid seed set")
}

async fn revision(service: &RegistryService, gts_id: &str) -> Option<(i64, Value)> {
    let record = service
        .entity(
            common::caller(),
            &EntityKey::GtsId(gts_id.to_owned()),
            FieldSelection::full(),
        )
        .await
        .expect("read a seed")?;
    Some((
        record.origin.expect("managed").resource_version,
        common::doc(record.content.as_deref()).expect("content"),
    ))
}

async fn assert_stored(
    service: &RegistryService,
    description: &str,
    child_revision: i64,
    backend: &str,
) {
    for ((id, version), document) in [(BASE, 1), (CHILD, child_revision), (INSTANCE, 1)]
        .into_iter()
        .zip(documents(description))
    {
        assert_eq!(
            revision(service, id).await,
            Some((version, document)),
            "{backend}: {id} has the desired content and exactly one admitted revision"
        );
    }
}

async fn seed_concurrently(a: &Registry, b: &Registry, set: &SeedSet, backend: &str) {
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = JoinSet::new();
    for (side, service) in [("left", &a.service), ("right", &b.service)] {
        let service = Arc::clone(service);
        let desired = set.clone();
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            let cancel = CancellationToken::new();
            barrier.wait().await;
            let started = Instant::now();
            let result = seed(&service, &desired, BUDGET, &cancel).await;
            (side, started, Instant::now(), result)
        });
    }
    let mut intervals = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let (side, started, ended, result) = task.expect("seeder task panicked");
        result.unwrap_or_else(|error| panic!("{backend}: {side} seeder: {error}"));
        intervals.push((started, ended));
    }
    assert_eq!(intervals.len(), 2);
    assert!(
        intervals[0].0 < intervals[1].1 && intervals[1].0 < intervals[0].1,
        "{backend}: seed calls must overlap: {intervals:?}"
    );
}

async fn assert_single_instance_seeding(dsn: &str, backend: &str) {
    let cancel = CancellationToken::new();
    let set = seeds("first");
    let db = common::provider_for_with_outbox(dsn, 8).await;
    let a = registry(&db, backend).await;
    seed(&a.service, &set, BUDGET, &cancel)
        .await
        .expect("cold start");
    assert_stored(&a.service, "first", 1, backend).await;
    a.outbox.stop().await;
    drop(a.service);
    drop(db);

    // The old runtime is fully stopped before the replacement starts.
    let db = common::provider_for_with_outbox(dsn, 8).await;
    let b = registry(&db, backend).await;
    let summary = seed(&b.service, &set, BUDGET, &cancel)
        .await
        .expect("equal restart");
    assert!(summary.up_to_date, "{backend}: nothing submitted");
    assert_stored(&b.service, "first", 1, backend).await;
    seed(&b.service, &seeds("second"), BUDGET, &cancel)
        .await
        .expect("permitted update");
    assert_stored(&b.service, "second", 2, backend).await;
    b.outbox.stop().await;
}

async fn assert_concurrent_seeding(dsn: &str, backend: &str) {
    // Each replica has its own pool. Complete migrations before starting either runtime.
    let left_db = common::provider_for_with_outbox(dsn, 8).await;
    let right_db = common::provider_for_with_outbox(dsn, 8).await;
    let a = registry(&left_db, &format!("{backend}: first replica")).await;
    let b = registry(&right_db, &format!("{backend}: second replica")).await;
    let set = seeds("first");
    seed_concurrently(&a, &b, &set, backend).await;
    assert_stored(&a.service, "first", 1, backend).await;
    assert_stored(&b.service, "first", 1, backend).await;

    // An equal reconciliation allocates nothing while both replicas are active.
    let cancel = CancellationToken::new();
    let summary = seed(&a.service, &set, BUDGET, &cancel)
        .await
        .unwrap_or_else(|e| panic!("{backend}: equal reconciliation: {e}"));
    assert!(summary.up_to_date, "{backend}: nothing submitted");
    assert_stored(&a.service, "first", 1, backend).await;

    // Both race the same permitted change: one revision.
    let changed = seeds("second");
    seed_concurrently(&a, &b, &changed, backend).await;
    assert_stored(&a.service, "second", 2, backend).await;
    assert_stored(&b.service, "second", 2, backend).await;

    a.outbox.stop().await;
    b.outbox.stop().await;
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

/// The generic MariaDB image can accept TCP before its authentication setup is
/// ready. Wait for a fully authenticated connection before running migrations.
async fn wait_for_mariadb(dsn: &str) {
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        let options = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        match connect_db(dsn, options).await {
            Ok(_) => return,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "MariaDB did not become ready: {error}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

#[tokio::test]
async fn seeding_behaves_on_sqlite_control() {
    let dir = common::TestDir::new("tr-seed-backends");
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("seed.db").display()
    );
    assert_single_instance_seeding(&dsn, "sqlite").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeding_behaves_on_postgres() {
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

    assert_concurrent_seeding(
        &format!("postgres://user:pass@{host}:{port}/app"),
        "postgres",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeding_behaves_on_mysql() {
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

    assert_concurrent_seeding(&format!("mysql://root@{host}:{port}/test"), "mysql").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seeding_behaves_on_mariadb() {
    let container = test_containers::mariadb()
        .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
        .with_exposed_port(ContainerPort::Tcp(3306))
        .with_env_var("MARIADB_ROOT_PASSWORD", "root")
        .with_env_var("MARIADB_USER", "user")
        .with_env_var("MARIADB_PASSWORD", "pass")
        .with_env_var("MARIADB_DATABASE", "app")
        .start()
        .await
        .expect("start mariadb container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mariadb port");
    let host = container
        .get_host()
        .await
        .expect("mariadb host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;
    let dsn = format!("mysql://user:pass@{host}:{port}/app");
    wait_for_mariadb(&dsn).await;
    let probe = Database::connect(&dsn)
        .await
        .expect("connect MariaDB probe");
    let row = probe
        .query_one_raw(Statement::from_string(
            DbBackend::MySql,
            "SELECT IF(@@innodb_snapshot_isolation, 'ON', 'OFF') AS snapshot_isolation",
        ))
        .await
        .expect("read MariaDB snapshot isolation")
        .expect("setting row");
    let snapshot: String = row.try_get_by_index(0).expect("snapshot isolation value");
    assert_eq!(
        snapshot, "ON",
        "exercise MariaDB snapshot-isolation conflicts"
    );
    probe.close().await.expect("close MariaDB probe");
    assert_concurrent_seeding(&dsn, "mariadb").await;
}

//! The two concurrency branches of `unit::commit_revision`, on `PostgreSQL` and
//! `MySQL`.
//!
//! `revision_test.rs` runs one admission at a time, which reaches neither branch:
//! both need a *second* admission to commit while this one's transaction is open,
//! and `SQLite` cannot produce that — a second concurrent writer fails the whole
//! transaction with `database is locked` rather than committing underneath one.
//!
//! The two branches, both opened by the same `READ COMMITTED` window between the
//! entity read and the content read:
//!
//! * **The `unchanged` re-read.** A pass that answered `unchanged` on what it read
//!   would report "your content is current" about content that no longer is. The
//!   re-read is the only thing stopping it, and deleting it leaves every
//!   single-threaded test green.
//! * **The lost compare-and-swap.** A pass whose content genuinely differs reaches
//!   the CAS, and by then `expected` no longer matches. It must answer
//!   `precondition_failed` rather than allocate a revision against a moved version.
//!
//! # The interleaving is deterministic, not raced
//!
//! Both cases pause the pass under test at the current-content read using
//! `common::PausingStores`, a decorator that delegates every call to the real
//! adapter and blocks on a channel inside that one. Nothing is faked: the second
//! admission is a real `commit_revision` on a real second connection, committing
//! before the first resumes. Racing the two instead would exercise the branches only
//! by luck, and could not tell a missing re-read from a lucky ordering.
//!
//! Gated behind `--features integration` because it needs a Docker daemon:
//!
//! ```text
//! cargo test -p cf-gears-types-registry --features integration \
//!     --test revision_race_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;

use common::{
    PausePoint, PausingStores, allow_all, provider_for, seed_current_type_schema,
    seed_operation_item, seed_pending_revision_item, stores,
};
use types_registry::domain::admission::unit::{
    EvaluatedOutcome, EvaluatedUnit, RevisionCommit, commit_revision,
};
use types_registry::domain::admission::worker::{ItemFailure, WorkerError};
use types_registry::domain::artifacts::{MaterializedArtifacts, content_hash};
use types_registry::domain::enums::{EntityKind, OwnershipScope};
use types_registry::domain::family::family_key;
use types_registry::domain::ports::{NewEntity, NewRevision, commit_write};
use types_registry::infra::storage::repo::{EntityRepo, TypeSchemaRepo, VersionFamilyRepo};

const NOW: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);

/// The candidate every case revises. One identifier per case, so the two cases
/// cannot see each other's rows.
const UNCHANGED_CASE_ID: &str = gts_id!("acme.crm.reread.type.v1~");
const CAS_CASE_ID: &str = gts_id!("acme.crm.swapped.type.v1~");

const BODY_A: &str = r#"{"$schema":"http://json-schema.org/draft-07/schema#","title":"a"}"#;
const BODY_B: &str = r#"{"$schema":"http://json-schema.org/draft-07/schema#","title":"b"}"#;
const BODY_C: &str = r#"{"$schema":"http://json-schema.org/draft-07/schema#","title":"c"}"#;

type Provider = Arc<DBProvider<DbError>>;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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

/// A Type Schema candidate for `gts_id` carrying `body`. The artifacts are
/// placeholders: `commit_revision` writes them without reading them, and what this
/// file is about is the order of the statements around them.
fn unit(gts_id: &str, body: &str, operation_item_id: i64) -> EvaluatedUnit {
    let parsed = gts::GtsId::try_new(gts_id).expect("fixture identifier");
    EvaluatedUnit {
        gts_id: parsed.id().to_owned(),
        gts_uuid: parsed.to_uuid(),
        family_key: family_key(&parsed),
        canonical_body: body.to_owned(),
        content_hash: content_hash(body),
        outcome: EvaluatedOutcome::TypeSchema {
            artifacts: MaterializedArtifacts {
                resolved_schema: body.to_owned(),
                effective_traits: "{}".to_owned(),
                effective_traits_schema: "{}".to_owned(),
                resolution_fingerprint: vec![0x11],
            },
        },
        operation_item_id,
    }
}

/// One admitted entity at `resource_version = 1` whose current revision carries
/// `BODY_A` under its **real** digest, which is what makes the `unchanged`
/// prefilter meaningful.
async fn seed_entity_at_revision_one(db: &Provider, gts_id: &str) -> i64 {
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let parsed = gts::GtsId::try_new(gts_id).expect("fixture identifier");
    let (family, _) = VersionFamilyRepo::create_or_get(
        &conn,
        &scope,
        family_key(&parsed).as_str(),
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("family");

    let entity = EntityRepo::insert(
        &conn,
        &scope,
        NewEntity {
            gts_uuid: parsed.to_uuid(),
            gts_id: parsed.id().to_owned(),
            entity_kind: EntityKind::TypeSchema,
            family_id: family.id,
            ownership_scope: OwnershipScope::Global,
            owner_tenant_id: None,
            owning_gear: Some("types-registry".to_owned()),
            now: NOW,
        },
    )
    .await
    .expect("insert")
    .expect("the identifier is free");

    let item = seed_operation_item(&conn, gts_id, 1, NOW).await;
    TypeSchemaRepo::insert_revision(
        &conn,
        &scope,
        NewRevision {
            entity_id: entity.id,
            revision_no: 1,
            raw_schema: BODY_A.to_owned(),
            content_hash: content_hash(BODY_A),
            gts_spec_version: gts::GTS_SPECIFICATION_VERSION.to_owned(),
            gts_impl_version: gts::GTS_IMPLEMENTATION_VERSION.to_owned(),
            compat_forced: false,
            operation_item_id: item,
            now: NOW,
        },
    )
    .await
    .expect("seed revision 1");
    seed_current_type_schema(&conn, entity.id, 1, BODY_A, NOW).await;
    entity.id
}

/// Run one `commit_revision` to completion in its own transaction on its own
/// connection, with the plain adapter.
async fn commit_alone(
    db: &Provider,
    gts_id: &str,
    body: &str,
    expected: i64,
) -> Result<RevisionCommit, ItemFailure> {
    let item = {
        let conn = db.conn().expect("conn");
        seed_pending_revision_item(&conn, gts_id, expected, NOW).await
    };
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let unit = Arc::new(unit(gts_id, body, item));
    let stores = stores();
    provider
        .transaction_with_config(commit_write(&db.db()), move |tx| {
            let unit = Arc::clone(&unit);
            let stores = Arc::clone(&stores);
            Box::pin(async move {
                commit_revision(
                    stores.as_ref(),
                    tx,
                    &allow_all(),
                    unit.as_ref(),
                    expected,
                    NOW,
                )
                .await
            })
        })
        .await
        .expect("a concurrency outcome is an ItemFailure, never a WorkerError")
}

/// The current `resource_version` of one entity.
async fn resource_version(db: &Provider, gts_id: &str) -> i64 {
    let conn = db.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .expect("the entity exists")
        .resource_version
}

// ---------------------------------------------------------------------------
// The two branches
// ---------------------------------------------------------------------------

/// A pass whose content equals the current revision is held at the content read
/// while a real revision commits underneath it. It must **not** report
/// `unchanged`: its answer was true when it read and false by the time it would
/// write, and the caller would be told a version that had already moved.
///
/// Mutation-checked: deleting the re-read block in `commit_revision` turns this
/// into `Unchanged { resource_version: 1 }` while the entity sits at 2.
async fn a_paused_unchanged_pass_reports_the_precondition_it_lost(db: &Provider, backend: &str) {
    seed_entity_at_revision_one(db, UNCHANGED_CASE_ID).await;

    let item = {
        let conn = db.conn().expect("conn");
        seed_pending_revision_item(&conn, UNCHANGED_CASE_ID, 1, NOW).await
    };
    let (decorated, reached, resume) = PausingStores::new(PausePoint::CurrentDocuments);
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let unit = Arc::new(unit(UNCHANGED_CASE_ID, BODY_A, item));

    let paused = tokio::spawn(async move {
        provider
            .transaction_with_config(commit_write(&provider.db()), move |tx| {
                let unit = Arc::clone(&unit);
                let stores = Arc::clone(&decorated);
                Box::pin(async move {
                    commit_revision(stores.as_ref(), tx, &allow_all(), unit.as_ref(), 1, NOW).await
                })
            })
            .await
    });

    reached.await.expect("the pass reaches the content read");

    // A real second admission on a second connection, committed while the first
    // transaction is still open. This is the commit the re-read exists to notice.
    let winner = commit_alone(db, UNCHANGED_CASE_ID, BODY_B, 1).await;
    assert!(
        matches!(winner, Ok(RevisionCommit::Admitted(c)) if c.resource_version == 2),
        "the competing revision must commit first on {backend}: {winner:?}",
    );

    resume.send(()).expect("the paused pass is still waiting");
    let outcome = paused
        .await
        .expect("task")
        .expect("a lost precondition is an ItemFailure, never a WorkerError");

    match outcome {
        Err(failure) => assert_eq!(
            failure.reason, "precondition_failed",
            "the pass must report the version it lost on {backend}",
        ),
        Ok(other) => panic!("expected a refusal on {backend}, got {other:?}"),
    }
    assert_eq!(
        resource_version(db, UNCHANGED_CASE_ID).await,
        2,
        "and it must have written nothing on {backend}",
    );
}

/// The same window, a candidate whose content genuinely differs: it passes the
/// `unchanged` test, reaches the compare-and-swap, and finds `expected` gone. The
/// precondition is in the statement's `WHERE`, so the lost race is `false` rather
/// than a second revision at a version that had moved.
async fn a_paused_revision_loses_the_compare_and_swap(db: &Provider, backend: &str) {
    seed_entity_at_revision_one(db, CAS_CASE_ID).await;

    let item = {
        let conn = db.conn().expect("conn");
        seed_pending_revision_item(&conn, CAS_CASE_ID, 1, NOW).await
    };
    let (decorated, reached, resume) = PausingStores::new(PausePoint::CurrentDocuments);
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let unit = Arc::new(unit(CAS_CASE_ID, BODY_B, item));

    let paused = tokio::spawn(async move {
        provider
            .transaction_with_config(commit_write(&provider.db()), move |tx| {
                let unit = Arc::clone(&unit);
                let stores = Arc::clone(&decorated);
                Box::pin(async move {
                    commit_revision(stores.as_ref(), tx, &allow_all(), unit.as_ref(), 1, NOW).await
                })
            })
            .await
    });

    reached.await.expect("the pass reaches the content read");

    let winner = commit_alone(db, CAS_CASE_ID, BODY_C, 1).await;
    assert!(
        matches!(winner, Ok(RevisionCommit::Admitted(c)) if c.resource_version == 2),
        "the competing revision must commit first on {backend}: {winner:?}",
    );

    resume.send(()).expect("the paused pass is still waiting");
    let outcome = paused
        .await
        .expect("task")
        .expect("a lost compare-and-swap is an ItemFailure, never a WorkerError");

    match outcome {
        Err(failure) => assert_eq!(
            failure.reason, "precondition_failed",
            "a lost compare-and-swap is terminal, not a rebase, on {backend}",
        ),
        Ok(other) => panic!("expected a refusal on {backend}, got {other:?}"),
    }
    assert_eq!(
        resource_version(db, CAS_CASE_ID).await,
        2,
        "the loser must not have advanced the version a second time on {backend}",
    );
}

/// Both cases in one body, so neither backend can drift into covering less.
async fn assert_revision_races_behave(db: &Provider, backend: &str) {
    a_paused_unchanged_pass_reports_the_precondition_it_lost(db, backend).await;
    a_paused_revision_loses_the_compare_and_swap(db, backend).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revision_races_behave_on_postgres() {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

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
        .expect("postgres container host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;

    let db = provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 8).await;
    assert_revision_races_behave(&db, "postgres").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revision_races_behave_on_mysql() {
    use testcontainers::runners::AsyncRunner;

    let container = test_containers::mysql()
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    let host = container
        .get_host()
        .await
        .expect("mysql container host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;

    let db = provider_for(&format!("mysql://root@{host}:{port}/test"), 8).await;
    assert_revision_races_behave(&db, "mysql").await;
}

//! Commit exclusion on `PostgreSQL` and `MySQL`.
//!
//! The first commit is paused after claiming `entity_write_order`; a second real
//! connection must wait and then observe the first commit. SQLite cannot distinguish
//! this lock from its own writer serialization, so only PostgreSQL and MySQL run it.
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
    ClaimSignallingStores, PausePoint, PausingStores, allow_all, provider_for,
    seed_current_type_schema, seed_operation_item, seed_pending_revision_item,
};
use types_registry::domain::admission::unit::{
    EvaluatedOutcome, EvaluatedUnit, RevisionCommit, commit_revision,
};
use types_registry::domain::admission::vector::RevisionVector;
use types_registry::domain::admission::worker::{ItemFailure, WorkerError};
use types_registry::domain::artifacts::{MaterializedArtifacts, content_hash};
use types_registry::domain::enums::{EntityKind, OwnershipScope};
use types_registry::domain::family::family_key;
use types_registry::domain::ports::{NewEntity, NewRevision, commit_write};
use types_registry::infra::storage::repo::{EntityRepo, TypeSchemaRepo, VersionFamilyRepo};

const NOW: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);

/// The candidate every case revises. One identifier per case, so the two cases
/// cannot see each other's rows.
const EXCLUSION_CASE_ID: &str = gts_id!("acme.crm.queued.type.v1~");

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
        edges: Vec::new(),
        // The vector a real evaluation of this fixture would record, spelled out: the closure over
        // the candidate's own identifier resolves to the candidate and nothing else, and nothing
        // references it, so both halves are empty.
        vector: RevisionVector::new(vec![parsed.id().to_owned()], Vec::new()),
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

/// Commit on a separate connection through caller-supplied ports.
async fn try_commit_through(
    db: &Provider,
    stores: Arc<dyn types_registry::domain::ports::Stores>,
    gts_id: &str,
    body: &str,
    expected: i64,
) -> Result<Result<RevisionCommit, ItemFailure>, WorkerError> {
    let item = {
        let conn = db.conn().expect("conn");
        seed_pending_revision_item(&conn, gts_id, expected, NOW).await
    };
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let unit = Arc::new(unit(gts_id, body, item));
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
                    common::limits().activation_write_set,
                    NOW,
                    &common::metrics(),
                )
                .await
            })
        })
        .await
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

/// The second commit waits at the claim, then reads the first commit's result.
async fn a_second_commit_waits_for_the_first(db: &Provider, backend: &str) {
    seed_entity_at_revision_one(db, EXCLUSION_CASE_ID).await;

    let item = {
        let conn = db.conn().expect("conn");
        seed_pending_revision_item(&conn, EXCLUSION_CASE_ID, 1, NOW).await
    };
    let (decorated, reached, resume) = PausingStores::new(PausePoint::CurrentDocuments);
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let unit = Arc::new(unit(EXCLUSION_CASE_ID, BODY_B, item));

    let paused = tokio::spawn(async move {
        provider
            .transaction_with_config(commit_write(&provider.db()), move |tx| {
                let unit = Arc::clone(&unit);
                let stores = Arc::clone(&decorated);
                Box::pin(async move {
                    commit_revision(
                        stores.as_ref(),
                        tx,
                        &allow_all(),
                        unit.as_ref(),
                        1,
                        common::limits().activation_write_set,
                        NOW,
                        &common::metrics(),
                    )
                    .await
                })
            })
            .await
    });

    reached.await.expect("the pass reaches the content read");

    // `expected = 2` succeeds only if this reads after the held commit.
    let (signalling, entered) = ClaimSignallingStores::new();
    let mut second = {
        let db = Arc::clone(db);
        tokio::spawn(async move {
            try_commit_through(&db, signalling, EXCLUSION_CASE_ID, BODY_C, 2).await
        })
    };
    entered
        .await
        .expect("the second commit must reach the claim");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut second)
            .await
            .is_err(),
        "and having reached it, must still be queued behind the held one on {backend}",
    );
    assert_eq!(
        resource_version(db, EXCLUSION_CASE_ID).await,
        1,
        "and nothing can have committed while the row was held, on {backend}",
    );

    resume.send(()).expect("the paused pass is still waiting");
    let first = paused
        .await
        .expect("task")
        .expect("the held pass must not fail on infrastructure");
    assert!(
        matches!(first, Ok(RevisionCommit::Admitted(c)) if c.resource_version == 2),
        "the first pass commits once released on {backend}: {first:?}",
    );

    let second = second
        .await
        .expect("task")
        .expect("the queued commit must not fail on infrastructure");
    assert!(
        matches!(second, Ok(RevisionCommit::Admitted(c)) if c.resource_version == 3),
        "and the queued commit lands against what the first left behind on {backend}: \
         {second:?}",
    );
}

/// Both cases in one body, so neither backend can drift into covering less.
async fn assert_revision_races_behave(db: &Provider, backend: &str) {
    a_second_commit_waits_for_the_first(db, backend).await;
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

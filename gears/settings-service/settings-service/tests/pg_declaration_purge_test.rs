#![cfg(feature = "integration")]
// The fixture and the seed are plain helpers, which `allow-expect-in-tests`
// does not reach; a failed expectation there is the test failing, as in the
// sibling PostgreSQL suites.
#![allow(clippy::expect_used)]
//! Purging a retired declaration on a real `PostgreSQL`.
//!
//! What `SQLite` cannot show: the locks. The purge takes the declaration row
//! `FOR UPDATE`, which conflicts with the key-share lock a staged secret's
//! foreign key takes on it and with a revive's row update, so a staged secret in
//! flight is either committed before the collect (and released) or refused its
//! insert, and a revive racing the purge either wins (the purge is then refused,
//! the declaration being active) or loses. A purge that wins against a revive makes the revive answer `404`,
//! and a purge of an evolution's predecessor leaves the entries its successor
//! still holds. The cascade, too, is the real one.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_declaration_purge_test
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cf_gears_settings_service::audit::{AuditRecord, AuditSink};
use cf_gears_settings_service::domain::category::DomainVisibility;
use cf_gears_settings_service::domain::contribution::SettingTypeRegistrar;
use cf_gears_settings_service::domain::declaration::admin::etag_of;
use cf_gears_settings_service::domain::declaration::{
    CreateDeclaration, Created, DeclarationAdmin, DeclarationRepository, PurgedDeclaration,
};
use cf_gears_settings_service::domain::error::DomainError;
use cf_gears_settings_service::domain::ports::{NoMetrics, SecretManager};
use cf_gears_settings_service::domain::resolution::EffectiveCache;
use cf_gears_settings_service::domain::stepup::{
    StepUpRefusal, StepUpRequirement, StepUpSubject, StepUpVerifier, USER_SUBJECT_TYPE,
};
use cf_gears_settings_service::domain::validation::{TraitSet, TypeValidator, ValidationResult};
use cf_gears_settings_service::domain::writes::WriteActor;
use cf_gears_settings_service::infra::storage::category_repo::CategoryRepo;
use cf_gears_settings_service::infra::storage::declaration_repo::DeclarationRepo;
use cf_gears_settings_service::infra::storage::migrations::Migrator;
use cf_gears_settings_service::infra::storage::pending_secret_repo::PendingSecretRepo;
use cf_gears_settings_service::infra::storage::value_repo::ValueRepo;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use secrecy::SecretString;
use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

mod common;
use common::pg_fixture;

/// The most a test waits for a backend to be seen waiting on a lock. An upper
/// bound only: the wait is proved by what the backend reports, not by how long
/// it took.
const POLL_LIMIT: Duration = Duration::from_secs(20);

/// Poll `pg_stat_activity` until some backend is waiting on a lock, so the test
/// opens its gate only once the competing transaction is provably blocked.
async fn wait_until_a_backend_waits_on_a_lock(conn: &DatabaseConnection) {
    let started = Instant::now();
    loop {
        let waiting = count(
            conn,
            "SELECT pid FROM pg_stat_activity
             WHERE wait_event_type = 'Lock' AND datname = current_database()",
        )
        .await;
        if waiting > 0 {
            return;
        }
        assert!(
            started.elapsed() < POLL_LIMIT,
            "no backend was seen waiting on a lock within {POLL_LIMIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A dependency a test can hold at a gate: it reports that it was reached and
/// waits until the test opens it.
struct Hook {
    reached: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

impl Hook {
    fn new() -> Self {
        Self {
            reached: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }

    async fn pass(&self) {
        self.reached.add_permits(1);
        self.release.acquire().await.expect("open").forget();
    }

    async fn wait_reached(&self) {
        tokio::time::timeout(POLL_LIMIT, self.reached.acquire())
            .await
            .expect("the gated dependency was reached")
            .expect("open")
            .forget();
    }

    fn open(&self) {
        self.release.add_permits(1);
    }
}

/// A revive does ask its collaborators: every value type is a plain one that
/// accepts what it is shown. A purge reaches none of them.
struct PlainValidator;

#[async_trait]
impl TypeValidator for PlainValidator {
    async fn validate_value(
        &self,
        _value_type_id: &str,
        _value: &Value,
    ) -> Result<ValidationResult, DomainError> {
        Ok(ValidationResult::accepted())
    }

    async fn resolve_traits(&self, _value_type_id: &str) -> Result<TraitSet, DomainError> {
        Ok(TraitSet::default())
    }
}

/// Registers the type of a revive, after holding it at the hook if there is
/// one: the revive has taken the row by then.
struct GatedRegistrar(Option<Arc<Hook>>);

#[async_trait]
impl SettingTypeRegistrar for GatedRegistrar {
    async fn register_setting_type(
        &self,
        _key: &SettingKey,
        _value_type_id: &str,
    ) -> Result<(), DomainError> {
        if let Some(hook) = &self.0 {
            hook.pass().await;
        }
        Ok(())
    }
}

struct FreshStepUp(StepUpRequirement);

#[async_trait]
impl StepUpVerifier for FreshStepUp {
    async fn verify(
        &self,
        _token: Option<&str>,
        _subject: &StepUpSubject,
    ) -> Result<(), StepUpRefusal> {
        Ok(())
    }

    fn requirement(&self) -> &StepUpRequirement {
        &self.0
    }
}

/// Appends nothing, after holding the append at the hook if there is one: the
/// purge has deleted the row and still holds it by then.
struct GatedAudit(Option<Arc<Hook>>);

#[async_trait]
impl AuditSink for GatedAudit {
    async fn append<C: DBRunner>(
        &self,
        _conn: &C,
        _scope: &AccessScope,
        _record: AuditRecord,
    ) -> Result<(), DomainError> {
        if let Some(hook) = &self.0 {
            hook.pass().await;
        }
        Ok(())
    }
}

/// A Credential Store that only remembers which references were deleted.
#[derive(Default)]
struct RecordingSecrets {
    deleted: std::sync::Mutex<Vec<String>>,
}

impl RecordingSecrets {
    fn deleted(&self) -> Vec<String> {
        let mut deleted = self.deleted.lock().expect("lock").clone();
        deleted.sort();
        deleted
    }
}

#[async_trait]
impl SecretManager for RecordingSecrets {
    fn mint_reference(&self, _key: &str, tenant: Uuid) -> String {
        format!("minted-{tenant}")
    }

    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
        _plaintext: &Value,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<SecretString, DomainError> {
        Err(DomainError::Unavailable {
            detail: "a recording store reads nothing".to_owned(),
        })
    }

    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        secret_ref: &str,
    ) -> Result<(), DomainError> {
        self.deleted
            .lock()
            .expect("lock")
            .push(secret_ref.to_owned());
        Ok(())
    }
}

type Admin =
    DeclarationAdmin<DeclarationRepo, CategoryRepo, ValueRepo, GatedAudit, PendingSecretRepo>;

fn admin() -> Arc<Admin> {
    admin_with(None, None)
}

/// An admin whose audit append (a purge's) and whose type registration (a
/// revive's) can each be held at a gate.
fn admin_with(audit: Option<Arc<Hook>>, registrar: Option<Arc<Hook>>) -> Arc<Admin> {
    admin_releasing_to(audit, registrar, Arc::new(RecordingSecrets::default()))
}

/// The same, with the Credential Store the release talks to.
fn admin_releasing_to(
    audit: Option<Arc<Hook>>,
    registrar: Option<Arc<Hook>>,
    secrets: Arc<dyn SecretManager>,
) -> Arc<Admin> {
    Arc::new(DeclarationAdmin::new(
        DeclarationRepo,
        CategoryRepo,
        ValueRepo,
        Arc::new(PlainValidator),
        Arc::new(GatedRegistrar(registrar)),
        Arc::new(FreshStepUp(StepUpRequirement {
            max_age: Duration::from_mins(5),
            acr_values: Vec::new(),
            amr_values: Vec::new(),
        })),
        GatedAudit(audit),
        Arc::new(EffectiveCache::new(Duration::from_secs(60))),
        PendingSecretRepo,
        secrets,
        Arc::new(NoMetrics),
    ))
}

fn actor() -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .subject_type(USER_SUBJECT_TYPE)
            .build()
            .expect("context"),
        request_id: "req-purge".to_owned(),
        step_up_token: Some(SecretString::from("token".to_owned())),
        visibility: DomainVisibility::Unrestricted,
    }
}

async fn exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql).await.expect("statement runs");
}

async fn count(conn: &DatabaseConnection, sql: &str) -> usize {
    conn.query_all_raw(sea_orm::Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect("readable")
    .len()
}

/// A retired, admin-authored declaration with what a purge has to take along:
/// two secret values (one with a subject), a restriction and a staged secret.
struct Seeded {
    id: Uuid,
    category: Uuid,
    tenant_a: Uuid,
    tenant_b: Uuid,
    tenant_c: Uuid,
}

async fn seed(conn: &DatabaseConnection) -> Seeded {
    let category = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO categories (id, key, name) VALUES ('{category}', 'network', 'Network')"
        ),
    )
    .await;
    let id = Uuid::new_v4();
    exec(
        conn,
        &format!(
            "INSERT INTO setting_declarations
               (id, key, leaf_slug, value_type_id, category_id, default_value,
                scope_class, status, created_by)
             VALUES ('{id}',
                     'gts.cf.core.settings.setting_type.v1~acme.settings.network.one.v1~',
                     'one', 'gts.cf.core.settings.type_bool_flag.v1~', '{category}', 'true',
                     'local', 'retired', 'fixture')"
        ),
    )
    .await;
    let (tenant_a, tenant_b, tenant_c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    exec(
        conn,
        &format!(
            "INSERT INTO setting_values
               (id, declaration_id, tenant_id, secret_ref, data_classification, set_by)
             VALUES (gen_random_uuid(), '{id}', '{tenant_a}', 'ref-value', 'secret', 'fixture');
             INSERT INTO setting_values
               (id, declaration_id, tenant_id, subject_type, subject_id, secret_ref,
                data_classification, set_by)
             VALUES (gen_random_uuid(), '{id}', '{tenant_b}', 'user', 'subject-1', 'ref-subject',
                     'secret', 'fixture');
             INSERT INTO tenant_permissions (id, declaration_id, tenant_id, access, set_by)
             VALUES (gen_random_uuid(), '{id}', '{tenant_c}', 'read_only', 'fixture');
             INSERT INTO pending_secrets
               (id, declaration_id, tenant_id, subject_id, secret_ref, expires_at)
             VALUES (gen_random_uuid(), '{id}', '{tenant_c}', 'subject-1', 'ref-staged',
                     now() + interval '10 minutes');"
        ),
    )
    .await;
    Seeded {
        id,
        category,
        tenant_a,
        tenant_b,
        tenant_c,
    }
}

/// The tag a purge presents, read as the service reads it.
async fn tag_of(db: &DBProvider<DbError>, id: Uuid) -> String {
    let conn = db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let current = DeclarationRepo
        .find(&conn, &scope, &DomainVisibility::Unrestricted, id)
        .await
        .expect("lookup")
        .expect("the declaration");
    etag_of(&current).as_str().to_owned()
}

/// One purge in its own transaction, as the handler runs it.
async fn purge(
    db: &DBProvider<DbError>,
    admin: &Arc<Admin>,
    id: Uuid,
    tag: String,
) -> Result<PurgedDeclaration, DomainError> {
    let admin = Arc::clone(admin);
    db.db()
        .transaction_ref_mapped::<_, PurgedDeclaration, DomainError>(move |tx| {
            Box::pin(async move {
                admin
                    .purge(
                        tx,
                        &AccessScope::allow_all(),
                        id,
                        Some(tag.as_str()),
                        &actor(),
                    )
                    .await
            })
        })
        .await
}

fn refs(purged: &PurgedDeclaration) -> Vec<String> {
    let mut refs: Vec<String> = purged
        .secrets
        .iter()
        .map(|e| e.secret_ref.clone())
        .collect();
    refs.sort();
    refs
}

async fn filed_under(conn: &DatabaseConnection, id: Uuid) -> [usize; 4] {
    [
        count(
            conn,
            &format!("SELECT id FROM setting_declarations WHERE id = '{id}'"),
        )
        .await,
        count(
            conn,
            &format!("SELECT id FROM setting_values WHERE declaration_id = '{id}'"),
        )
        .await,
        count(
            conn,
            &format!("SELECT id FROM tenant_permissions WHERE declaration_id = '{id}'"),
        )
        .await,
        count(
            conn,
            &format!("SELECT id FROM pending_secrets WHERE declaration_id = '{id}'"),
        )
        .await,
    ]
}

/// The real chain, the real cascade: both kinds of reference are collected —
/// the subject row's included — and nothing is left behind.
#[tokio::test]
async fn a_purge_collects_every_reference_and_the_cascade_leaves_no_row() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    let _ = (seeded.tenant_a, seeded.tenant_b, seeded.tenant_c);
    let db = DBProvider::<DbError>::new(fixture.db.clone());
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [1, 2, 1, 1]);

    let tag = tag_of(&db, seeded.id).await;
    let purged = purge(&db, &admin(), seeded.id, tag).await.expect("purged");

    assert_eq!(
        refs(&purged),
        vec!["ref-staged", "ref-subject", "ref-value"],
        "the values' references, with or without a subject, and the staged one"
    );
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [0, 0, 0, 0]);
}

/// A staged secret's insert holds a key-share lock on the declaration row until
/// it commits; the purge's `FOR UPDATE` waits for it, and so collects it.
#[tokio::test]
async fn the_lock_waits_for_a_staged_secret_in_flight_and_the_collect_sees_it() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    let db = Arc::new(DBProvider::<DbError>::new(fixture.db.clone()));
    let tag = tag_of(&db, seeded.id).await;

    let (inserted_tx, inserted_rx) = tokio::sync::oneshot::channel();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel::<()>();
    let staging = {
        let conn = fixture.conn.clone();
        let id = seeded.id;
        tokio::spawn(async move {
            let tx = conn.begin().await.expect("transaction A");
            tx.execute_unprepared(&format!(
                "INSERT INTO pending_secrets
                   (id, declaration_id, tenant_id, subject_id, secret_ref, expires_at)
                 VALUES (gen_random_uuid(), '{id}', '{tenant}', 'subject-2', 'ref-in-flight',
                         now() + interval '10 minutes')",
                tenant = Uuid::new_v4()
            ))
            .await
            .expect("staged inside A");
            inserted_tx.send(()).expect("the purge side is listening");
            commit_rx.await.expect("the test lets A commit");
            tx.commit().await.expect("A commits");
        })
    };
    inserted_rx.await.expect("A has inserted");

    let purging = {
        let (db, admin, id) = (Arc::clone(&db), admin(), seeded.id);
        tokio::spawn(async move { purge(&db, &admin, id, tag).await })
    };
    // Ordering, not wall time: A commits only once the purge's backend is seen
    // waiting on a lock.
    wait_until_a_backend_waits_on_a_lock(&fixture.conn).await;
    assert!(!purging.is_finished(), "the purge is blocked behind A");
    commit_tx.send(()).expect("A is waiting");
    let purged = purging.await.expect("joined").expect("purged");
    staging.await.expect("A finished");

    assert!(
        refs(&purged).contains(&"ref-in-flight".to_owned()),
        "the secret staged in flight is collected: {:?}",
        refs(&purged)
    );
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [0, 0, 0, 0]);
}

/// What a revive of `seed`'s declaration asks for.
fn revive_request(category: Uuid) -> CreateDeclaration {
    CreateDeclaration {
        value_type_id: "gts.cf.core.settings.type_bool_flag.v1~".to_owned(),
        vendor: "acme".to_owned(),
        name: "one".to_owned(),
        category_id: category,
        default_value: serde_json::json!(true),
        scope_class: "local".to_owned(),
        display_name: None,
        description: None,
        mode: None,
        requires_step_up: None,
        anonymous_exposable: None,
        domain_affinity: None,
        licence_feature: None,
        data_classification: None,
    }
}

/// One re-declaration in its own transaction, as the handler runs it.
async fn redeclare(
    db: &DBProvider<DbError>,
    admin: &Arc<Admin>,
    request: CreateDeclaration,
) -> Result<Created, DomainError> {
    let admin = Arc::clone(admin);
    db.db()
        .transaction_ref_mapped::<_, Created, DomainError>(move |tx| {
            Box::pin(async move {
                admin
                    .create(tx, &AccessScope::allow_all(), request, &actor())
                    .await
            })
        })
        .await
}

/// A revive holds the row until it commits; the purge then finds the
/// declaration active and refuses, deleting nothing.
#[tokio::test]
async fn a_revive_racing_a_purge_wins_and_the_purge_is_refused() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    let db = Arc::new(DBProvider::<DbError>::new(fixture.db.clone()));
    let tag = tag_of(&db, seeded.id).await;

    // The revive is held after it took the row: the registration it makes
    // next waits at the gate.
    let gate = Arc::new(Hook::new());
    let reviving = {
        let (db, admin) = (Arc::clone(&db), admin_with(None, Some(Arc::clone(&gate))));
        let request = revive_request(seeded.category);
        tokio::spawn(async move { redeclare(&db, &admin, request).await })
    };
    gate.wait_reached().await;

    let purging = {
        let (db, admin, id) = (Arc::clone(&db), admin(), seeded.id);
        tokio::spawn(async move { purge(&db, &admin, id, tag).await })
    };
    wait_until_a_backend_waits_on_a_lock(&fixture.conn).await;
    assert!(
        !purging.is_finished(),
        "the purge is blocked behind the revive"
    );
    gate.open();

    let revived = reviving.await.expect("joined").expect("the revive commits");
    assert!(revived.reactivated);
    match purging.await.expect("joined") {
        Err(DomainError::Conflict { detail }) => {
            assert!(detail.starts_with("declaration_not_retired"), "{detail}");
        }
        other => panic!("expected the not-retired refusal, got {other:?}"),
    }
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [1, 2, 1, 1]);
    assert_eq!(
        count(
            &fixture.conn,
            &format!(
                "SELECT id FROM setting_declarations WHERE id = '{}' AND status = 'active'",
                seeded.id
            )
        )
        .await,
        1,
        "the declaration is active and still there"
    );
}

/// A purge holds the row until it commits; the revive then finds it gone and
/// answers `404`, not `500`.
#[tokio::test]
async fn a_revive_that_loses_the_race_to_a_purge_answers_not_found() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    let db = Arc::new(DBProvider::<DbError>::new(fixture.db.clone()));
    let tag = tag_of(&db, seeded.id).await;

    // The purge is held at its audit append: the row is deleted in its
    // transaction and not yet committed.
    let gate = Arc::new(Hook::new());
    let purging = {
        let (db, admin, id) = (
            Arc::clone(&db),
            admin_with(Some(Arc::clone(&gate)), None),
            seeded.id,
        );
        tokio::spawn(async move { purge(&db, &admin, id, tag).await })
    };
    gate.wait_reached().await;

    let reviving = {
        let (db, admin) = (Arc::clone(&db), admin());
        let request = revive_request(seeded.category);
        tokio::spawn(async move { redeclare(&db, &admin, request).await })
    };
    wait_until_a_backend_waits_on_a_lock(&fixture.conn).await;
    assert!(
        !reviving.is_finished(),
        "the revive is blocked behind the purge"
    );
    gate.open();

    purging.await.expect("joined").expect("the purge commits");
    match reviving.await.expect("joined") {
        Err(DomainError::NotFound { resource }) => assert_eq!(resource, "declaration"),
        other => panic!("a revive that lost the race answers not found, got {other:?}"),
    }
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [0, 0, 0, 0]);
}

/// An evolution copies a predecessor's references into its successor, and the
/// store deletes by reference: the purge of the retired predecessor releases
/// only what no other declaration still holds.
#[tokio::test]
async fn a_purge_leaves_the_entries_another_declaration_still_holds() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    let successor = Uuid::new_v4();
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO setting_declarations
               (id, key, leaf_slug, value_type_id, category_id, default_value,
                scope_class, status, created_by)
             VALUES ('{successor}',
                     'gts.cf.core.settings.setting_type.v1~acme.settings.network.two.v1~',
                     'two', 'gts.cf.core.settings.type_bool_flag.v1~', '{category}', 'true',
                     'local', 'active', 'fixture');
             INSERT INTO setting_values
               (id, declaration_id, tenant_id, secret_ref, data_classification, set_by)
             VALUES (gen_random_uuid(), '{successor}', '{tenant}', 'ref-value', 'secret',
                     'fixture');
             INSERT INTO pending_secrets
               (id, declaration_id, tenant_id, subject_id, secret_ref, expires_at)
             VALUES (gen_random_uuid(), '{successor}', '{tenant}', 'subject-1', 'ref-staged',
                     now() + interval '10 minutes');",
            category = seeded.category,
            tenant = seeded.tenant_a,
        ),
    )
    .await;
    let db = DBProvider::<DbError>::new(fixture.db.clone());
    let tag = tag_of(&db, seeded.id).await;

    let purged = purge(&db, &admin(), seeded.id, tag).await.expect("purged");

    assert_eq!(
        refs(&purged),
        vec!["ref-subject"],
        "the reference only the purged declaration holds"
    );
    assert_eq!(
        purged.kept_secrets.len(),
        2,
        "the value's and the staged one are kept"
    );
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [0, 0, 0, 0]);
    assert_eq!(
        filed_under(&fixture.conn, successor).await,
        [1, 1, 0, 1],
        "the successor's rows are untouched"
    );
}

/// What the handler does after the purge commits: evict, check the kept
/// references again on a fresh connection, release.
async fn purge_and_release(
    db: &DBProvider<DbError>,
    admin: &Arc<Admin>,
    id: Uuid,
    tag: String,
) -> Result<PurgedDeclaration, DomainError> {
    let mut purged = purge(db, admin, id, tag).await?;
    let conn = db.conn().expect("connection");
    admin.reclaim_kept(&conn, &mut purged).await;
    let summary = admin.release_purged(purged.clone(), "req-purge").await;
    assert_eq!(summary.failed, 0, "the recording store refuses nothing");
    Ok(purged)
}

/// Two retired declarations that share an entry, purged together: each keeps
/// it for the other and both commit, and the check after the later commit is
/// what releases it. Nothing released it before that check existed.
#[tokio::test]
async fn two_purges_of_declarations_that_share_an_entry_still_release_it() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let seeded = seed(&fixture.conn).await;
    // A second retired declaration naming `ref-value`, as an evolution's
    // predecessor and successor do.
    let other = Uuid::new_v4();
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO setting_declarations
               (id, key, leaf_slug, value_type_id, category_id, default_value,
                scope_class, status, created_by)
             VALUES ('{other}',
                     'gts.cf.core.settings.setting_type.v1~acme.settings.network.one.v2~',
                     'one', 'gts.cf.core.settings.type_bool_flag.v1~', '{category}', 'true',
                     'local', 'retired', 'fixture');
             INSERT INTO setting_values
               (id, declaration_id, tenant_id, secret_ref, data_classification, set_by)
             VALUES (gen_random_uuid(), '{other}', '{tenant}', 'ref-value', 'secret',
                     'fixture');",
            category = seeded.category,
            tenant = seeded.tenant_a,
        ),
    )
    .await;
    let db = Arc::new(DBProvider::<DbError>::new(fixture.db.clone()));
    let store = Arc::new(RecordingSecrets::default());
    let tag_a = tag_of(&db, seeded.id).await;
    let tag_b = tag_of(&db, other).await;

    // Purge A is held at its audit append: it has collected, and not committed.
    let gate = Arc::new(Hook::new());
    let purging_a = {
        let (db, id) = (Arc::clone(&db), seeded.id);
        let admin = admin_releasing_to(Some(Arc::clone(&gate)), None, store.clone());
        tokio::spawn(async move { purge_and_release(&db, &admin, id, tag_a).await })
    };
    gate.wait_reached().await;

    // Purge B runs and commits meanwhile: A's rows are still there, so it
    // keeps the entry, and its own check still finds them.
    let admin_b = admin_releasing_to(None, None, store.clone());
    let purged_b = purge_and_release(&db, &admin_b, other, tag_b)
        .await
        .expect("purge B commits");
    assert!(refs(&purged_b).is_empty(), "{:?}", refs(&purged_b));
    assert_eq!(purged_b.kept_secrets.len(), 1, "B keeps `ref-value` for A");
    assert!(store.deleted().is_empty(), "nothing is released yet");

    gate.open();
    let purged_a = purging_a.await.expect("joined").expect("purge A commits");
    assert_eq!(
        refs(&purged_a),
        vec!["ref-staged", "ref-subject", "ref-value"],
        "A's check finds nobody holding `ref-value` and releases it"
    );
    assert!(
        purged_a.kept_secrets.is_empty(),
        "{:?}",
        purged_a.kept_secrets
    );
    assert_eq!(
        store.deleted(),
        vec!["ref-staged", "ref-subject", "ref-value"],
        "the shared entry is released, once"
    );
    assert_eq!(filed_under(&fixture.conn, seeded.id).await, [0, 0, 0, 0]);
    assert_eq!(filed_under(&fixture.conn, other).await, [0, 0, 0, 0]);
}

/// A second evolution of the active declaration, blocked behind the first on
/// the row lock, finds it retired once the first commits: the conflict an
/// evolution of a stale read gets, never a failure, and one successor only.
#[tokio::test]
async fn an_evolution_that_loses_the_race_to_another_evolution_answers_the_conflict() {
    let Some(fixture) = pg_fixture("declaration purge").await else {
        return;
    };
    run_migrations_for_testing(&fixture.db, Migrator::migrations())
        .await
        .expect("the chain applies");
    let category = Uuid::new_v4();
    let id = Uuid::new_v4();
    exec(
        &fixture.conn,
        &format!(
            "INSERT INTO categories (id, key, name) VALUES ('{category}', 'network', 'Network');
             INSERT INTO setting_declarations
               (id, key, leaf_slug, value_type_id, category_id, default_value,
                scope_class, status, created_by)
             VALUES ('{id}',
                     'gts.cf.core.settings.setting_type.v1~acme.settings.network.one.v1~',
                     'one', 'gts.cf.core.settings.type_bool_flag.v1~', '{category}', 'true',
                     'local', 'active', 'fixture');"
        ),
    )
    .await;
    let db = Arc::new(DBProvider::<DbError>::new(fixture.db.clone()));
    let mut request = revive_request(category);
    request.scope_class = "cascading".to_owned();

    // Evolution A is held at its type registration: it has locked the row,
    // retired it and not committed.
    let gate = Arc::new(Hook::new());
    let evolving_a = {
        let (db, admin) = (Arc::clone(&db), admin_with(None, Some(Arc::clone(&gate))));
        let request = request.clone();
        tokio::spawn(async move { redeclare(&db, &admin, request).await })
    };
    gate.wait_reached().await;

    let evolving_b = {
        let (db, admin) = (Arc::clone(&db), admin());
        tokio::spawn(async move { redeclare(&db, &admin, request).await })
    };
    wait_until_a_backend_waits_on_a_lock(&fixture.conn).await;
    assert!(
        !evolving_b.is_finished(),
        "the second evolution is blocked behind the first"
    );
    gate.open();

    let evolved = evolving_a
        .await
        .expect("joined")
        .expect("the first evolution commits");
    assert!(evolved.evolved);
    match evolving_b.await.expect("joined") {
        Err(DomainError::Conflict { detail }) => {
            assert!(detail.starts_with("declaration_key_conflict"), "{detail}");
            assert!(
                detail.contains("no longer the active declaration"),
                "{detail}"
            );
        }
        other => panic!("the loser answers the conflict, got {other:?}"),
    }
    assert_eq!(
        count(
            &fixture.conn,
            "SELECT id FROM setting_declarations WHERE key LIKE '%network.one.v%'"
        )
        .await,
        2,
        "the predecessor and exactly one successor"
    );
    assert_eq!(
        count(
            &fixture.conn,
            "SELECT id FROM setting_declarations
             WHERE key = 'gts.cf.core.settings.setting_type.v1~acme.settings.network.one.v2~'
               AND status = 'active'"
        )
        .await,
        1
    );
}

//! End-to-end tests of the three stages over `SQLite` with the real `m0001` /
//! `m0002` migrations, an in-memory legacy store and an in-memory V2 plugin.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use credstore::infra::storage::migrations::Migrator;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV2, SecretValue, StoreKey, TenantId, ValueVersion,
};
use credstore_value_migration::fence::{FENCE_KEY_REF, compute_fp};
use credstore_value_migration::{
    LegacyStoreError, LegacyValueStore, MigrationError, Mode, Outcome, activate, cleanup, copy,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use toolkit_db::sea_orm_migration::MigratorTrait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

type Address = (Uuid, String, Option<Uuid>);
type TargetKey = (Uuid, Uuid, String);

#[derive(Default)]
struct FakeLegacy {
    values: Mutex<HashMap<Address, Vec<u8>>>,
}

impl FakeLegacy {
    fn put(&self, tenant: Uuid, reference: &str, owner: Option<Uuid>, value: &[u8]) {
        self.values
            .lock()
            .unwrap()
            .insert((tenant, reference.to_owned(), owner), value.to_vec());
    }
    fn has(&self, tenant: Uuid, reference: &str, owner: Option<Uuid>) -> bool {
        self.values
            .lock()
            .unwrap()
            .contains_key(&(tenant, reference.to_owned(), owner))
    }
}

#[async_trait]
impl LegacyValueStore for FakeLegacy {
    async fn get(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<Option<Vec<u8>>, LegacyStoreError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(&(tenant_id, reference.to_owned(), owner_id))
            .cloned())
    }

    async fn delete(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<(), LegacyStoreError> {
        self.values
            .lock()
            .unwrap()
            .remove(&(tenant_id, reference.to_owned(), owner_id));
        Ok(())
    }
}

#[derive(Default)]
struct FakeTarget {
    values: Mutex<HashMap<TargetKey, Vec<u8>>>,
    puts: Mutex<usize>,
    /// Fail every `put` once this many have succeeded.
    fail_after: Mutex<Option<usize>>,
    /// Return other bytes from `get`.
    corrupt: Mutex<bool>,
}

impl FakeTarget {
    fn puts(&self) -> usize {
        *self.puts.lock().unwrap()
    }
}

#[async_trait]
impl CredStorePluginClientV2 for FakeTarget {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        let mut puts = self.puts.lock().unwrap();
        if let Some(limit) = *self.fail_after.lock().unwrap()
            && *puts >= limit
        {
            return Err(CredStoreError::service_unavailable("injected"));
        }
        *puts += 1;
        let version = format!("v{}", *puts);
        self.values.lock().unwrap().insert(
            (key.tenant_id.0, key.record_id, version.clone()),
            value.as_bytes().to_vec(),
        );
        Ok(ValueVersion::new(version))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        let found = self
            .values
            .lock()
            .unwrap()
            .get(&(key.tenant_id.0, key.record_id, version.0.clone()))
            .cloned();
        Ok(found.map(|mut v| {
            if *self.corrupt.lock().unwrap() {
                v.push(b'!');
            }
            SecretValue::new(v)
        }))
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        Ok(())
    }
}

const FENCE_KEY: [u8; 32] = [7; 32];
const TYPE_A: Uuid = Uuid::from_u128(0xa);
const TYPE_B: Uuid = Uuid::from_u128(0xb);

struct Row {
    id: Uuid,
    tenant: Uuid,
    reference: &'static str,
    sharing: i16,
    owner: Uuid,
    status: i16,
    fp: Option<Vec<u8>>,
    fp_key_id: Option<i16>,
    ty: Uuid,
}

impl Row {
    /// An `active`, tenant-shared row whose fingerprint matches `value`.
    fn fenced(tenant: Uuid, reference: &'static str, value: &[u8]) -> Self {
        Self {
            id: Uuid::new_v4(),
            tenant,
            reference,
            sharing: 2,
            owner: Uuid::nil(),
            status: 2,
            fp: Some(compute_fp(&FENCE_KEY, value)),
            fp_key_id: Some(1),
            ty: TYPE_A,
        }
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    db: DatabaseConnection,
    legacy: FakeLegacy,
    target: FakeTarget,
    results: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite").display()
        );
        let db = Database::connect(url).await.unwrap();
        Migrator::up(&db, Some(1)).await.unwrap();
        let legacy = FakeLegacy::default();
        legacy.put(Uuid::nil(), FENCE_KEY_REF, None, &FENCE_KEY);
        let results = dir.path().join("results.jsonl");
        Self {
            dir,
            db,
            legacy,
            target: FakeTarget::default(),
            results,
        }
    }

    async fn insert(&self, r: &Row) {
        let values: Vec<Value> = vec![
            r.id.into(),
            r.tenant.into(),
            r.reference.into(),
            r.sharing.into(),
            r.owner.into(),
            r.status.into(),
            r.ty.into(),
            r.fp.clone().into(),
            r.fp_key_id.into(),
        ];
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "INSERT INTO credstore_secrets (id, tenant_id, reference, sharing, owner_id, \
                 status, secret_type_uuid, value_fp, fp_key_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                values,
            ))
            .await
            .unwrap();
    }

    /// Inserts a fenced row and seeds its legacy value.
    async fn seed(&self, tenant: Uuid, reference: &'static str, value: &[u8]) -> Row {
        let row = Row::fenced(tenant, reference, value);
        self.insert(&row).await;
        self.legacy.put(tenant, reference, None, value);
        row
    }

    async fn copy(
        &self,
        mode: Mode,
    ) -> Result<credstore_value_migration::CopyReport, MigrationError> {
        copy(&self.db, &self.legacy, &self.target, &self.results, mode).await
    }

    async fn m0002(&self) {
        Migrator::up(&self.db, None).await.unwrap();
    }

    /// `(status, value_version, fallback)` of a row after `m0002`.
    async fn state(&self, id: Uuid) -> Option<(i16, Option<String>, i16)> {
        let row = self
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "SELECT status, value_version, fallback FROM credstore_secrets WHERE id = ?1",
                vec![id.into()],
            ))
            .await
            .unwrap()?;
        Some((
            row.try_get("", "status").unwrap(),
            row.try_get("", "value_version").unwrap(),
            row.try_get("", "fallback").unwrap(),
        ))
    }

    async fn read_back(&self, tenant: Uuid, id: Uuid, version: &str) -> Option<Vec<u8>> {
        let ctx = SecurityContext::anonymous();
        self.target
            .get(
                &ctx,
                &StoreKey::new(TenantId(tenant), id),
                &ValueVersion::new(version),
            )
            .await
            .unwrap()
            .map(|v| v.as_bytes().to_vec())
    }
}

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(0x1000 + n)
}

#[tokio::test]
async fn happy_path_copy_m0002_activate() {
    let f = Fixture::new().await;
    let shared = f.seed(tenant(1), "api-key", b"shared-value").await;
    // Private row: the legacy address carries the owner.
    let owner = Uuid::new_v4();
    let mut private = Row::fenced(tenant(1), "api-key", b"private-value");
    private.sharing = 1;
    private.owner = owner;
    f.insert(&private).await;
    f.legacy
        .put(tenant(1), "api-key", Some(owner), b"private-value");
    // Out-of-band seeded row: no fingerprint.
    let mut seeded = Row::fenced(tenant(2), "seeded", b"x");
    seeded.fp = None;
    seeded.fp_key_id = None;
    f.insert(&seeded).await;
    f.legacy.put(tenant(2), "seeded", None, b"seeded-value");
    // Unfinished rows.
    let mut provisioning = Row::fenced(tenant(2), "half-written", b"x");
    provisioning.status = 1;
    f.insert(&provisioning).await;
    let mut deprovisioning = Row::fenced(tenant(2), "half-deleted", b"x");
    deprovisioning.status = 3;
    f.insert(&deprovisioning).await;

    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!(report.total_rows, 5);
    assert_eq!((report.copied, report.unverified), (2, 1));
    assert_eq!(report.unfinished.len(), 2);
    assert!(!report.has_losses());
    assert!(report.fence_key_present);

    f.m0002().await;
    let report = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!(report.promoted, 3);
    assert_eq!(report.ignored_unfinished, 2);
    assert!(report.unknown_ids.is_empty() && report.unexpected.is_empty());

    for (row, value) in [
        (&shared, &b"shared-value"[..]),
        (&private, b"private-value"),
        (&seeded, b"seeded-value"),
    ] {
        let (status, version, fallback) = f.state(row.id).await.unwrap();
        assert_eq!((status, fallback), (2, 1));
        let version = version.unwrap();
        assert_eq!(
            f.read_back(row.tenant, row.id, &version).await.unwrap(),
            value
        );
    }
    assert!(f.state(provisioning.id).await.is_none());

    // The stage is idempotent.
    let again = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!((again.promoted, again.already_done), (0, 3));
}

#[tokio::test]
async fn fp_mismatch_is_not_copied_and_suppressed_by_activate() {
    let f = Fixture::new().await;
    let row = f.seed(tenant(1), "k", b"expected").await;
    f.legacy.put(tenant(1), "k", None, b"foreign");

    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!(report.fp_mismatch.len(), 1);
    assert!(report.has_losses());
    assert_eq!(f.target.puts(), 0);

    f.m0002().await;
    let report = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!((report.promoted, report.suppressed), (0, 1));
    assert_eq!(f.state(row.id).await.unwrap(), (4, None, 2));
    let again = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!((again.suppressed, again.already_done), (0, 1));
}

#[tokio::test]
async fn missing_value_is_reported_and_suppressed() {
    let f = Fixture::new().await;
    let row = Row::fenced(tenant(1), "gone", b"x");
    f.insert(&row).await;

    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!(report.missing.len(), 1);
    f.m0002().await;
    let report = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!(report.suppressed, 1);
    assert_eq!(f.state(row.id).await.unwrap(), (4, None, 2));
}

#[tokio::test]
async fn unknown_fence_key_id_is_not_copied() {
    let f = Fixture::new().await;
    let mut row = Row::fenced(tenant(1), "k", b"v");
    row.fp_key_id = Some(2);
    f.insert(&row).await;
    f.legacy.put(tenant(1), "k", None, b"v");

    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!(report.unknown_fence_key.len(), 1);
    assert_eq!(f.target.puts(), 0);
}

#[tokio::test]
async fn absent_fence_key_with_fingerprinted_rows_aborts() {
    let f = Fixture::new().await;
    f.seed(tenant(1), "k", b"v").await;
    f.legacy
        .delete(Uuid::nil(), FENCE_KEY_REF, None)
        .await
        .unwrap();

    let err = f.copy(Mode::Apply).await.unwrap_err();
    assert!(matches!(err, MigrationError::FenceKeyAbsent { rows: 1 }));
    assert!(!f.results.exists());
}

#[tokio::test]
async fn absent_fence_key_is_fine_when_no_row_has_a_fingerprint() {
    let f = Fixture::new().await;
    let mut row = Row::fenced(tenant(1), "k", b"v");
    row.fp = None;
    row.fp_key_id = None;
    f.insert(&row).await;
    f.legacy.put(tenant(1), "k", None, b"v");
    f.legacy
        .delete(Uuid::nil(), FENCE_KEY_REF, None)
        .await
        .unwrap();

    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!((report.unverified, report.fence_key_present), (1, false));
}

#[tokio::test]
async fn read_back_mismatch_aborts() {
    let f = Fixture::new().await;
    f.seed(tenant(1), "k", b"v").await;
    *f.target.corrupt.lock().unwrap() = true;

    let err = f.copy(Mode::Apply).await.unwrap_err();
    assert!(matches!(err, MigrationError::ReadBack { .. }));
}

#[tokio::test]
async fn copy_restart_skips_recorded_ids() {
    let f = Fixture::new().await;
    for (n, name) in ["a", "b", "c"].into_iter().enumerate() {
        f.seed(tenant(n as u128), name, name.as_bytes()).await;
    }
    *f.target.fail_after.lock().unwrap() = Some(1);
    assert!(matches!(
        f.copy(Mode::Apply).await.unwrap_err(),
        MigrationError::Target(_)
    ));
    assert_eq!(f.target.puts(), 1);

    *f.target.fail_after.lock().unwrap() = None;
    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!((report.resumed, report.copied), (1, 3));
    assert_eq!(f.target.puts(), 3);

    let third = f.copy(Mode::Apply).await.unwrap();
    assert_eq!((third.resumed, f.target.puts()), (3, 3));
}

#[tokio::test]
async fn torn_final_line_of_the_results_file_is_discarded() {
    let f = Fixture::new().await;
    f.seed(tenant(1), "a", b"a").await;
    f.seed(tenant(2), "b", b"b").await;
    *f.target.fail_after.lock().unwrap() = Some(1);
    f.copy(Mode::Apply).await.unwrap_err();
    // Simulate a crash in the middle of the next write.
    let mut text = std::fs::read_to_string(&f.results).unwrap();
    text.push_str("{\"id\":\"0000");
    std::fs::write(&f.results, text).unwrap();

    *f.target.fail_after.lock().unwrap() = None;
    let report = f.copy(Mode::Apply).await.unwrap();
    assert_eq!((report.resumed, report.copied), (1, 2));
    let lines = std::fs::read_to_string(&f.results).unwrap();
    assert_eq!(lines.lines().count(), 3);
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let f = Fixture::new().await;
    let row = f.seed(tenant(1), "k", b"v").await;
    let report = f.copy(Mode::DryRun).await.unwrap();
    assert_eq!(report.copied, 1);
    assert!(!f.results.exists());
    assert_eq!(f.target.puts(), 0);
    assert!(f.legacy.has(tenant(1), "k", None));

    f.copy(Mode::Apply).await.unwrap();
    f.m0002().await;
    let report = activate(&f.db, &f.results, Mode::DryRun).await.unwrap();
    assert_eq!(report.promoted, 1);
    assert_eq!(f.state(row.id).await.unwrap().0, 4);

    let before = std::fs::read_to_string(&f.results).unwrap();
    let report = cleanup(&f.legacy, &f.results, Mode::DryRun, true)
        .await
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert!(f.legacy.has(tenant(1), "k", None));
    assert!(f.legacy.has(Uuid::nil(), FENCE_KEY_REF, None));
    assert_eq!(std::fs::read_to_string(&f.results).unwrap(), before);
}

#[tokio::test]
async fn results_file_never_contains_values_or_fingerprints() {
    let f = Fixture::new().await;
    let row = f.seed(tenant(1), "k", b"super-secret-value").await;
    f.copy(Mode::Apply).await.unwrap();

    let text = std::fs::read_to_string(&f.results).unwrap();
    assert!(!text.contains("super-secret-value"));
    for b in row.fp.unwrap() {
        // No fingerprint byte sequence (hex or array form) either.
        assert!(!text.contains(&format!("{b:02x}{b:02x}{b:02x}")));
    }
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["format"], "credstore-value-migration");
    assert_eq!(header["version"], 1);
    let entry: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(entry["outcome"], "copied");
    assert_eq!(entry["value_version"], "v1");
    assert!(entry.get("owner_id").is_none());
}

#[tokio::test]
async fn type_divergent_pairs_are_reported() {
    let f = Fixture::new().await;
    let shared = f.seed(tenant(1), "k", b"a").await;
    let mut private = Row::fenced(tenant(1), "k", b"b");
    private.sharing = 1;
    private.owner = Uuid::new_v4();
    private.ty = TYPE_B;
    f.insert(&private).await;
    f.legacy.put(tenant(1), "k", Some(private.owner), b"b");
    // Same type: not reported.
    f.seed(tenant(2), "same", b"c").await;
    let mut same = Row::fenced(tenant(2), "same", b"d");
    same.sharing = 1;
    same.owner = Uuid::new_v4();
    f.insert(&same).await;
    f.legacy.put(tenant(2), "same", Some(same.owner), b"d");

    for mode in [Mode::DryRun, Mode::Apply] {
        let report = f.copy(mode).await.unwrap();
        assert_eq!(report.type_divergent.len(), 1);
        let d = &report.type_divergent[0];
        assert_eq!((d.private_row, d.nonprivate_row), (private.id, shared.id));
        assert_eq!((d.private_type, d.nonprivate_type), (TYPE_B, TYPE_A));
    }
}

#[tokio::test]
async fn cleanup_deletes_copied_and_unfinished_keeps_evidence_and_fence_key() {
    let f = Fixture::new().await;
    f.seed(tenant(1), "ok", b"v").await;
    let bad = f.seed(tenant(1), "bad", b"v").await;
    f.legacy.put(tenant(1), "bad", None, b"foreign");
    let mut half = Row::fenced(tenant(1), "half", b"x");
    half.status = 3;
    f.insert(&half).await;
    f.legacy.put(tenant(1), "half", None, b"x");
    f.copy(Mode::Apply).await.unwrap();
    f.m0002().await;
    activate(&f.db, &f.results, Mode::Apply).await.unwrap();

    let report = cleanup(&f.legacy, &f.results, Mode::Apply, false)
        .await
        .unwrap();
    assert_eq!(report.deleted, 2);
    assert_eq!(report.kept_as_evidence.len(), 1);
    assert_eq!(report.kept_as_evidence[0].id, bad.id);
    assert!(!f.legacy.has(tenant(1), "ok", None));
    assert!(!f.legacy.has(tenant(1), "half", None));
    assert!(f.legacy.has(tenant(1), "bad", None));
    assert!(f.legacy.has(Uuid::nil(), FENCE_KEY_REF, None));
    assert!(!report.fence_key_deleted);

    // Re-run is safe; the flag deletes the fence key last.
    let report = cleanup(&f.legacy, &f.results, Mode::Apply, true)
        .await
        .unwrap();
    assert!(report.fence_key_deleted);
    assert!(!f.legacy.has(Uuid::nil(), FENCE_KEY_REF, None));
    assert!(f.legacy.has(tenant(1), "bad", None));
}

#[tokio::test]
async fn cleanup_refuses_a_reference_shaped_like_a_new_key() {
    let f = Fixture::new().await;
    let first = f.seed(tenant(1), "plain", b"v").await;
    let uuid_ref: &'static str = Box::leak(first.id.to_string().into_boxed_str());
    f.seed(tenant(1), uuid_ref, b"w").await;
    f.copy(Mode::Apply).await.unwrap();

    let err = cleanup(&f.legacy, &f.results, Mode::Apply, false)
        .await
        .unwrap_err();
    assert!(matches!(err, MigrationError::NewKeyShaped { .. }));
    assert!(f.legacy.has(tenant(1), "plain", None));
    assert!(f.legacy.has(tenant(1), uuid_ref, None));
}

#[tokio::test]
async fn stages_check_the_schema_generation() {
    let f = Fixture::new().await;
    f.seed(tenant(1), "k", b"v").await;
    f.copy(Mode::Apply).await.unwrap();
    assert!(matches!(
        activate(&f.db, &f.results, Mode::Apply).await.unwrap_err(),
        MigrationError::WrongSchema(_)
    ));
    f.m0002().await;
    assert!(matches!(
        f.copy(Mode::Apply).await.unwrap_err(),
        MigrationError::WrongSchema(_)
    ));
}

#[tokio::test]
async fn activate_reports_rows_deleted_in_the_meantime() {
    let f = Fixture::new().await;
    let row = f.seed(tenant(1), "k", b"v").await;
    f.seed(tenant(1), "other", b"v").await;
    f.copy(Mode::Apply).await.unwrap();
    f.m0002().await;
    f.db.execute_raw(Statement::from_sql_and_values(
        f.db.get_database_backend(),
        "DELETE FROM credstore_secrets WHERE id = ?1",
        vec![row.id.into()],
    ))
    .await
    .unwrap();

    let report = activate(&f.db, &f.results, Mode::Apply).await.unwrap();
    assert_eq!((report.promoted, report.unknown_ids), (1, vec![row.id]));
}

#[test]
fn outcome_names_are_stable() {
    assert_eq!(Outcome::FpMismatch.as_str(), "fp_mismatch");
    assert!(Outcome::Unverified.is_copied());
    assert!(Outcome::Missing.is_loss());
}

#[tokio::test]
#[allow(clippy::use_debug)]
async fn cli_exit_codes_gate_on_losses() {
    use std::process::ExitCode;
    use std::sync::Arc;

    let f = Fixture::new().await;
    f.insert(&Row::fenced(tenant(1), "gone", b"x")).await;
    let url = format!("sqlite://{}", f.dir.path().join("db.sqlite").display());
    let legacy = Arc::new(FakeLegacy::default());
    legacy.put(Uuid::nil(), FENCE_KEY_REF, None, &FENCE_KEY);
    let target = Arc::new(FakeTarget::default());
    let results = f.results.display().to_string();
    let run = |extra: &'static [&'static str]| {
        let mut args = vec![
            "migrate",
            "--database-url",
            &url,
            "--results",
            &results,
            "copy",
        ];
        args.extend_from_slice(extra);
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let (legacy, target) = (legacy.clone(), target.clone());
        async move {
            let code = credstore_value_migration::run_cli_from(args, Some(legacy), Some(target))
                .await
                .unwrap();
            format!("{code:?}")
        }
    };
    assert_eq!(run(&[]).await, format!("{:?}", ExitCode::from(2)));
    assert!(!f.results.exists());
    assert_eq!(run(&["--apply"]).await, format!("{:?}", ExitCode::from(2)));
    assert_eq!(
        run(&["--apply", "--accept-losses"]).await,
        format!("{:?}", ExitCode::SUCCESS)
    );
}

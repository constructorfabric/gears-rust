//! The feature 04 fixture: the intake and a reader over the contract fakes, the PDP fake and a
//! recording attribution step, on a database of the caller's choice.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    dead_code,
    reason = "test fixtures: each integration test binary uses a different subset"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bss_rating::authz::ServiceGrant;
use bss_rating::domain::error::DomainError;
use bss_rating::domain::exception::{ExceptionRegister, SubjectKind};
use bss_rating::domain::ports::{Clock, TenantGrant};
use bss_rating::domain::usage::declarations::DeclarationCache;
use bss_rating::domain::usage::entry::{self, EntryIdentity};
use bss_rating::domain::usage::feed::FeedSubscription;
use bss_rating::domain::usage::intake::{IntakeParts, PageEntry, PageReport, UsageIntake};
use bss_rating::domain::usage::loss::SourceLoss;
use bss_rating::domain::usage::metrics::UsageMetricsPort;
use bss_rating::domain::usage::normalize::NormalizedUsage;
use bss_rating::domain::usage::normalize::{NaturalKey, RejectReason};
use bss_rating::domain::usage::reader::{
    FeedGrant, ReaderParts, ReaderSettings, SourceSession, Step, TenantHierarchy, UsageFeedReader,
};
use bss_rating::domain::usage::source::{Checkpoint, SourceDefinition, SourceId, TenantScope};
use bss_rating::domain::usage::source::{SourceState, Transition};
use bss_rating::domain::usage::{
    CaptureReject, CheckpointRepo, ContributionSink, ContributionState, NewUsageRecord,
    StoredDigest, StoredUsageRecord, UsageRecordRepo,
};
use bss_rating::infra::feed_grant::PdpFeedGrant;
use bss_rating::infra::storage::entity::usage_record;
use bss_rating::infra::storage::repo::exception_repo::{self, OrmExceptionRepo};
use bss_rating::infra::storage::repo::usage_repo::{self, OrmUsageRepo};
use bss_rating::test_providers::usage::{RecordSpec, meter, qty, record_entry, sum_declaration};
use bss_rating::test_providers::{
    FakeMeterDeclarations, FakePdp, FakeQueueDepth, FakeTenantHierarchy, FakeUsageFeed,
    RecordingMetrics,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_db::Db;
use toolkit_db::secure::DbTx;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::TestDb;

pub const T1: Uuid = Uuid::from_u128(0x10);
pub const T2: Uuid = Uuid::from_u128(0x20);
pub const HOUR: &str = "2026-10-08T10:00:00Z";

/// A clock the test moves.
pub struct TestClock(Mutex<OffsetDateTime>);

impl TestClock {
    pub fn at(text: &str) -> Arc<Self> {
        Arc::new(Self(Mutex::new(
            OffsetDateTime::parse(text, &Rfc3339).unwrap(),
        )))
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}

/// The attribution step, recorded: what was handed on and what withdrawn, and a switch that
/// fails it.
#[derive(Default)]
pub struct Contributions {
    interpreted: Mutex<Vec<Uuid>>,
    withdrawn: Mutex<Vec<(Uuid, ContributionState)>>,
    pub fail: AtomicBool,
}

impl Contributions {
    pub fn interpreted(&self) -> Vec<Uuid> {
        self.interpreted.lock().unwrap().clone()
    }

    pub fn withdrawn(&self) -> Vec<(Uuid, ContributionState)> {
        self.withdrawn.lock().unwrap().clone()
    }
}

#[async_trait]
impl ContributionSink for Contributions {
    async fn interpret(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        record: &NormalizedUsage,
    ) -> Result<(), DomainError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(DomainError::Internal(
                "the test failed the page".to_owned(),
                None,
            ));
        }
        self.interpreted
            .lock()
            .unwrap()
            .push(record.identity.usage_record_id);
        Ok(())
    }

    async fn withdraw(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        _invalidation: Uuid,
    ) -> Result<(), DomainError> {
        self.withdrawn
            .lock()
            .unwrap()
            .push((target.usage_record_id, from));
        Ok(())
    }
}

/// The feed-read grant, scripted: the PDP's answer until call `refuse_from` (counted from 1), a
/// refusal from then on.
pub struct ScriptedGrant {
    inner: PdpFeedGrant,
    calls: AtomicUsize,
    refuse_from: AtomicUsize,
}

impl ScriptedGrant {
    /// Refuses from the `nth` call after the ones made so far (1 = the next call).
    pub fn refuse_from_call(&self, nth: usize) {
        let made = self.calls.load(Ordering::SeqCst);
        self.refuse_from.store(made + nth, Ordering::SeqCst);
    }

    /// Answers as the PDP does again.
    pub fn follow_the_pdp(&self) {
        self.refuse_from.store(usize::MAX, Ordering::SeqCst);
    }
}

#[async_trait]
impl FeedGrant for ScriptedGrant {
    async fn covers(&self, source: &SourceDefinition) -> Result<bool, DomainError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call >= self.refuse_from.load(Ordering::SeqCst) {
            return Ok(false);
        }
        self.inner.covers(source).await
    }
}

/// The usage repository, observed: it counts the lookups interpretation makes and can lose the
/// conditional writes a concurrent writer would win.
#[derive(Default)]
pub struct ProbedRepo {
    pub key_lookups: AtomicUsize,
    pub lose_withdrawals: AtomicBool,
    pub lose_retirements: AtomicBool,
}

#[async_trait]
impl UsageRecordRepo for ProbedRepo {
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
    ) -> Result<Option<StoredUsageRecord>, DomainError> {
        OrmUsageRepo.find(tx, scope, identity).await
    }

    async fn stored_digests(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<StoredDigest>, DomainError> {
        OrmUsageRepo.stored_digests(tx, scope, tenant_id, ids).await
    }

    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        record: &NewUsageRecord,
    ) -> Result<bool, DomainError> {
        UsageRecordRepo::insert(&OrmUsageRepo, tx, scope, record).await
    }

    async fn natural_key_holder(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &NaturalKey,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        self.key_lookups.fetch_add(1, Ordering::SeqCst);
        OrmUsageRepo
            .natural_key_holder(tx, scope, key, except)
            .await
    }

    async fn invalidation_naming(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        except: Uuid,
    ) -> Result<Option<Uuid>, DomainError> {
        self.key_lookups.fetch_add(1, Ordering::SeqCst);
        OrmUsageRepo
            .invalidation_naming(tx, scope, target, except)
            .await
    }

    async fn withdraw(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        target: EntryIdentity,
        from: ContributionState,
        by: Uuid,
        at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        if self.lose_withdrawals.load(Ordering::SeqCst) {
            return Ok(false);
        }
        OrmUsageRepo.withdraw(tx, scope, target, from, by, at).await
    }

    async fn complete_retry(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        normalized: &NormalizedUsage,
        to: ContributionState,
    ) -> Result<bool, DomainError> {
        OrmUsageRepo.complete_retry(tx, scope, normalized, to).await
    }

    async fn reject_again(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        identity: EntryIdentity,
        reason: RejectReason,
    ) -> Result<bool, DomainError> {
        OrmUsageRepo.reject_again(tx, scope, identity, reason).await
    }

    async fn capture_rejected(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        raw_sha256: &bss_rating::domain::digest::Sha256Hex,
    ) -> Result<bool, DomainError> {
        OrmUsageRepo.capture_rejected(tx, source, raw_sha256).await
    }

    async fn insert_capture_reject(
        &self,
        tx: &DbTx<'_>,
        reject: &CaptureReject,
    ) -> Result<bool, DomainError> {
        OrmUsageRepo.insert_capture_reject(tx, reject).await
    }
}

#[async_trait]
impl CheckpointRepo for ProbedRepo {
    async fn lock_usage(&self, tx: &DbTx<'_>) -> Result<Vec<Checkpoint>, DomainError> {
        OrmUsageRepo.lock_usage(tx).await
    }

    async fn lock(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
    ) -> Result<Option<Checkpoint>, DomainError> {
        OrmUsageRepo.lock(tx, source).await
    }

    async fn insert(&self, tx: &DbTx<'_>, checkpoint: &Checkpoint) -> Result<bool, DomainError> {
        CheckpointRepo::insert(&OrmUsageRepo, tx, checkpoint).await
    }

    async fn set_cursor(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        cursor: Option<&bss_rating::domain::usage::feed::FeedCursor>,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        OrmUsageRepo.set_cursor(tx, source, cursor, now).await
    }

    async fn set_state(
        &self,
        tx: &DbTx<'_>,
        source: SourceId,
        transition: Transition,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        if transition.to == SourceState::Retired && self.lose_retirements.load(Ordering::SeqCst) {
            return Ok(false);
        }
        OrmUsageRepo.set_state(tx, source, transition, now).await
    }

    async fn stamp_generation(&self, tx: &DbTx<'_>, generation: u64) -> Result<(), DomainError> {
        OrmUsageRepo.stamp_generation(tx, generation).await
    }
}

pub struct Fixture {
    pub test_db: TestDb,
    pub pdp: Arc<FakePdp>,
    pub feed_grant: Arc<ScriptedGrant>,
    pub feed: Arc<FakeUsageFeed>,
    pub registry: Arc<FakeMeterDeclarations>,
    pub declarations: DeclarationCache,
    pub hierarchy: Arc<FakeTenantHierarchy>,
    pub depth: Arc<FakeQueueDepth>,
    pub metrics: Arc<RecordingMetrics>,
    pub clock: Arc<TestClock>,
    pub contributions: Arc<Contributions>,
    pub grant: Arc<dyn TenantGrant>,
    pub intake: UsageIntake,
    pub reader: UsageFeedReader,
    /// One reading term per source, kept across [`step`] calls as a leader's term keeps it.
    pub sessions: tokio::sync::Mutex<BTreeMap<SourceId, SourceSession>>,
}

impl Fixture {
    pub fn db(&self) -> &Db {
        &self.test_db.db
    }

    /// The parts of the fixture's reader, for a test that builds its own.
    pub fn reader_parts(&self) -> ReaderParts {
        reader_parts(
            &self.test_db.db,
            &self.feed,
            &self.declarations,
            &self.feed_grant,
            &self.hierarchy,
            &self.grant,
            &self.depth,
            &self.intake,
            &self.metrics,
            &self.clock,
        )
    }

    /// The parts of the fixture's intake, for a test that builds its own.
    pub fn intake_parts(&self) -> IntakeParts {
        intake_parts(&self.clock, &self.contributions)
    }
}

fn intake_parts(clock: &Arc<TestClock>, contributions: &Arc<Contributions>) -> IntakeParts {
    IntakeParts {
        checkpoints: Arc::new(OrmUsageRepo),
        records: Arc::new(OrmUsageRepo),
        losses: Arc::new(OrmUsageRepo),
        exceptions: ExceptionRegister::new(Arc::new(OrmExceptionRepo), clock.clone()),
        contributions: contributions.clone(),
        clock: clock.clone(),
        retention_floor: Duration::from_hours(125 * 24),
    }
}

#[allow(clippy::too_many_arguments, reason = "the fixture's parts, wired once")]
fn reader_parts(
    db: &Db,
    feed: &Arc<FakeUsageFeed>,
    declarations: &DeclarationCache,
    feed_grant: &Arc<ScriptedGrant>,
    hierarchy: &Arc<FakeTenantHierarchy>,
    grant: &Arc<dyn TenantGrant>,
    depth: &Arc<FakeQueueDepth>,
    intake: &UsageIntake,
    metrics: &Arc<RecordingMetrics>,
    clock: &Arc<TestClock>,
) -> ReaderParts {
    ReaderParts {
        db: db.clone(),
        feed: feed.clone(),
        declarations: declarations.clone(),
        feed_grant: feed_grant.clone() as Arc<dyn FeedGrant>,
        hierarchy: hierarchy.clone() as Arc<dyn TenantHierarchy>,
        tenant_grant: Arc::clone(grant),
        depth: depth.clone(),
        intake: intake.clone(),
        checkpoints: Arc::new(OrmUsageRepo),
        losses: Arc::new(OrmUsageRepo),
        metrics: metrics.clone() as Arc<dyn UsageMetricsPort>,
        clock: clock.clone(),
        settings: settings(),
    }
}

pub fn settings() -> ReaderSettings {
    ReaderSettings::new(
        3,
        Duration::from_millis(10),
        100,
        Duration::from_millis(50),
        Duration::from_secs(5),
    )
    .unwrap()
}

pub async fn fixture() -> Fixture {
    fixture_over(super::migrated_db().await)
}

/// The fixture over `test_db` (SQLite or PostgreSQL).
pub fn fixture_over(test_db: TestDb) -> Fixture {
    let pdp = FakePdp::new();
    let feed = Arc::new(FakeUsageFeed::new());
    let registry = Arc::new(FakeMeterDeclarations::new());
    for name in ["hours", "gb"] {
        registry.declare(&meter(name), sum_declaration("h", &[]));
    }
    let declarations = DeclarationCache::new(registry.clone(), Duration::from_secs(5));
    let hierarchy = Arc::new(FakeTenantHierarchy::new());
    let depth = Arc::new(FakeQueueDepth::new());
    let metrics = Arc::new(RecordingMetrics::default());
    let clock = TestClock::at("2026-10-08T12:00:00Z");
    let contributions = Arc::new(Contributions::default());
    let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(pdp.enforcer()));
    let feed_grant = Arc::new(ScriptedGrant {
        inner: PdpFeedGrant::new(pdp.enforcer()),
        calls: AtomicUsize::new(0),
        refuse_from: AtomicUsize::new(usize::MAX),
    });
    let intake = UsageIntake::new(intake_parts(&clock, &contributions));
    let reader = UsageFeedReader::new(reader_parts(
        &test_db.db,
        &feed,
        &declarations,
        &feed_grant,
        &hierarchy,
        &grant,
        &depth,
        &intake,
        &metrics,
        &clock,
    ));
    Fixture {
        test_db,
        pdp,
        feed_grant,
        feed,
        registry,
        declarations,
        hierarchy,
        depth,
        metrics,
        clock,
        contributions,
        grant,
        intake,
        reader,
        sessions: tokio::sync::Mutex::new(BTreeMap::new()),
    }
}

pub fn source(meters: &[&str], roots: &[Uuid]) -> SourceDefinition {
    SourceDefinition::new(
        FeedSubscription::new(meters.iter().map(|name| meter(name))).unwrap(),
        TenantScope::new(roots.iter().copied()).unwrap(),
    )
}

/// Reconciles the stored sources with `configured`, as a gear instance does when it starts.
pub async fn configure(f: &Fixture, configured: &[SourceDefinition]) -> Vec<Checkpoint> {
    configure_at(f, configured, 0).await
}

/// [`configure`] under configuration `generation`.
pub async fn configure_at(
    f: &Fixture,
    configured: &[SourceDefinition],
    generation: u64,
) -> Vec<Checkpoint> {
    let mut scopes = BTreeMap::new();
    for definition in configured {
        let owner = definition.tenant_scope().owner();
        scopes.insert(owner, f.grant.scope_for(owner).await.unwrap());
    }
    f.intake
        .reconcile_sources(f.db(), configured.to_vec(), generation, scopes)
        .await
        .unwrap()
}

pub async fn scope(f: &Fixture, tenant: Uuid) -> AccessScope {
    f.grant.scope_for(tenant).await.unwrap()
}

pub async fn checkpoint(f: &Fixture, definition: &SourceDefinition) -> Checkpoint {
    let id = definition.source_id();
    f.db()
        .transaction_ref_mapped(move |tx| Box::pin(async move { OrmUsageRepo.lock(tx, id).await }))
        .await
        .unwrap()
        .unwrap()
}

pub async fn stored(f: &Fixture, entry: &Value) -> Option<usage_record::Model> {
    let identity = identity(entry);
    let scope = scope(f, identity.tenant_id).await;
    let conn = f.db().conn().unwrap();
    usage_repo::find_record_row(&conn, &scope, identity)
        .await
        .unwrap()
}

pub async fn records(f: &Fixture, tenant: Uuid) -> Vec<usage_record::Model> {
    let scope = scope(f, tenant).await;
    let conn = f.db().conn().unwrap();
    usage_repo::list_records(&conn, &scope, tenant)
        .await
        .unwrap()
}

pub async fn losses(f: &Fixture, definition: &SourceDefinition) -> Vec<SourceLoss> {
    let scope = scope(f, definition.tenant_scope().owner()).await;
    let conn = f.db().conn().unwrap();
    usage_repo::list_losses(&conn, &scope, definition.source_id())
        .await
        .unwrap()
}

/// The open exceptions of `tenant`'s usage records, as `(subject_ref, reason_code)`.
pub async fn open_exceptions(f: &Fixture, tenant: Uuid) -> Vec<(String, String)> {
    let scope = scope(f, tenant).await;
    f.db()
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                exception_repo::list_open(tx, &scope, &[SubjectKind::UsageRecord], None, 100).await
            })
        })
        .await
        .unwrap()
        .into_iter()
        .map(|record| {
            (
                record.key.subject_ref().to_owned(),
                record.key.reason_code().to_owned(),
            )
        })
        .collect()
}

pub fn identity(entry: &Value) -> EntryIdentity {
    EntryIdentity {
        tenant_id: Uuid::parse_str(entry["tenant_id"].as_str().unwrap()).unwrap(),
        usage_record_id: Uuid::parse_str(entry["id"].as_str().unwrap()).unwrap(),
    }
}

/// `entries` as the reader sorts a page of the source's own meters and tenants.
pub fn page_entries(entries: Vec<Value>) -> Vec<PageEntry> {
    entries
        .into_iter()
        .map(|raw| match entry::identify(&raw) {
            Some(identity) => PageEntry::Identified { identity, raw },
            None => PageEntry::Unidentifiable(raw),
        })
        .collect()
}

pub fn id_of(entry: &Value) -> Uuid {
    identity(entry).usage_record_id
}

pub fn hourly(tenant: Uuid, key: &str, quantity: &str) -> Value {
    record_entry(&RecordSpec::hourly(tenant, &meter("hours"), key, HOUR).quantity(&qty(quantity)))
}

pub fn committed(step: Step) -> PageReport {
    match step {
        Step::Committed { report, .. } => report,
        other => panic!("expected a committed page, got {other:?}"),
    }
}

pub async fn step(f: &Fixture, definition: &SourceDefinition) -> Step {
    try_step(f, definition).await.unwrap()
}

/// One step of `definition`'s reader in its ongoing term.
pub async fn try_step(f: &Fixture, definition: &SourceDefinition) -> Result<Step, DomainError> {
    let id = definition.source_id();
    let mut sessions = f.sessions.lock().await;
    let session = sessions.entry(id).or_insert_with(|| f.reader.session(id));
    session.step().await
}

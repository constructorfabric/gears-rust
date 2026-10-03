//! Tests for the write-intent protocol of secret writes (ADR-0006): the
//! intent announced before `plugin.put`, the lease guard, the commit
//! transaction's definite outcomes with the cleanup enqueued in that same
//! transaction (never an inline `destroy`/`delete_key`), the writer's own
//! settlement of a reclaimed intent, and the reclaim (piggyback and startup).
//!
//! The fake repo models "enqueued in the same transaction": a cleanup task
//! appears in `FakeSecretRepo::enqueued_tasks` only in the step that applies
//! the row change or intent deletion that caused it.

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{
    CredentialPatch, CredentialWrite, DestroySelector, Fallback as SdkFallback, PatchField,
    SecretRef, SecretType, SecretValue, SharingMode, StoreKey, ValueVersion,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::clock::MonotonicClock;
use crate::domain::ports::metrics::CleanupOp;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::model::{CleanupTask, PutPrecondition, SecretRow, WritePrecondition};
use crate::domain::secret::service::{ListSettings, Service, WriteSettings};
use crate::domain::secret::test_support::*;

fn key(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid ref")
}

fn write_create(value: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::generic().into()),
        sharing: SharingMode::Tenant,
        fallback: SdkFallback::Inherit,
        expires_at: None,
        secret: Some(SecretValue::from(value)),
    }
}

fn write_replace(value: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: None,
        ..write_create(value)
    }
}

fn patch_secret(secret: PatchField<SecretValue>) -> CredentialPatch {
    CredentialPatch {
        sharing: None,
        fallback: None,
        expires_at: PatchField::Absent,
        secret_type: None,
        secret,
    }
}

fn destroy(k: &StoreKey, selector: DestroySelector) -> CleanupTask {
    CleanupTask::Destroy {
        key: k.clone(),
        selector,
    }
}

fn below(v: &str) -> DestroySelector {
    DestroySelector::Below(ValueVersion::new(v))
}

fn exactly(v: &str) -> DestroySelector {
    DestroySelector::Exactly(ValueVersion::new(v))
}

struct Fixture {
    svc: Service,
    repo: Arc<FakeSecretRepo>,
    plugin: Arc<FakePlugin>,
    metrics: Arc<FakeMetrics>,
    clock: Arc<ManualClock>,
    ctx: toolkit_security::SecurityContext,
}

impl Fixture {
    fn new() -> Self {
        Self::with(FakePlugin::new(), WriteSettings::default())
    }

    fn with(plugin: Arc<FakePlugin>, write: WriteSettings) -> Self {
        let tenant = Uuid::new_v4();
        let repo = Arc::new(FakeSecretRepo::new());
        let metrics = FakeMetrics::new();
        let clock = ManualClock::new();
        let svc = Service::new(
            repo.clone(),
            Arc::new(FakeDir::single(tenant)),
            mock_enforcer(),
            Arc::new(FakePluginSelector::new(plugin.clone())) as Arc<dyn PluginSelector>,
            catalog_type_resolver(),
            metrics.clone(),
            ListSettings {
                max_limit: 200,
                secret_mode_cap: 25,
            },
        )
        .with_write_settings(write)
        .with_clock(clock.clone() as Arc<dyn MonotonicClock>);
        Self {
            svc,
            repo,
            plugin,
            metrics,
            clock,
            ctx: make_ctx(Uuid::new_v4(), tenant),
        }
    }

    /// Creates `k` (value `v1`) and returns its row. A create on a fresh key
    /// enqueues nothing and retires its intent, so what the tests observe
    /// afterwards is what the write under test did.
    async fn create_k(&self) -> SecretRow {
        self.svc
            .put(
                &self.ctx,
                &key("k"),
                write_create("v1"),
                PutPrecondition::CreateOnly,
            )
            .await
            .expect("create");
        self.repo.rows()[0].clone()
    }

    async fn replace_k(
        &self,
        value: &str,
        precondition: PutPrecondition,
    ) -> Result<credstore_sdk::PutOutcome, DomainError> {
        self.svc
            .put(&self.ctx, &key("k"), write_replace(value), precondition)
            .await
    }
}

fn matches(row: &SecretRow) -> PutPrecondition {
    PutPrecondition::Version {
        id: row.id,
        version: row.version,
    }
}

// ── 1. rotation: destroy(below) in the commit tx, nothing inline ────────────

#[tokio::test]
async fn rotation_enqueues_destroy_below_in_the_commit_and_never_calls_the_plugin_inline() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();
    assert!(
        f.repo.enqueued_tasks().is_empty(),
        "a create on a fresh key has nothing to clean up"
    );

    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert_eq!(
        f.repo.enqueued_tasks(),
        [destroy(&store_key, below("2"))],
        "destroy(below the committed version) is enqueued by the commit"
    );
    assert!(f.repo.intents().is_empty(), "the intent was retired");
    assert!(f.plugin.destroy_calls().is_empty(), "no inline destroy");
    assert_eq!(f.plugin.versions(&store_key), vec!["1", "2"]);

    // A PATCH carrying a secret is the same write.
    f.svc
        .patch(
            &f.ctx,
            &key("k"),
            patch_secret(PatchField::Set(SecretValue::from("v3"))),
            WritePrecondition::Exists,
        )
        .await
        .expect("patch");
    assert_eq!(
        f.repo.enqueued_tasks(),
        [
            destroy(&store_key, below("2")),
            destroy(&store_key, below("3"))
        ]
    );
    assert!(f.plugin.destroy_calls().is_empty());

    run_cleanup(&f.repo, &f.plugin).await;
    assert_eq!(f.plugin.versions(&store_key), vec!["3"]);
    assert_eq!(
        f.metrics.store_cleanup_enqueued(),
        [CleanupOp::Destroy, CleanupOp::Destroy]
    );
}

#[tokio::test]
async fn rotation_on_a_plugin_without_destroy_enqueues_nothing() {
    let f = Fixture::with(FakePlugin::without_destroy(), WriteSettings::default());
    let row = f.create_k().await;

    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert!(f.repo.enqueued_tasks().is_empty());
    assert!(f.metrics.store_cleanup_enqueued().is_empty());
    assert_eq!(
        f.plugin.versions(&row.store_key()),
        vec!["1", "2"],
        "rotated versions stay until the record is deleted"
    );
}

// ── 2. removing the secret: both destroys with the CAS ──────────────────────

#[tokio::test]
async fn removing_the_secret_enqueues_both_destroys_with_the_cas() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    f.svc
        .patch(
            &f.ctx,
            &key("k"),
            patch_secret(PatchField::Null),
            WritePrecondition::Exists,
        )
        .await
        .expect("remove");
    assert_eq!(
        f.repo.enqueued_tasks(),
        [
            destroy(&store_key, below("1")),
            destroy(&store_key, exactly("1"))
        ]
    );
    assert!(f.repo.intents().is_empty(), "removal announces no intent");
    assert!(f.repo.begun_attempts().len() == 1, "only the create did");
    assert!(f.plugin.destroy_calls().is_empty());
    assert_eq!(
        f.metrics.store_cleanup_enqueued(),
        [CleanupOp::Destroy, CleanupOp::Destroy]
    );
}

#[tokio::test]
async fn removing_the_secret_on_a_plugin_without_destroy_enqueues_nothing() {
    let f = Fixture::with(FakePlugin::without_destroy(), WriteSettings::default());
    f.create_k().await;

    f.svc
        .patch(
            &f.ctx,
            &key("k"),
            patch_secret(PatchField::Null),
            WritePrecondition::Exists,
        )
        .await
        .expect("remove");
    assert!(f.repo.enqueued_tasks().is_empty());
}

// ── 3-4. ambiguous tx1: the intent remains, reclaim settles it ──────────────

#[tokio::test]
async fn replace_with_an_ambiguous_commit_is_503_and_reclaim_enqueues_nothing_for_a_live_row() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_switch_value(1);
    let err = f
        .replace_k("torn", matches(&row))
        .await
        .expect_err("ambiguous commit");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(f.repo.intents().len(), 1, "the intent remains");
    assert!(f.repo.enqueued_tasks().is_empty());

    // Not expired yet: nothing to reclaim.
    assert_eq!(f.svc.reclaim_expired().await.expect("reclaim"), 0);
    assert_eq!(f.repo.intents().len(), 1);

    // Lease over: the intent is deleted; the record's row exists, so the
    // orphan above its pointer is the documented residual and nothing is
    // enqueued.
    f.repo.expire_intents();
    assert_eq!(f.svc.reclaim_expired().await.expect("reclaim"), 1);
    assert!(f.repo.intents().is_empty());
    assert!(f.repo.enqueued_tasks().is_empty());
    assert_eq!(f.metrics.write_intents_reclaimed_total(), 1);
    // The orphan is removed by the record's next write (destroy below).
    f.replace_k("next", matches(&row))
        .await
        .expect("next write");
    run_cleanup(&f.repo, &f.plugin).await;
    assert_eq!(f.plugin.versions(&row.store_key()), vec!["3"]);
}

#[tokio::test]
async fn create_with_an_ambiguous_commit_leaves_the_intent_and_reclaim_purges_the_key() {
    let f = Fixture::new();

    f.repo.fail_next_insert_active(1);
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v1"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("ambiguous commit");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    let intents = f.repo.intents();
    assert_eq!(intents.len(), 1);
    let store_key = intents[0].1.clone();
    assert!(f.repo.rows().is_empty());
    assert!(f.plugin.holds_key(&store_key), "the put landed");

    f.repo.expire_intents();
    assert_eq!(f.svc.reclaim_expired().await.expect("reclaim"), 1);
    assert!(f.repo.intents().is_empty());
    assert_eq!(
        f.repo.enqueued_tasks(),
        [CleanupTask::Purge(store_key.clone())],
        "no row exists for the record: the whole key is dead"
    );
    assert_eq!(f.metrics.store_cleanup_enqueued(), [CleanupOp::Purge]);
    run_cleanup(&f.repo, &f.plugin).await;
    assert!(!f.plugin.holds_key(&store_key));
}

// ── 5. late writer ──────────────────────────────────────────────────────────

#[tokio::test]
async fn late_writer_whose_row_was_deleted_purges_the_key_in_the_commit() {
    let f = Fixture::new();
    let row = f.create_k().await;

    // The row is deleted between this writer's read and its commit.
    f.repo.delete_row_before_next_commit(row.id);
    let err = f
        .replace_k("late", matches(&row))
        .await
        .expect_err("the CAS matches nothing");
    assert!(matches!(err, DomainError::VersionConflict), "{err:?}");

    assert_eq!(
        f.repo.enqueued_tasks(),
        [CleanupTask::Purge(row.store_key())],
        "no row exists any more: purge, enqueued with the intent deletion"
    );
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.metrics.write_intent_lost_total(), 0, "not a lost intent");
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing inline");
}

#[tokio::test]
async fn late_patch_whose_row_was_deleted_purges_the_key_in_the_commit() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.delete_row_before_next_commit(row.id);
    let err = f
        .svc
        .patch(
            &f.ctx,
            &key("k"),
            patch_secret(PatchField::Set(SecretValue::from("late"))),
            WritePrecondition::Version {
                id: row.id,
                version: row.version,
            },
        )
        .await
        .expect_err("the CAS matches nothing");
    assert!(matches!(err, DomainError::VersionConflict), "{err:?}");
    assert_eq!(
        f.repo.enqueued_tasks(),
        [CleanupTask::Purge(row.store_key())]
    );
}

// ── 6. reclaim races a live writer ──────────────────────────────────────────

#[tokio::test]
async fn reclaim_before_the_replace_commit_leaves_the_row_and_the_writer_destroys_its_version() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    f.repo.reclaim_intents_before_next_commits(1);
    let err = f
        .replace_k("slow", matches(&row))
        .await
        .expect_err("the intent was reclaimed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    // tx1 aborted: the row is unchanged.
    let after = f.repo.rows()[0].clone();
    assert_eq!(after.version, row.version);
    assert_eq!(after.value_version, row.value_version);
    // The writer knows its version and the row exists: destroy exactly it.
    assert_eq!(
        f.repo.enqueued_tasks(),
        [destroy(&store_key, exactly("2"))],
        "the reclaim of a live record's intent enqueued nothing"
    );
    assert_eq!(f.metrics.write_intent_lost_total(), 1);
    assert_eq!(f.metrics.write_intents_reclaimed_total(), 0, "not by us");
    run_cleanup(&f.repo, &f.plugin).await;
    assert_eq!(f.plugin.versions(&store_key), vec!["1"]);
}

#[tokio::test]
async fn reclaim_before_the_create_commit_purges_the_key_and_inserts_no_row() {
    let f = Fixture::new();

    f.repo.reclaim_intents_before_next_commits(1);
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("slow"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("the intent was reclaimed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert!(f.repo.rows().is_empty(), "no row was inserted");
    // The reclaimer (no row for the record) and the writer (settling its own
    // version) both purge the one fresh key: idempotent.
    let purged = f.repo.purged_keys();
    assert_eq!(purged.len(), 2);
    assert_eq!(purged[0], purged[1]);
    assert_eq!(f.metrics.write_intent_lost_total(), 1);
    run_cleanup(&f.repo, &f.plugin).await;
    assert!(!f.plugin.holds_key(&purged[0]));
}

#[tokio::test]
async fn reclaimed_intent_on_a_plugin_without_destroy_enqueues_nothing_for_a_live_row() {
    let f = Fixture::with(FakePlugin::without_destroy(), WriteSettings::default());
    let row = f.create_k().await;

    f.repo.reclaim_intents_before_next_commits(1);
    f.replace_k("slow", matches(&row))
        .await
        .expect_err("the intent was reclaimed");
    assert!(f.repo.enqueued_tasks().is_empty());
    assert_eq!(f.metrics.write_intent_lost_total(), 1);
}

#[tokio::test]
async fn failed_settlement_of_a_reclaimed_intent_is_counted_and_still_503() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.reclaim_intents_before_next_commits(1);
    f.repo.fail_next_settle(1);
    let err = f
        .replace_k("slow", matches(&row))
        .await
        .expect_err("the intent was reclaimed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert!(f.repo.enqueued_tasks().is_empty(), "the settlement failed");
    assert_eq!(f.metrics.write_intent_lost_total(), 1);
    assert_eq!(f.metrics.write_intent_reclaim_failed_total(), 1);
}

// ── 7. lease guard ──────────────────────────────────────────────────────────

fn short_lease() -> WriteSettings {
    WriteSettings {
        intent_lease: Duration::from_secs(100),
        reclaim_batch: 16,
    }
}

#[tokio::test]
async fn lease_guard_abandons_a_write_that_started_too_late() {
    let f = Fixture::with(FakePlugin::new(), short_lease());
    let row = f.create_k().await;
    let store_key = row.store_key();

    // More than half the lease passes between announcing and the put.
    f.repo
        .advance_clock_on_begin_write_intent(&f.clock, Duration::from_secs(51));
    let err = f
        .replace_k("late", matches(&row))
        .await
        .expect_err("the guard must refuse");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(
        f.plugin.versions(&store_key),
        vec!["1"],
        "no put was issued"
    );
    assert!(f.repo.intents().is_empty(), "its own intent was deleted");
    assert!(f.repo.enqueued_tasks().is_empty());
    assert_eq!(f.repo.rows()[0].version, row.version);

    // A create is guarded the same way.
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("other"),
            write_create("late"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("the guard must refuse");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert!(f.repo.intents().is_empty());
}

#[tokio::test]
async fn lease_guard_allows_a_write_that_used_at_most_half_the_lease() {
    let f = Fixture::with(FakePlugin::new(), short_lease());
    let row = f.create_k().await;

    f.repo
        .advance_clock_on_begin_write_intent(&f.clock, Duration::from_secs(50));
    f.replace_k("ok", matches(&row))
        .await
        .expect("exactly half the lease is still within it");
    assert_eq!(f.repo.rows()[0].value_version, Some(ValueVersion::new("2")));
}

// ── 8. create unique violation ──────────────────────────────────────────────

#[tokio::test]
async fn create_unique_violation_purges_the_key_with_the_intent_deletion_and_is_409() {
    let f = Fixture::new();

    f.repo.conflict_next_insert_active(1);
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("loser"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("the reference was taken concurrently");
    assert!(matches!(err, DomainError::Conflict), "{err:?}");

    let tasks = f.repo.enqueued_tasks();
    assert_eq!(tasks.len(), 1);
    let CleanupTask::Purge(purged) = &tasks[0] else {
        panic!("a lost create purges its fresh key: {tasks:?}");
    };
    assert!(f.repo.intents().is_empty(), "the intent deletion committed");
    assert!(f.repo.rows().is_empty());
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing inline");
    assert!(f.plugin.destroy_calls().is_empty());
    assert!(f.plugin.holds_key(purged));
}

// ── 9. Exists retry ─────────────────────────────────────────────────────────

#[tokio::test]
async fn exists_retry_is_a_new_attempt_with_a_new_intent() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.force_next_switch_value_none(1);
    f.replace_k("again", PutPrecondition::Exists)
        .await
        .expect("the retry commits");

    let attempts = f.repo.begun_attempts();
    assert_eq!(attempts.len(), 3, "the create, the lost attempt, the retry");
    assert_ne!(
        attempts[1], attempts[2],
        "a retry never reuses an attempt id"
    );
    assert!(f.repo.intents().is_empty());
    // The lost attempt cleaned up its own version, the retry destroyed below.
    assert_eq!(
        f.repo.enqueued_tasks(),
        [
            destroy(&row.store_key(), exactly("2")),
            destroy(&row.store_key(), below("3")),
        ]
    );
    run_cleanup(&f.repo, &f.plugin).await;
    assert_eq!(f.plugin.versions(&row.store_key()), vec!["3"]);
}

// ── 10. piggyback reclaim ───────────────────────────────────────────────────

#[tokio::test]
async fn piggyback_reclaim_runs_after_a_write() {
    let f = Fixture::new();

    // A crashed writer's intent, lease over.
    f.repo.fail_next_insert_active(1);
    f.svc
        .put(
            &f.ctx,
            &key("crashed"),
            write_create("x"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("ambiguous");
    f.repo.expire_intents();
    assert_eq!(f.repo.intents().len(), 1);

    // An unrelated successful write reclaims it on the way out.
    f.svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v1"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
    assert!(f.repo.intents().is_empty(), "reclaimed after the write");
    assert_eq!(f.metrics.write_intents_reclaimed_total(), 1);
    assert_eq!(f.repo.purged_keys().len(), 1, "no row: purge the dead key");
}

#[tokio::test]
async fn piggyback_reclaim_failure_does_not_change_the_reply() {
    let f = Fixture::new();
    f.repo.fail_next_reclaim(1);

    let outcome = f
        .svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v1"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("the write's reply is unaffected");
    assert!(outcome.created);
    assert_eq!(f.metrics.write_intent_reclaim_failed_total(), 1);
    assert_eq!(f.repo.rows().len(), 1);

    // A failing write keeps its own error, too.
    let row = f.repo.rows()[0].clone();
    f.repo.fail_next_reclaim(1);
    let err = f
        .replace_k(
            "x",
            PutPrecondition::Version {
                id: row.id,
                version: row.version + 5,
            },
        )
        .await
        .expect_err("stale validator");
    assert!(matches!(err, DomainError::VersionConflict), "{err:?}");
}

#[tokio::test]
async fn piggyback_reclaim_runs_after_a_lost_commit_too() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.reclaim_intents_before_next_commits(1);
    f.repo.fail_next_reclaim(1);
    f.replace_k("slow", matches(&row))
        .await
        .expect_err("the intent was reclaimed");
    assert_eq!(
        f.metrics.write_intent_reclaim_failed_total(),
        1,
        "the piggyback pass ran after the settlement and failed"
    );
}

// ── tx0 failure, put failure ────────────────────────────────────────────────

#[tokio::test]
async fn a_failed_intent_insert_is_503_and_nothing_is_written_to_the_store() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_begin_write_intent(1);
    let err = f
        .replace_k("v2", matches(&row))
        .await
        .expect_err("tx0 failed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(f.plugin.versions(&row.store_key()), vec!["1"], "no put");
    assert!(f.repo.intents().is_empty());

    f.repo.fail_next_begin_write_intent(1);
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("other"),
            write_create("v"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("tx0 failed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(f.repo.rows().len(), 1);
}

#[tokio::test]
async fn a_failed_put_keeps_the_intent_for_the_reclaim() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.plugin.fail_next_puts(1);
    f.replace_k("v2", matches(&row))
        .await
        .expect_err("the store failed");
    assert_eq!(f.repo.intents().len(), 1, "the intent stays");

    f.repo.expire_intents();
    assert_eq!(f.svc.reclaim_expired().await.expect("reclaim"), 1);
    assert!(f.repo.enqueued_tasks().is_empty(), "the row exists");
}

// ── startup reclaim ─────────────────────────────────────────────────────────

#[tokio::test]
async fn startup_reclaim_repeats_until_a_pass_returns_fewer_than_the_batch() {
    let f = Fixture::with(
        FakePlugin::new(),
        WriteSettings {
            intent_lease: Duration::from_mins(5),
            reclaim_batch: 2,
        },
    );
    for name in ["a", "b", "c"] {
        f.repo.fail_next_insert_active(1);
        f.repo.fail_next_reclaim(0);
        f.svc
            .put(
                &f.ctx,
                &key(name),
                write_create("x"),
                PutPrecondition::CreateOnly,
            )
            .await
            .expect_err("ambiguous");
    }
    assert_eq!(f.repo.intents().len(), 3);
    f.repo.expire_intents();

    // Two passes: 2 (a full batch), then 1.
    assert_eq!(f.svc.reclaim_at_startup().await.expect("startup"), 3);
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.repo.purged_keys().len(), 3);
    assert_eq!(f.metrics.write_intents_reclaimed_total(), 3);
}

#[tokio::test]
async fn startup_reclaim_reports_an_error_and_counts_it() {
    let f = Fixture::new();
    f.repo.fail_next_reclaim(1);
    f.svc
        .reclaim_at_startup()
        .await
        .expect_err("the pass failed");
    assert_eq!(f.metrics.write_intent_reclaim_failed_total(), 1);
    assert_eq!(f.svc.reclaim_at_startup().await.expect("startup"), 0);
}

// ── delete ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_counts_the_purge_it_enqueued() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect("delete");
    assert_eq!(
        f.repo.enqueued_tasks(),
        [CleanupTask::Purge(row.store_key())]
    );
    assert_eq!(f.metrics.store_cleanup_enqueued(), [CleanupOp::Purge]);
}

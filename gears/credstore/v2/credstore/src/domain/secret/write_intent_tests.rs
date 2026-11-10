// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Tests for the write-intent protocol of secret writes (ADR-0006): the
//! intent announced before `plugin.put`, the commit
//! transaction's definite outcomes with the cleanup debt recorded in that same
//! transaction and executed by the same request after the confirmed commit,
//! the 503 of a writer whose intent was healed, and heal on access.
//!
//! The fake repo models "recorded in the same transaction": a debt appears in
//! `FakeSecretRepo::recorded_tasks` only in the step that applies the row
//! change or intent deletion that caused it, and leaves
//! `FakeSecretRepo::pending_tasks` only when the service deletes it after a
//! successful execution.

use std::sync::Arc;

use credstore_sdk::{
    CredentialPatch, CredentialWrite, DestroySelector, Fallback as SdkFallback, PatchField,
    SecretRef, SecretType, SecretValue, SecretWrite, SharingMode, StoreKey, ValueVersion,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CleanupOp, VerifyOp, VerifyOutcome};
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
        secret: SecretWrite::Set(SecretValue::from(value)),
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
        let svc = Service::new(
            repo.clone(),
            Arc::new(FakeDir::single(tenant)),
            mock_enforcer(),
            Arc::new(FakePluginSelector::new(plugin.clone())) as Arc<dyn PluginSelector>,
            catalog_type_resolver(),
            metrics.clone(),
            ListSettings { max_limit: 200 },
        )
        .with_write_settings(write);
        Self {
            svc,
            repo,
            plugin,
            metrics,
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

// ── 1. rotation: destroy(below) recorded in the commit, executed after it ──

#[tokio::test]
async fn rotation_records_destroy_below_in_the_commit_and_executes_it_after_the_commit() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();
    assert!(
        f.repo.recorded_tasks().is_empty(),
        "a create on a fresh key has nothing to clean up"
    );

    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert_eq!(
        f.repo.recorded_tasks(),
        [destroy(&store_key, below("2"))],
        "destroy(below the committed version) is recorded by the commit"
    );
    assert!(f.repo.intents().is_empty(), "the intent was retired");
    assert_eq!(
        f.plugin.destroy_calls(),
        [(store_key.clone(), below("2"))],
        "executed by the same request"
    );
    assert!(
        f.repo.pending_tasks().is_empty(),
        "the executed debt is gone"
    );
    assert_eq!(f.plugin.versions(&store_key), vec!["2"]);

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
        f.repo.recorded_tasks(),
        [
            destroy(&store_key, below("2")),
            destroy(&store_key, below("3"))
        ]
    );
    assert!(f.repo.pending_tasks().is_empty());
    assert_eq!(f.plugin.versions(&store_key), vec!["3"]);
    assert_eq!(
        f.metrics.store_cleanup_recorded(),
        [CleanupOp::Destroy, CleanupOp::Destroy]
    );
    assert!(f.metrics.store_cleanup_failed().is_empty());
}

#[tokio::test]
async fn rotation_on_a_plugin_without_destroy_records_nothing() {
    let f = Fixture::with(FakePlugin::without_destroy(), WriteSettings::default());
    let row = f.create_k().await;

    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert!(f.repo.recorded_tasks().is_empty());
    assert!(f.metrics.store_cleanup_recorded().is_empty());
    assert_eq!(
        f.plugin.versions(&row.store_key()),
        vec!["1", "2"],
        "rotated versions stay until the record is deleted"
    );
}

#[tokio::test]
async fn a_failing_destroy_leaves_the_debt_and_does_not_change_the_reply() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    f.plugin.fail_next_destroys(1);
    let outcome = f
        .replace_k("v2", matches(&row))
        .await
        .expect("the reply does not depend on the cleanup");
    assert!(!outcome.created);
    assert_eq!(f.repo.rows()[0].value_version, Some(ValueVersion::new("2")));
    assert_eq!(
        f.repo.pending_tasks(),
        [destroy(&store_key, below("2"))],
        "the debt row stays"
    );
    assert_eq!(f.metrics.store_cleanup_failed(), [CleanupOp::Destroy]);
    assert_eq!(f.plugin.versions(&store_key), vec!["1", "2"]);
}

// ── 2. removing the secret: both destroys with the CAS ──────────────────────

#[tokio::test]
async fn removing_the_secret_records_both_destroys_with_the_cas_and_executes_them() {
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
        f.repo.recorded_tasks(),
        [
            destroy(&store_key, below("1")),
            destroy(&store_key, exactly("1"))
        ]
    );
    assert!(f.repo.intents().is_empty(), "removal announces no intent");
    assert!(f.repo.begun_attempts().len() == 1, "only the create did");
    assert_eq!(f.plugin.destroy_calls().len(), 2);
    assert!(f.plugin.delete_key_calls().is_empty(), "never delete_key");
    assert!(f.repo.pending_tasks().is_empty());
    assert!(f.plugin.versions(&store_key).is_empty());
    assert_eq!(
        f.metrics.store_cleanup_recorded(),
        [CleanupOp::Destroy, CleanupOp::Destroy]
    );
}

#[tokio::test]
async fn removing_the_secret_on_a_plugin_without_destroy_records_nothing() {
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
    assert!(f.repo.recorded_tasks().is_empty());
}

// ── 3-4. ambiguous tx1: nothing executed, a later request heals ─────────────

#[tokio::test]
async fn ambiguous_replace_commit_with_a_failed_verification_is_503_executes_nothing_and_the_next_write_heals_the_intent()
 {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_switch_value(1);
    f.repo.fail_next_verifications(1);
    let err = f
        .replace_k("torn", matches(&row))
        .await
        .expect_err("ambiguous commit");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(f.repo.intents().len(), 1, "the intent remains");
    assert!(f.repo.recorded_tasks().is_empty());
    assert!(f.plugin.destroy_calls().is_empty(), "nothing executed");
    assert!(f.plugin.delete_key_calls().is_empty());

    // Not expired yet: a write leaves the intent alone.
    f.replace_k("n1", matches(&row)).await.expect("write");
    assert_eq!(f.repo.intents().len(), 1);
    assert_eq!(f.metrics.write_intents_healed_total(), 0);

    // Lease over: the next successful write's commit deletes it, and its
    // destroy(below) also removes the orphan above the old pointer.
    f.repo.expire_intents();
    let row = f.repo.rows()[0].clone();
    f.replace_k("next", matches(&row))
        .await
        .expect("next write");
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.metrics.write_intents_healed_total(), 1);
    assert_eq!(f.plugin.versions(&row.store_key()), vec!["4"]);
}

#[tokio::test]
async fn ambiguous_create_commit_with_a_failed_verification_leaves_the_intent_and_a_later_create_of_the_reference_purges_the_key()
 {
    let f = Fixture::new();

    f.repo.fail_next_insert_active(1);
    f.repo.fail_next_verifications(1);
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
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing executed");

    // Not expired: left alone.
    f.svc.get_record(&f.ctx, &key("k")).await.expect("read");
    assert_eq!(f.repo.intents().len(), 1);

    f.repo.expire_intents();
    f.svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v2"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("the create heals the failed one first");
    assert_eq!(
        f.repo.recorded_tasks(),
        [CleanupTask::Purge(store_key.clone())],
        "no row exists for the record: the whole key is dead"
    );
    assert_eq!(f.metrics.store_cleanup_recorded(), [CleanupOp::Purge]);
    assert_eq!(f.metrics.write_intents_healed_total(), 1);
    assert!(f.repo.intents().is_empty());
    assert!(f.repo.pending_tasks().is_empty());
    assert!(!f.plugin.holds_key(&store_key));
}

// ── 3a. verification after an ambiguous tx1 ─────────────────────────────────

#[tokio::test]
async fn ambiguous_replace_that_did_commit_is_verified_and_its_debts_are_executed() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    // tx1 commits, but the reply is lost.
    f.repo.fail_next_switch_value_after_commit(1);
    f.replace_k("v2", matches(&row))
        .await
        .expect("the verification finds the pointer at the attempt's version");

    assert_eq!(f.repo.verification_count(), 1);
    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::Committed)]
    );
    assert_eq!(f.repo.rows()[0].value_version, Some(ValueVersion::new("2")));
    assert!(f.repo.intents().is_empty());
    assert_eq!(
        f.plugin.destroy_calls(),
        [(store_key.clone(), below("2"))],
        "the debts tx1 recorded are executed"
    );
    assert!(f.repo.pending_tasks().is_empty());
    assert_eq!(f.plugin.versions(&store_key), vec!["2"]);
}

#[tokio::test]
async fn ambiguous_create_that_did_commit_is_verified() {
    let f = Fixture::new();

    f.repo.fail_next_insert_active_after_commit(1);
    f.svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v1"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("the verification finds the row");

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::Committed)]
    );
    assert_eq!(f.repo.rows().len(), 1);
    assert!(f.repo.intents().is_empty());
    assert!(f.repo.recorded_tasks().is_empty());
}

#[tokio::test]
async fn ambiguous_replace_that_did_not_commit_runs_tx1_again_in_the_verification() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    // tx1 did not commit: the own intent is still there.
    f.repo.fail_next_switch_value(1);
    f.replace_k("v2", matches(&row))
        .await
        .expect("the verification runs tx1 again");

    assert_eq!(f.repo.verification_count(), 1);
    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::NotCommitted)]
    );
    assert_eq!(f.repo.rows()[0].value_version, Some(ValueVersion::new("2")));
    assert_eq!(f.repo.rows()[0].version, row.version + 1);
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.plugin.destroy_calls(), [(store_key, below("2"))]);
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn ambiguous_create_that_did_not_commit_runs_tx1_again_in_the_verification() {
    let f = Fixture::new();

    f.repo.fail_next_insert_active(1);
    f.svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("v1"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("the verification runs tx1 again");

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::NotCommitted)]
    );
    assert_eq!(f.repo.rows().len(), 1);
    assert!(f.repo.intents().is_empty());
}

#[tokio::test]
async fn ambiguous_replace_whose_intent_was_healed_records_destroy_exact_and_is_503() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    // A heal took the intent, and tx1 failed ambiguously: the attempt did not
    // take effect and the pointer is not at its version.
    f.repo.heal_intents_before_next_commits(1);
    f.repo.fail_next_switch_value(1);
    let err = f
        .replace_k("v2", matches(&row))
        .await
        .expect_err("not applied");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::NotApplied)]
    );
    assert_eq!(f.repo.recorded_tasks(), [destroy(&store_key, exactly("2"))]);
    assert_eq!(
        f.plugin.destroy_calls(),
        [(store_key.clone(), exactly("2"))]
    );
    assert!(f.repo.pending_tasks().is_empty());
    assert_eq!(f.plugin.versions(&store_key), vec!["1"]);
    assert_eq!(f.repo.rows()[0].value_version, Some(ValueVersion::new("1")));
}

#[tokio::test]
async fn ambiguous_create_whose_intent_was_healed_records_a_purge_and_is_503() {
    let f = Fixture::new();

    f.repo.heal_intents_before_next_commits(1);
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
        .expect_err("not applied");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::NotApplied)]
    );
    assert!(f.repo.rows().is_empty());
    let purged = f.repo.purged_keys();
    assert_eq!(purged.len(), 1, "no row has the record id: the key is dead");
    assert_eq!(f.plugin.delete_key_calls(), purged);
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn a_failing_verification_is_503_and_executes_nothing() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_switch_value_after_commit(1);
    f.repo.fail_next_verifications(1);
    let err = f
        .replace_k("v2", matches(&row))
        .await
        .expect_err("the outcome stays unknown");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    assert_eq!(
        f.repo.verification_count(),
        1,
        "one verification per request"
    );
    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Write, VerifyOutcome::Failed)]
    );
    assert!(f.plugin.destroy_calls().is_empty(), "nothing executed");
    assert!(f.plugin.delete_key_calls().is_empty());
    // The commit did happen: its debt is durable and a later request runs it.
    assert_eq!(
        f.repo.pending_tasks(),
        [destroy(&row.store_key(), below("2"))]
    );
}

// ── 5. late writer ──────────────────────────────────────────────────────────

#[tokio::test]
async fn late_writer_whose_row_was_deleted_records_and_executes_a_purge() {
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
        f.repo.recorded_tasks(),
        [CleanupTask::Purge(row.store_key())],
        "no row exists any more: purge, recorded with the intent deletion"
    );
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.plugin.delete_key_calls(), [row.store_key()]);
    assert!(!f.plugin.holds_key(&row.store_key()));
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn late_patch_whose_row_was_deleted_records_a_purge() {
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
        f.repo.recorded_tasks(),
        [CleanupTask::Purge(row.store_key())]
    );
}

// ── 6. intent gone at tx1: a heal raced a live writer ───────────────────────

#[tokio::test]
async fn heal_before_the_replace_commit_answers_503_and_does_nothing_else() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();

    f.repo.heal_intents_before_next_commits(1);
    let err = f
        .replace_k("slow", matches(&row))
        .await
        .expect_err("the intent was healed");
    let DomainError::ServiceUnavailable { detail, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(detail, "write intent expired; retry");

    // tx1 aborted: the row is unchanged.
    let after = f.repo.rows()[0].clone();
    assert_eq!(after.version, row.version);
    assert_eq!(after.value_version, row.value_version);
    // Nothing else: no debt, no destroy, no verification. Whoever healed the
    // intent owns the cleanup of the writer's version.
    assert!(f.repo.recorded_tasks().is_empty());
    assert!(f.repo.pending_tasks().is_empty());
    assert!(f.plugin.destroy_calls().is_empty());
    assert!(f.plugin.delete_key_calls().is_empty());
    assert!(f.metrics.commit_verifications().is_empty());
    assert_eq!(f.metrics.write_intents_healed_total(), 0, "not by us");
    assert_eq!(f.plugin.versions(&store_key), vec!["1", "2"]);
}

#[tokio::test]
async fn heal_before_the_create_commit_answers_503_and_does_nothing_else() {
    let f = Fixture::new();

    f.repo.heal_intents_before_next_commits(1);
    let err = f
        .svc
        .put(
            &f.ctx,
            &key("k"),
            write_create("slow"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect_err("the intent was healed");
    let DomainError::ServiceUnavailable { detail, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(detail, "write intent expired; retry");
    assert!(f.repo.rows().is_empty(), "no row was inserted");
    assert!(f.repo.recorded_tasks().is_empty());
    assert!(f.repo.pending_tasks().is_empty());
    assert!(f.plugin.destroy_calls().is_empty());
    assert!(f.plugin.delete_key_calls().is_empty());
    assert!(f.metrics.commit_verifications().is_empty());
}

// ── 8. create unique violation ──────────────────────────────────────────────

#[tokio::test]
async fn create_unique_violation_records_a_purge_with_the_intent_deletion_and_executes_it() {
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

    let tasks = f.repo.recorded_tasks();
    assert_eq!(tasks.len(), 1);
    let CleanupTask::Purge(purged) = &tasks[0] else {
        panic!("a lost create purges its fresh key: {tasks:?}");
    };
    assert!(f.repo.intents().is_empty(), "the intent deletion committed");
    assert!(f.repo.rows().is_empty());
    assert_eq!(f.plugin.delete_key_calls(), std::slice::from_ref(purged));
    assert!(f.plugin.destroy_calls().is_empty());
    assert!(!f.plugin.holds_key(purged));
    assert!(f.repo.pending_tasks().is_empty());
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
        f.repo.recorded_tasks(),
        [
            destroy(&row.store_key(), exactly("2")),
            destroy(&row.store_key(), below("3")),
        ]
    );
    assert_eq!(f.plugin.versions(&row.store_key()), vec!["3"]);
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
async fn a_failed_put_keeps_the_intent_for_a_later_heal() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.plugin.fail_next_puts(1);
    f.replace_k("v2", matches(&row))
        .await
        .expect_err("the store failed");
    assert_eq!(f.repo.intents().len(), 1, "the intent stays");
    assert!(f.repo.recorded_tasks().is_empty());
}

// ── heal on access ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_pending_debt_is_executed_by_the_next_read_of_the_record() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();
    f.plugin.fail_next_destroys(1);
    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert_eq!(f.repo.pending_tasks().len(), 1);
    assert_eq!(f.plugin.versions(&store_key), vec!["1", "2"]);

    let secret = f
        .svc
        .get_secret(&f.ctx, &key("k"))
        .await
        .expect("read")
        .expect("value");
    assert_eq!(secret.secret.as_bytes(), b"v2", "the answer is unchanged");
    assert!(f.repo.pending_tasks().is_empty(), "the read executed it");
    assert_eq!(f.plugin.versions(&store_key), vec!["2"]);
}

#[tokio::test]
async fn a_pending_debt_is_executed_by_the_next_write_of_the_record() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();
    f.plugin.fail_next_destroys(1);
    f.replace_k("v2", matches(&row)).await.expect("replace");
    let row = f.repo.rows()[0].clone();

    f.replace_k("v3", matches(&row)).await.expect("replace");
    assert!(f.repo.pending_tasks().is_empty());
    assert_eq!(f.plugin.versions(&store_key), vec!["3"]);
    assert_eq!(
        f.plugin.destroy_calls()[1],
        (store_key, below("2")),
        "the old debt ran first, in the heal"
    );
}

#[tokio::test]
async fn a_failing_heal_leaves_the_debt_and_does_not_change_the_read() {
    let f = Fixture::new();
    let row = f.create_k().await;
    f.plugin.fail_next_destroys(2);
    f.replace_k("v2", matches(&row)).await.expect("replace");

    let secret = f
        .svc
        .get_secret(&f.ctx, &key("k"))
        .await
        .expect("read")
        .expect("value");
    assert_eq!(secret.secret.as_bytes(), b"v2");
    assert_eq!(f.repo.pending_tasks().len(), 1, "still owed");
    assert_eq!(
        f.metrics.store_cleanup_failed(),
        [CleanupOp::Destroy, CleanupOp::Destroy]
    );
}

#[tokio::test]
async fn the_next_secret_write_deletes_expired_intents_of_the_record_and_not_live_ones() {
    let f = Fixture::new();
    let row = f.create_k().await;
    let store_key = row.store_key();
    f.repo.seed_intent(&store_key, "k", true);
    let live = f.repo.seed_intent(&store_key, "k", false);

    f.replace_k("v2", matches(&row)).await.expect("replace");
    assert_eq!(f.metrics.write_intents_healed_total(), 1);
    let left: Vec<Uuid> = f.repo.intents().into_iter().map(|i| i.0).collect();
    assert_eq!(left, [live], "only the unexpired intent is left");
}

#[tokio::test]
async fn a_read_never_touches_expired_intents_of_a_live_record() {
    let f = Fixture::new();
    let row = f.create_k().await;
    f.repo.seed_intent(&row.store_key(), "k", true);

    f.svc
        .get_secret(&f.ctx, &key("k"))
        .await
        .expect("read")
        .expect("value");
    assert_eq!(f.repo.intents().len(), 1);
    assert_eq!(f.metrics.write_intents_healed_total(), 0);
    assert!(f.repo.recorded_tasks().is_empty());
}

#[tokio::test]
async fn a_point_read_heals_the_failed_create_of_its_reference() {
    let f = Fixture::new();
    // A create that failed after announcing: an expired intent, no row.
    let dead = StoreKey::new(
        credstore_sdk::TenantId(f.ctx.subject_tenant_id()),
        Uuid::new_v4(),
    );
    f.plugin.seed(&dead, b"orphan");
    f.repo.seed_intent(&dead, "k", true);

    let got = f.svc.get_record(&f.ctx, &key("k")).await.expect("read");
    assert!(got.is_none());
    assert!(f.repo.intents().is_empty());
    assert_eq!(f.repo.recorded_tasks(), [CleanupTask::Purge(dead.clone())]);
    assert!(!f.plugin.holds_key(&dead), "the purge was executed");
    assert!(f.repo.pending_tasks().is_empty());
    assert_eq!(f.metrics.write_intents_healed_total(), 1);
}

#[tokio::test]
async fn failed_create_heal_leaves_an_intent_whose_record_has_a_row_and_unexpired_ones() {
    let f = Fixture::new();
    let row = f.create_k().await;
    f.repo.seed_intent(&row.store_key(), "k", true);
    let dead = StoreKey::new(
        credstore_sdk::TenantId(f.ctx.subject_tenant_id()),
        Uuid::new_v4(),
    );
    f.repo.seed_intent(&dead, "k", false);

    f.svc.get_record(&f.ctx, &key("k")).await.expect("read");
    assert_eq!(f.repo.intents().len(), 2, "neither is a failed create");
    assert!(f.repo.recorded_tasks().is_empty());
}

#[tokio::test]
async fn a_failing_failed_create_heal_does_not_change_the_reply() {
    let f = Fixture::new();
    f.create_k().await;
    f.repo.fail_next_heal_failed_creates(2);

    assert!(
        f.svc
            .get_record(&f.ctx, &key("k"))
            .await
            .expect("read")
            .is_some()
    );
    f.svc
        .put(
            &f.ctx,
            &key("other"),
            write_create("v"),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("create");
}

// ── delete ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_records_the_purge_debt_and_executes_it_after_the_commit() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect("delete");
    assert_eq!(
        f.repo.recorded_tasks(),
        [CleanupTask::Purge(row.store_key())]
    );
    assert_eq!(f.metrics.store_cleanup_recorded(), [CleanupOp::Purge]);
    assert_eq!(f.plugin.delete_key_calls(), [row.store_key()]);
    assert!(!f.plugin.holds_key(&row.store_key()));
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn a_failing_purge_leaves_the_debt_and_the_delete_still_succeeds() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.plugin.fail_next_delete_keys(1);
    f.svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect("the reply does not depend on the purge");
    assert!(f.repo.rows().is_empty());
    assert_eq!(
        f.repo.pending_tasks(),
        [CleanupTask::Purge(row.store_key())]
    );
    assert_eq!(f.metrics.store_cleanup_failed(), [CleanupOp::Purge]);
    assert!(f.plugin.holds_key(&row.store_key()));
}

// ── delete: verification after an ambiguous commit ──────────────────────────

#[tokio::test]
async fn ambiguous_delete_that_did_commit_is_verified_and_its_purge_executed() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_delete_by_id_after_commit(1);
    f.svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect("the row is gone: the delete committed");

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Delete, VerifyOutcome::Committed)]
    );
    assert!(f.repo.rows().is_empty());
    assert_eq!(f.plugin.delete_key_calls(), [row.store_key()]);
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn ambiguous_delete_that_did_not_commit_runs_the_delete_again() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_delete_by_id(1);
    f.svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect("the verification deletes the row");

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Delete, VerifyOutcome::NotCommitted)]
    );
    assert!(f.repo.rows().is_empty());
    assert_eq!(f.plugin.delete_key_calls(), [row.store_key()]);
    assert!(f.repo.pending_tasks().is_empty());
}

#[tokio::test]
async fn ambiguous_delete_of_a_row_the_scope_does_not_admit_is_not_found() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_delete_by_id(1);
    f.repo.force_next_verify_delete_not_found(1);
    let err = f
        .svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect_err("the verification finds a row the scope does not admit");
    assert!(matches!(err, DomainError::NotFound), "{err:?}");

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Delete, VerifyOutcome::NotCommitted)]
    );
    assert_eq!(f.repo.rows().len(), 1, "nothing deleted");
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing executed");
    assert!(f.plugin.holds_key(&row.store_key()));
}

#[tokio::test]
async fn ambiguous_delete_not_found_under_a_version_precondition_is_a_conflict() {
    let f = Fixture::new();
    let row = f.create_k().await;

    f.repo.fail_next_delete_by_id(1);
    f.repo.force_next_verify_delete_not_found(1);
    let err = f
        .svc
        .delete(
            &f.ctx,
            &key("k"),
            WritePrecondition::Version {
                id: row.id,
                version: row.version,
            },
        )
        .await
        .expect_err("the same answer as a not-found of the delete itself");
    assert!(matches!(err, DomainError::VersionConflict), "{err:?}");

    assert_eq!(f.repo.rows().len(), 1, "nothing deleted");
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing executed");
}

#[tokio::test]
async fn a_failing_delete_verification_is_503_and_executes_nothing() {
    let f = Fixture::new();
    let _row = f.create_k().await;

    f.repo.fail_next_delete_by_id(1);
    f.repo.fail_next_verifications(1);
    let err = f
        .svc
        .delete(&f.ctx, &key("k"), WritePrecondition::Exists)
        .await
        .expect_err("the outcome stays unknown");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    assert_eq!(
        f.metrics.commit_verifications(),
        [(VerifyOp::Delete, VerifyOutcome::Failed)]
    );
    assert_eq!(f.repo.rows().len(), 1);
    assert!(f.plugin.delete_key_calls().is_empty(), "nothing executed");
}

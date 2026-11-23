//! Staging one verified snapshot (P-D-268) over a test database, through the staging the
//! discovery runs (`stage_snapshot`): once per revision, a replay answers exactly what the first
//! staging answered, other content at the revision is a conflict, a lower revision is stale, and
//! nothing inside the transaction asks the registry.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::api::rest::ApiState;
use crate::api::rest::quota::discovery::stage_snapshot;
use crate::domain::quota::intake::{RejectedRecord, plan_intake};
use crate::domain::quota::record::{JudgeError, Resolutions, judge_record};
use crate::domain::quota::{CandidateState, NewQuotaTarget};
use crate::infra::storage::repo::quota_catalog_repo as catalog;
use crate::infra::storage::repo::quota_intake_repo as repo;
use crate::infra::storage::stored_now;
use crate::quota_test_support::{
    FakeQuotaRegistry, TEST_PLUGIN, seed_catalog, snapshot_of, vcpu_record, vcpu_record_numbered,
};
use crate::test_support::{
    TestDsn, authed_ctx, body_json, problem_code, products_statements, raw_i64, raw_string_opt,
    recorded_test_db, resolved_usage_types, rest_app_on_db, test_db,
};
use serde_json::{Value, json};
use std::sync::Arc;

struct IntakeFixture {
    /// Keeps the state's outbox running for the test's life.
    _app: axum::Router,
    state: Arc<ApiState>,
    scope: AccessScope,
    tenant: Uuid,
    actor: Uuid,
    target: QuotaTarget,
    registry: FakeQuotaRegistry,
    dsn: TestDsn,
}

impl IntakeFixture {
    async fn new() -> Self {
        let (db, scope, tenant, dsn) = test_db().await;
        Self::on(db, scope, tenant, dsn).await
    }

    /// A fixture over a database whose statements `recorder` records.
    async fn recorded() -> (Self, toolkit_db::test_support::QueryRecorder) {
        let (db, scope, tenant, dsn, recorder) = recorded_test_db().await;
        (Self::on(db, scope, tenant, dsn).await, recorder)
    }

    async fn on(
        db: toolkit_db::DBProvider<toolkit_db::DbError>,
        scope: AccessScope,
        tenant: Uuid,
        dsn: TestDsn,
    ) -> Self {
        let actor = Uuid::new_v4();
        let target = repo::insert_target(
            &db.conn().unwrap(),
            &scope,
            tenant,
            NewQuotaTarget {
                plugin_instance_id: TEST_PLUGIN.to_owned(),
                target_key: "pool-a".to_owned(),
                name: "Pool A".to_owned(),
            },
            actor,
            stored_now(),
        )
        .await
        .unwrap();
        let (app, state) = rest_app_on_db(
            tenant,
            |_, _| axum::Router::new(),
            resolved_usage_types(),
            "test",
            db,
        )
        .await;
        Self {
            _app: app,
            state,
            scope,
            tenant,
            actor,
            target,
            registry: seed_catalog(FakeQuotaRegistry::default()),
            dsn,
        }
    }

    /// Judges the records outside, plans against the target's approved mappings, then stages as
    /// the discovery does: the status and the body it answers.
    async fn stage(&self, s: &VerifiedSnapshot) -> (u16, Value) {
        let ctx = authed_ctx(self.tenant);
        let mut memo = Resolutions::default();
        let mut judged = Vec::new();
        for (index, raw) in s.items.iter().enumerate() {
            judged.push(
                match judge_record(&self.registry, &ctx, raw, &mut memo).await {
                    Ok(record) => Ok(record),
                    Err(JudgeError::Refused(refusal)) => {
                        Err(RejectedRecord::of(index, raw, refusal))
                    }
                    Err(JudgeError::Unavailable(m)) => panic!("{m}"),
                },
            );
        }
        let approved = catalog::approved_mappings_of_target(
            &self.state.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            self.target.id,
        )
        .await
        .unwrap();
        let plan = plan_intake(judged, &approved);
        self.registry.begin();
        let staged = stage_snapshot(
            &self.state,
            &self.scope,
            (self.tenant, self.actor),
            &self.target,
            (s, &plan),
            None,
        )
        .await;
        self.registry.end();
        let response = staged.unwrap_or_else(axum::response::IntoResponse::into_response);
        let status = response.status().as_u16();
        (status, body_json(response).await)
    }

    async fn count(&self, tail: &str) -> i64 {
        raw_i64(&self.dsn, &format!("SELECT COUNT(*) AS v FROM {tail}")).await
    }
}

/// The verified snapshot of `records` at `revision`, as discovery would answer it.
fn snapshot(revision: &str, records: &[Value]) -> VerifiedSnapshot {
    let page = snapshot_of(records, revision);
    let mut items = records.to_vec();
    items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    VerifiedSnapshot {
        snapshot_id: page.snapshot_id,
        source_revision: page.source_revision,
        observed_at: page.observed_at,
        item_count: page.item_count,
        content_digest: page.content_digest,
        items,
    }
}

#[tokio::test]
async fn the_same_snapshot_twice_is_one_snapshot_and_answers_the_stored_candidates() {
    let f = IntakeFixture::new().await;
    let (first, a) = f.stage(&snapshot("7", &[vcpu_record()])).await;
    let (again, b) = f.stage(&snapshot("7", &[vcpu_record()])).await;
    assert_eq!(
        (first, again),
        (201, 200),
        "a first staging and its replay: {a} {b}"
    );
    assert_eq!(
        a, b,
        "the replay answers exactly what the first staging answered"
    );
    assert_eq!(a["candidates"].as_array().map(Vec::len), Some(1), "{a}");
    assert_eq!(f.count("products_quota_snapshot").await, 1);
    assert_eq!(f.count("products_quota_candidate").await, 1);
    assert_eq!(
        f.count("products_audit_log WHERE action = 'quota_discovery.intake'")
            .await,
        1
    );
}

#[tokio::test]
async fn other_content_at_the_same_revision_is_a_conflict_and_writes_nothing() {
    let f = IntakeFixture::new().await;
    assert_eq!(f.stage(&snapshot("7", &[vcpu_record()])).await.0, 201);
    let mut changed = vcpu_record();
    changed["limits"]["max"] = json!("2048");
    let (status, body) = f.stage(&snapshot("7", &[changed])).await;
    assert_eq!(
        (status, problem_code(&body).as_str()),
        (409, "QUOTA_SOURCE_CONFLICT")
    );
    assert_eq!(f.count("products_quota_candidate").await, 1);
    assert_eq!(f.count("products_quota_snapshot").await, 1);
    assert_eq!(
        f.count("products_audit_log WHERE action = 'quota_discovery.intake'")
            .await,
        1
    );
}

#[tokio::test]
async fn a_lower_revision_is_stale() {
    let f = IntakeFixture::new().await;
    assert_eq!(f.stage(&snapshot("10", &[vcpu_record()])).await.0, 201);
    let (status, body) = f.stage(&snapshot("9", &[vcpu_record()])).await;
    assert_eq!(
        (status, problem_code(&body).as_str()),
        (409, "QUOTA_SOURCE_STALE")
    );
    assert_eq!(f.count("products_quota_snapshot").await, 1);
    assert_eq!(f.stage(&snapshot("11", &[vcpu_record()])).await.0, 201);
}

/// `stage_snapshot` takes no registry: its signature is the guard, and no call inside its
/// transaction can reach one. The armed fake proves the fixture's own judgement happens before the
/// transaction.
#[tokio::test]
async fn staging_asks_no_registry_inside_its_transaction() {
    let f = IntakeFixture::new().await;
    f.registry.panic_after_begin(); // the fake panics if called from here on
    assert_eq!(f.stage(&snapshot("7", &[vcpu_record()])).await.0, 201);
}

/// The stored revision decides inside the transaction: a replay reads the stored snapshot and its
/// candidates and runs no `INSERT`, so it never reaches the insert race.
#[tokio::test]
async fn a_replay_inside_the_transaction_runs_no_insert() {
    let (f, recorder) = IntakeFixture::recorded().await;
    assert_eq!(f.stage(&snapshot("7", &[vcpu_record()])).await.0, 201);
    recorder.clear();
    assert_eq!(f.stage(&snapshot("7", &[vcpu_record()])).await.0, 200);
    let inserts: Vec<_> = products_statements(&recorder)
        .into_iter()
        .filter(|sql| sql.to_ascii_uppercase().starts_with("INSERT"))
        .collect();
    assert!(inserts.is_empty(), "{inserts:?}");
}

/// The stored snapshot decides before the stale rule: a revision below the target's highest that
/// is stored with the same digest replays.
#[tokio::test]
async fn a_stored_lower_revision_replays_after_a_higher_one() {
    let f = IntakeFixture::new().await;
    let (status, first) = f.stage(&snapshot("7", &[vcpu_record()])).await;
    assert_eq!(status, 201, "a first staging: {first}");
    assert_eq!(f.stage(&snapshot("8", &[vcpu_record()])).await.0, 201);
    assert_eq!(
        f.stage(&snapshot("7", &[vcpu_record()])).await,
        (200, first)
    );
    assert_eq!(f.count("products_quota_snapshot").await, 2);
}

/// What one staging stores: the verified snapshot with its records sorted by id, its candidates'
/// evidence and its notes; one `staged` discovery candidate per valid record at version 1 with its
/// author; and one audit row naming the snapshot.
#[tokio::test]
async fn a_staged_snapshot_keeps_its_records_evidence_and_notes() {
    let f = IntakeFixture::new().await;
    let mut unknown = vcpu_record_numbered(9);
    unknown["metric_ref"] =
        json!("gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.nothing.v1");
    let records = [
        vcpu_record_numbered(2),
        unknown.clone(),
        vcpu_record_numbered(1),
    ];
    let s = snapshot("7", &records);
    let (status, body) = f.stage(&s).await;
    assert_eq!(status, 201, "a first staging: {body}");
    let conn = f.state.db.conn().unwrap();
    let stored = &repo::snapshot_at(&conn, &f.scope, f.tenant, f.target.id, "7")
        .await
        .unwrap()
        .expect("the snapshot is stored");
    assert_eq!(body["snapshot"]["id"], json!(stored.id));
    let candidates = repo::candidates_of_snapshot(&conn, &f.scope, f.tenant, stored.id)
        .await
        .unwrap();
    assert_eq!(
        (
            stored.target_id,
            stored.snapshot_id,
            stored.source_revision.as_str(),
            stored.item_count,
            stored.content_digest.as_str(),
            stored.created_by,
        ),
        (
            f.target.id,
            s.snapshot_id,
            "7",
            3,
            s.content_digest.as_str(),
            f.actor
        )
    );
    assert_eq!(stored.snapshot_json["items"], json!(s.items));
    assert_eq!(stored.snapshot_json["observed_at"], "2026-11-13T12:00:00Z");
    assert_eq!(stored.review_json["rejected"][0]["record"], unknown["id"]);
    assert_eq!(
        stored.review_json["rejected"][0]["code"],
        "QUOTA_REFERENCE_UNKNOWN"
    );
    assert_eq!(candidates.len(), 2);
    for candidate in &candidates {
        assert_eq!(
            (
                candidate.origin,
                candidate.state,
                candidate.version,
                candidate.snapshot_row_id,
                candidate.created_by
            ),
            (
                CandidateOrigin::Discovery,
                CandidateState::Staged,
                1,
                Some(stored.id),
                f.actor
            )
        );
        for id in candidate.evidence_json.as_object().unwrap().keys() {
            assert!(stored.evidence_json.get(id).is_some(), "{id}");
        }
    }
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.capability_key.as_str())
            .collect::<Vec<_>>(),
        vec!["tenant-vcpu-1", "tenant-vcpu-2"]
    );
    let subject = raw_string_opt(
        &f.dsn,
        "SELECT subject_kind AS v FROM products_audit_log WHERE action = 'quota_discovery.intake'",
    )
    .await;
    assert_eq!(subject.as_deref(), Some("quota_snapshot"));
}

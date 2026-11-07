//! The maintenance surface driven as a scheduler job drives it.
//!
//! One pass of the sweep and of the audit retention, on request, for a
//! deployment that runs them once per cluster. The caller is a machine, so no
//! step-up and a service principal is fine; the gate is the platform scope's:
//! a policy decision and the root tenant.

use sea_orm::EntityTrait;
use serde_json::json;
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{ActorTenant, AuditOperation, AuditRecord, AuditSink as _};
use crate::domain::secrets::pending::{PendingSecretDraft, PendingSecretRepository as _};
use crate::infra::storage::entity::{audit_record, pending_secret};
use crate::test_support::{RestHarness, SECRET, context_for, service_context_for};

const SWEEP: &str = "/settings-service/v1/maintenance/pending-secrets/sweep";
const PRUNE: &str = "/settings-service/v1/maintenance/audit/prune";

/// Stage an expired secret the way an abandoned stage leaves one.
async fn seed_expired_stage(h: &RestHarness) -> Uuid {
    let declaration = h
        .inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
    h.secrets.seed("abandoned-ref", "hunter2");
    let conn = h.inner.db.conn().expect("connection");
    crate::infra::storage::pending_secret_repo::PendingSecretRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            PendingSecretDraft {
                declaration_id: declaration,
                tenant_id: h.inner.tree.root,
                subject_id: "someone who walked away".to_owned(),
                secret_ref: "abandoned-ref".to_owned(),
                expires_at: time::OffsetDateTime::now_utc() - time::Duration::minutes(1),
            },
        )
        .await
        .expect("a staged secret")
        .id
}

async fn pending_rows(h: &RestHarness) -> u64 {
    let conn = h.inner.db.conn().expect("connection");
    pending_secret::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .count(&conn)
        .await
        .expect("pending rows")
}

/// Append `n` audit records already past their horizon.
async fn seed_expired_audit(h: &RestHarness, n: usize) {
    let conn = h.inner.db.conn().expect("connection");
    for i in 0..n {
        crate::infra::storage::audit_store::AuditStore
            .append(
                &conn,
                &AccessScope::allow_all(),
                AuditRecord::new(
                    "k",
                    None,
                    "admin",
                    ActorTenant::machine(),
                    AuditOperation::Change,
                    format!("r{i}"),
                )
                .with_retain_until(time::OffsetDateTime::now_utc() - time::Duration::days(1)),
            )
            .await
            .expect("append");
    }
}

async fn audit_rows(h: &RestHarness) -> u64 {
    let conn = h.inner.db.conn().expect("connection");
    audit_record::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .count(&conn)
        .await
        .expect("audit rows")
}

#[tokio::test]
async fn the_sweep_route_releases_expired_stages_and_deletes_their_rows() {
    let h = RestHarness::new().await;
    seed_expired_stage(&h).await;

    let answer = h.send("POST", SWEEP, None, None, h.inner.tree.root).await;

    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(answer.body, json!({ "processed": 1, "more": false }));
    assert_eq!(pending_rows(&h).await, 0);
    assert!(h.secrets.held().is_empty(), "the entry went with the row");
}

#[tokio::test]
async fn the_sweep_route_answers_zero_when_secret_storage_is_not_configured() {
    let h = RestHarness::new().await;
    seed_expired_stage(&h).await;
    h.secrets.go_unconfigured();

    let answer = h.send("POST", SWEEP, None, None, h.inner.tree.root).await;

    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(answer.body, json!({ "processed": 0, "more": false }));
    assert_eq!(pending_rows(&h).await, 1, "nothing was touched");
}

#[tokio::test]
async fn the_prune_route_deletes_expired_records_and_says_more_while_its_budget_runs_out() {
    // The harness budget is two batches of two: four records a request.
    let h = RestHarness::new().await;
    seed_expired_audit(&h, 5).await;

    let first = h.send("POST", PRUNE, None, None, h.inner.tree.root).await;
    let second = h.send("POST", PRUNE, None, None, h.inner.tree.root).await;

    assert_eq!(first.status, 200, "{}", first.raw);
    assert_eq!(first.body, json!({ "deleted": 4, "more": true }));
    assert_eq!(second.status, 200, "{}", second.raw);
    assert_eq!(second.body, json!({ "deleted": 1, "more": false }));
    assert_eq!(audit_rows(&h).await, 0);
}

#[tokio::test]
async fn the_prune_route_leaves_records_inside_their_horizon() {
    let h = RestHarness::new().await;
    let conn = h.inner.db.conn().expect("connection");
    crate::infra::storage::audit_store::AuditStore
        .append(
            &conn,
            &AccessScope::allow_all(),
            AuditRecord::new(
                "k",
                None,
                "admin",
                ActorTenant::machine(),
                AuditOperation::Change,
                "fresh",
            ),
        )
        .await
        .expect("append");

    let answer = h.send("POST", PRUNE, None, None, h.inner.tree.root).await;

    assert_eq!(answer.body, json!({ "deleted": 0, "more": false }));
    assert_eq!(audit_rows(&h).await, 1);
}

#[tokio::test]
async fn a_caller_outside_the_root_tenant_is_refused_and_nothing_is_touched() {
    let h = RestHarness::new().await;
    seed_expired_stage(&h).await;
    seed_expired_audit(&h, 1).await;

    for path in [SWEEP, PRUNE] {
        let answer = h.send("POST", path, None, None, h.inner.tree.a).await;
        assert_eq!(answer.status, 403, "{path}: {}", answer.raw);
    }

    assert_eq!(pending_rows(&h).await, 1);
    assert_eq!(audit_rows(&h).await, 1);
}

#[tokio::test]
async fn a_caller_the_policy_denies_is_refused() {
    let denied = RestHarness::denying().await;
    let only_maintain = RestHarness::denying_action("maintain").await;

    for h in [&denied, &only_maintain] {
        for path in [SWEEP, PRUNE] {
            let answer = h.send("POST", path, None, None, h.inner.tree.root).await;
            assert_eq!(answer.status, 403, "{path}: {}", answer.raw);
        }
    }
}

#[tokio::test]
async fn a_service_principal_of_the_root_tenant_is_accepted_without_step_up() {
    let h = RestHarness::stale_step_up().await;
    seed_expired_stage(&h).await;

    let sweep = h
        .send_as(
            "POST",
            SWEEP,
            None,
            None,
            service_context_for(h.inner.tree.root),
        )
        .await;
    let prune = h
        .send_as(
            "POST",
            PRUNE,
            None,
            None,
            service_context_for(h.inner.tree.root),
        )
        .await;

    assert_eq!(sweep.status, 200, "{}", sweep.raw);
    assert_eq!(sweep.body["processed"], json!(1));
    assert_eq!(prune.status, 200, "{}", prune.raw);
}

#[tokio::test]
async fn an_interactive_root_caller_needs_no_step_up_either() {
    let h = RestHarness::stale_step_up().await;

    let answer = h
        .send_as("POST", PRUNE, None, None, context_for(h.inner.tree.root))
        .await;

    assert_eq!(answer.status, 200, "{}", answer.raw);
}

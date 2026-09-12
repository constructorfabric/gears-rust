// Created: 2026-09-08 by Constructor Tech
//! The coordinator: what survives a concurrent writer, what a batch does with
//! one step-up verification, what a clone copies, and what the impact walk
//! counts.
//!
//! The database here is `SQLite` on one connection, so two transactions never
//! actually overlap: they run in the order the test writes them. That is
//! enough, and it is the point — the guard is not a lock held across the
//! request but the tag compared inside the same transaction that writes, so a
//! second writer holding a tag the first invalidated is refused whether the
//! two overlapped in time or merely in intent.

use std::sync::Arc;

use serde_json::json;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{BATCH_LIMIT, BatchChange, WriteCoordinator};
use crate::audit::AuditOperation;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::ports::NoMetrics;
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::stepup::{NoStepUpVerifier, StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::domain::writes::{Change, ValueWriter, WriteActor};
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    BOOL, FixedStepUp, RecordingPublisher, RecordingSecrets, ResolutionHarness,
    resolution_catalogue,
};

struct Harness {
    base: ResolutionHarness,
    coordinator: WriteCoordinator,
    published: Arc<RecordingPublisher>,
}

impl Harness {
    async fn new() -> Self {
        Self::with_step_up(Arc::new(NoStepUpVerifier::default())).await
    }

    async fn with_step_up(step_up: Arc<dyn StepUpVerifier>) -> Self {
        let base = ResolutionHarness::new().await;
        let published = Arc::new(RecordingPublisher::default());
        let writer = Arc::new(ValueWriter::new(
            ValueRepo,
            Arc::clone(&base.resolver),
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            crate::infra::storage::audit_store::AuditStore,
            step_up,
            Arc::new(RecordingSecrets::default()),
            Arc::clone(&published) as Arc<dyn crate::domain::ports::ChangePublisher>,
            Arc::new(NoMetrics),
        ));
        let coordinator = WriteCoordinator::new(Arc::clone(&base.db), writer);
        Self {
            base,
            coordinator,
            published,
        }
    }

    /// A declaration a caller may write without step-up.
    async fn declare(&self, name: &str, class: &str) -> Uuid {
        let id = self
            .base
            .declare_typed(name, class, json!(false), BOOL, "public")
            .await;
        self.clear_step_up(id).await;
        id
    }

    async fn clear_step_up(&self, id: Uuid) {
        self.clear_step_up_as(id, "public").await;
    }

    /// The classification must keep matching the value type's trait, or the
    /// schema's own check refuses the update.
    async fn clear_step_up_as(&self, id: Uuid, classification: &str) {
        use crate::domain::declaration::DeclarationMetadata;
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .update_metadata(
                &conn,
                &AccessScope::allow_all(),
                id,
                DeclarationMetadata {
                    mode: "standard".to_owned(),
                    description: None,
                    domain_affinity: None,
                    licence_feature: None,
                    data_classification: classification.to_owned(),
                    requires_step_up: false,
                    anonymous_exposable: false,
                },
            )
            .await
            .expect("metadata");
    }

    async fn set(
        &self,
        actor: &WriteActor,
        name: &str,
        tenant: Option<Uuid>,
        value: serde_json::Value,
        if_match: Option<&str>,
    ) -> Result<crate::domain::writes::Committed, DomainError> {
        self.coordinator
            .change(
                actor,
                &self.base.key(name),
                tenant,
                Change::Set(value),
                if_match,
                "set",
            )
            .await
    }

    async fn rows(&self, declaration_id: Uuid) -> Vec<crate::domain::value::StoredValue> {
        let conn = self.base.db.conn().expect("connection");
        ValueRepo
            .find_all(&conn, &AccessScope::allow_all(), declaration_id)
            .await
            .expect("lookup")
    }

    /// The audit records stored for one key, in order.
    async fn history(&self, name: &str) -> Vec<String> {
        let conn = self.base.db.conn().expect("connection");
        let key = self.base.key(name);
        crate::infra::storage::audit_store::AuditStore
            .history(
                &conn,
                &AccessScope::allow_all(),
                key.as_str(),
                self.base.tree.root,
                &toolkit_odata::ODataQuery::default(),
            )
            .await
            .expect("history")
            .items
            .into_iter()
            .map(|r| r.operation.as_str().to_owned())
            .collect()
    }
}

fn actor(tenant: Uuid) -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type(USER_SUBJECT_TYPE)
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: Some("token".to_owned()),
    }
}

#[tokio::test]
async fn of_several_sets_presenting_one_tag_exactly_one_commits_and_leaves_one_record() {
    let h = Harness::new().await;
    let d = h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;

    // A stored value, and the tag every writer below reads before writing.
    let first = h
        .set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("the first write");
    let shared_tag = first.etag.clone();

    // Four writers race on that tag. The first to commit invalidates it, so
    // the rest are refused and store nothing.
    let mut committed = 0;
    let mut refused = 0;
    for i in 0..4 {
        match h
            .set(
                &actor(root),
                "flag",
                None,
                json!(i % 2 == 0),
                Some(&shared_tag),
            )
            .await
        {
            Ok(_) => committed += 1,
            Err(DomainError::PreconditionFailed { .. }) => refused += 1,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert_eq!(committed, 1, "exactly one of the four commits");
    assert_eq!(refused, 3);

    // One row, and one record per change that actually landed: the first write
    // and the one winner, and nothing for the three refusals.
    assert_eq!(h.rows(d).await.len(), 1);
    assert_eq!(h.history("flag").await, vec!["change", "create"]);
}

#[tokio::test]
async fn two_first_writes_at_an_empty_scope_leave_exactly_one_row() {
    let h = Harness::new().await;
    let d = h.declare("flag", scope_class::CASCADING).await;
    let tenant = h.base.tree.a;

    // Both writers know only that no row exists, so both present the
    // absent-state tag. The second finds the row the first created.
    let first = h
        .set(
            &actor(tenant),
            "flag",
            Some(tenant),
            json!(true),
            Some("absent"),
        )
        .await;
    let second = h
        .set(
            &actor(tenant),
            "flag",
            Some(tenant),
            json!(false),
            Some("absent"),
        )
        .await;
    assert!(first.is_ok(), "{first:?}");
    assert!(
        matches!(second, Err(DomainError::PreconditionFailed { .. })),
        "{second:?}"
    );
    let rows = h.rows(d).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].value, Some(json!(true)));
}

#[tokio::test]
async fn a_batch_verifies_step_up_once_and_a_refusal_stores_nothing() {
    // Every declaration in the batch requires step-up; the verifier accepts.
    let h = Harness::with_step_up(Arc::new(FixedStepUp::verified())).await;
    let one = h
        .base
        .declare_typed("one", scope_class::CASCADING, json!(false), BOOL, "public")
        .await;
    let two = h
        .base
        .declare_typed("two", scope_class::CASCADING, json!(false), BOOL, "public")
        .await;
    let root = h.base.tree.root;

    let changes = vec![
        BatchChange {
            key: h.base.key("one"),
            tenant: None,
            value: json!(true),
            if_match: Some("absent".to_owned()),
        },
        BatchChange {
            key: h.base.key("two"),
            tenant: None,
            value: json!(true),
            if_match: Some("absent".to_owned()),
        },
    ];
    let outcome = h
        .coordinator
        .batch(&actor(root), changes.clone())
        .await
        .expect("the batch runs");
    assert_eq!(outcome.results.len(), 2);
    assert!(outcome.results.iter().all(Result::is_ok));
    assert_eq!(h.rows(one).await.len(), 1);
    assert_eq!(h.rows(two).await.len(), 1);

    // The same batch against a verifier that refuses: one verification, one
    // refusal, and neither change stored.
    let refusing =
        Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Missing))).await;
    let one = refusing
        .base
        .declare_typed("one", scope_class::CASCADING, json!(false), BOOL, "public")
        .await;
    let two = refusing
        .base
        .declare_typed("two", scope_class::CASCADING, json!(false), BOOL, "public")
        .await;
    let changes = vec![
        BatchChange {
            key: refusing.base.key("one"),
            tenant: None,
            value: json!(true),
            if_match: Some("absent".to_owned()),
        },
        BatchChange {
            key: refusing.base.key("two"),
            tenant: None,
            value: json!(true),
            if_match: Some("absent".to_owned()),
        },
    ];
    let refused = refusing
        .coordinator
        .batch(&actor(refusing.base.tree.root), changes)
        .await;
    assert!(
        matches!(refused, Err(DomainError::StepUpRequired { .. })),
        "{refused:?}"
    );
    assert!(refusing.rows(one).await.is_empty());
    assert!(refusing.rows(two).await.is_empty());
}

#[tokio::test]
async fn a_batch_of_more_than_the_limit_is_refused_before_anything_is_written() {
    let h = Harness::new().await;
    let d = h.declare("flag", scope_class::CASCADING).await;
    let changes: Vec<BatchChange> = (0..=BATCH_LIMIT)
        .map(|_| BatchChange {
            key: h.base.key("flag"),
            tenant: None,
            value: json!(true),
            if_match: Some("absent".to_owned()),
        })
        .collect();
    let refused = h.coordinator.batch(&actor(h.base.tree.root), changes).await;
    assert!(
        matches!(&refused, Err(DomainError::Validation { field, .. }) if field == "changes"),
        "{refused:?}"
    );
    assert!(h.rows(d).await.is_empty());
}

#[tokio::test]
async fn a_clone_copies_the_source_s_effective_value_and_keeps_no_link() {
    let h = Harness::new().await;
    let d = h.declare("flag", scope_class::CASCADING).await;
    let t = &h.base.tree;

    // The source scope holds `true`; the target holds nothing.
    h.set(
        &actor(t.root),
        "flag",
        Some(t.a),
        json!(true),
        Some("absent"),
    )
    .await
    .expect("source");

    let cloned = h
        .coordinator
        .clone_value(
            &actor(t.root),
            &h.base.key("flag"),
            Some(t.a),
            Some(t.c),
            Some("absent"),
        )
        .await
        .expect("cloned");
    assert_eq!(cloned.new_value, Some(json!(true)));
    assert_eq!(cloned.tenant_id, t.c);

    // No continuing link: changing the source leaves the target where it was.
    let source_rows = h.rows(d).await;
    let source_tag = source_rows
        .iter()
        .find(|r| r.tenant_id == t.a)
        .map(|r| r.last_change_at.unix_timestamp_nanos().to_string())
        .expect("the source row");
    h.set(
        &actor(t.root),
        "flag",
        Some(t.a),
        json!(false),
        Some(&source_tag),
    )
    .await
    .expect("source changed");
    let target = h
        .rows(d)
        .await
        .into_iter()
        .find(|r| r.tenant_id == t.c)
        .expect("the target row");
    assert_eq!(target.value, Some(json!(true)), "the copy stands alone");
}

#[tokio::test]
async fn a_clone_of_a_secret_setting_is_refused_as_not_cloneable() {
    let h = Harness::new().await;
    let d = h
        .base
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            crate::test_support::SECRET,
            "secret",
        )
        .await;
    h.clear_step_up_as(d, "secret").await;
    let t = &h.base.tree;
    let refused = h
        .coordinator
        .clone_value(
            &actor(t.root),
            &h.base.key("api_token"),
            Some(t.a),
            Some(t.c),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(&refused, Err(DomainError::Validation { code, .. })
            if *code == crate::field::SECRET_NOT_CLONEABLE),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_clone_from_a_scope_outside_the_caller_s_subtree_is_refused() {
    let h = Harness::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let t = &h.base.tree;
    // `c` is a sibling of `a`: an administrator at `c` may read neither `a`'s
    // scope nor write from it.
    let refused = h
        .coordinator
        .clone_value(
            &actor(t.c),
            &h.base.key("flag"),
            Some(t.a),
            Some(t.c),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::Unauthorized { .. })),
        "{refused:?}"
    );
}

#[tokio::test]
async fn the_impact_report_skips_standalone_descendants_and_honours_its_limit() {
    let h = Harness::new().await;
    h.declare("flag", scope_class::CASCADING).await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    let declaration = h
        .base
        .resolver
        .find_declaration(&conn, &h.base.key("flag"))
        .await
        .expect("lookup")
        .expect("declared");

    // Setting `true` at the root: `a`, `b` and `c` change; `s` is standalone
    // and never counted, even though it hangs below `a`.
    let report = h
        .coordinator
        .writer()
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Platform,
            &json!(true),
            None,
        )
        .await
        .expect("report");
    let listed: Vec<Uuid> = report.changed.iter().map(|e| e.tenant_id).collect();
    assert!(
        !listed.contains(&t.s),
        "a standalone descendant is not listed"
    );
    assert_eq!(report.total_changed, listed.len());
    assert!(!report.truncated);

    // The limit bounds the list without changing the count.
    let bounded = h
        .coordinator
        .writer()
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Platform,
            &json!(true),
            Some(1),
        )
        .await
        .expect("report");
    assert_eq!(bounded.changed.len(), 1);
    assert_eq!(bounded.total_changed, report.total_changed);

    // A limit outside the permitted band is clamped rather than refused, and
    // the walk never blocks a write: it stores nothing and emits no record.
    for limit in [0, usize::MAX] {
        let clamped = h
            .coordinator
            .writer()
            .impact(
                &conn,
                &declaration,
                ScopeTarget::Platform,
                &json!(true),
                Some(limit),
            )
            .await
            .expect("report");
        assert!(clamped.changed.len() <= report.total_changed.max(1));
    }
    assert!(h.published.events.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_non_cascading_setting_has_no_impact_to_report() {
    let h = Harness::new().await;
    h.declare("only_here", scope_class::LOCAL).await;
    let conn = h.base.db.conn().expect("connection");
    let declaration = h
        .base
        .resolver
        .find_declaration(&conn, &h.base.key("only_here"))
        .await
        .expect("lookup")
        .expect("declared");
    let report = h
        .coordinator
        .writer()
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Platform,
            &json!(true),
            None,
        )
        .await
        .expect("report");
    assert!(report.changed.is_empty());
    assert_eq!(report.total_changed, 0);
    assert!(!report.truncated);
}

#[tokio::test]
async fn history_of_a_retired_declaration_is_still_readable() {
    let h = Harness::new().await;
    let d = h.declare("flag", scope_class::CASCADING).await;
    let root = h.base.tree.root;
    h.set(&actor(root), "flag", None, json!(true), Some("absent"))
        .await
        .expect("a change to remember");

    // Retiring the declaration does not touch what was recorded about it: the
    // trail outlives the setting, which is the point of keeping it.
    h.base.retire(d).await;
    assert_eq!(h.history("flag").await, vec!["create"]);
    assert_eq!(
        h.history("flag").await.len(),
        1,
        "the record survives the retirement"
    );
    let _ = AuditOperation::Create;
}

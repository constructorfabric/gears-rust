// Created: 2026-09-07 by Constructor Tech
//! The write path over the resolution harness: gates, commit, fallthrough.

use std::sync::Arc;

use serde_json::{Value, json};
use settings_service_sdk::EffectiveSource;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{Change, Committed, Gated, StepUpPolicy, ValueWriter, WriteActor};
use crate::audit::{AuditOperation, AuditValue};
use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess};
use crate::domain::error::DomainError;
use crate::domain::ports::{NoMetrics, NoSecretManager, SecretManager, ValueEvent};
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::stepup::{NoStepUpVerifier, StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    FixedStepUp, RecordingAudit, RecordingPublisher, RecordingSecrets, ResolutionHarness, SECRET,
    resolution_catalogue,
};

type Writer = ValueWriter<DeclarationRepo, ValueRepo, AccessRepo, Arc<RecordingAudit>>;

struct WriteHarness {
    base: ResolutionHarness,
    writer: Arc<Writer>,
    audit: Arc<RecordingAudit>,
    published: Arc<RecordingPublisher>,
}

impl WriteHarness {
    async fn new() -> Self {
        Self::with_step_up(Arc::new(NoStepUpVerifier::default())).await
    }

    async fn with_step_up(step_up: Arc<dyn StepUpVerifier>) -> Self {
        Self::build(step_up, Arc::new(NoSecretManager)).await
    }

    /// A harness whose Secret Manager keeps plaintext in memory and remembers
    /// what it released.
    async fn with_secrets() -> (Self, Arc<RecordingSecrets>) {
        let secrets = Arc::new(RecordingSecrets::default());
        let harness = Self::build(
            Arc::new(NoStepUpVerifier::default()),
            Arc::clone(&secrets) as Arc<dyn SecretManager>,
        )
        .await;
        (harness, secrets)
    }

    async fn build(step_up: Arc<dyn StepUpVerifier>, secrets: Arc<dyn SecretManager>) -> Self {
        let base = ResolutionHarness::new().await;
        let audit = Arc::new(RecordingAudit::default());
        let published = Arc::new(RecordingPublisher::default());
        let writer = Arc::new(ValueWriter::new(
            ValueRepo,
            Arc::clone(&base.resolver),
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            Arc::clone(&audit),
            step_up,
            secrets,
            Arc::clone(&published) as Arc<dyn crate::domain::ports::ChangePublisher>,
            Arc::new(NoMetrics),
        ));
        Self {
            base,
            writer,
            audit,
            published,
        }
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

fn service_actor(tenant: Uuid) -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type("gts.cf.core.security.subject_service.v1~")
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: None,
    }
}

impl WriteHarness {
    /// Declare without step-up unless asked: most tests are about the rest.
    async fn declare(&self, name: &str, class: &str, default: Value) -> Uuid {
        let id = self.base.declare(name, class, default).await;
        self.clear_step_up(id).await;
        id
    }

    async fn clear_step_up(&self, id: Uuid) {
        self.clear_step_up_as(id, "public").await;
    }

    async fn clear_step_up_as(&self, id: Uuid, classification: &str) {
        use crate::domain::declaration::{DeclarationMetadata, DeclarationRepository};
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

    async fn gate(
        &self,
        actor: &WriteActor,
        name: &str,
        target: Option<Uuid>,
    ) -> Result<Gated, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.writer
            .gate(
                &conn,
                actor,
                &self.base.key(name),
                target,
                StepUpPolicy::Verify,
                "set",
            )
            .await
    }

    /// Gate, then commit in one transaction, then the after-commit step.
    async fn write(
        &self,
        actor: &WriteActor,
        name: &str,
        target: Option<Uuid>,
        change: Change,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        let gated = self.gate(actor, name, target).await?;
        // The coordinator's sequence: stage outside, commit inside, discard on
        // a refusal so a staged secret never outlives its write.
        let staged = self.writer.stage(&gated, change).await?;
        let writer = Arc::clone(&self.writer);
        let actor_owned = actor.clone();
        let if_match = if_match.map(str::to_owned);
        let gated_owned = gated.clone();
        let staged_owned = staged.clone();
        let outcome = self
            .base
            .db
            .db()
            .transaction_ref_mapped::<_, Committed, DomainError>(move |tx| {
                Box::pin(async move {
                    writer
                        .commit_in(
                            tx,
                            &gated_owned,
                            &staged_owned,
                            if_match.as_deref(),
                            &actor_owned,
                            Uuid::new_v4(),
                        )
                        .await
                })
            })
            .await;
        let committed = match outcome {
            Ok(committed) => committed,
            Err(err) => {
                self.writer.discard(&gated, &staged).await;
                return Err(err);
            }
        };
        self.writer.after_commit(&committed, actor).await;
        Ok(committed)
    }

    async fn restrict(&self, declaration: Uuid, tenant: Uuid, access: TenantAccess) {
        let conn = self.base.db.conn().expect("connection");
        AccessRepo
            .upsert(
                &conn,
                &AccessScope::allow_all(),
                RestrictionDraft {
                    declaration_id: declaration,
                    tenant_id: tenant,
                    access,
                    set_by: "root-admin".to_owned(),
                },
            )
            .await
            .expect("row");
    }
}

#[tokio::test]
async fn a_set_creates_the_row_with_its_record_and_the_read_sees_it() {
    let h = WriteHarness::new().await;
    h.declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let root = h.base.tree.root;
    let admin = actor(root);

    let committed = h
        .write(
            &admin,
            "strict",
            Some(h.base.tree.a),
            Change::Set(json!(true)),
            Some("absent"),
        )
        .await
        .expect("commits");
    assert_eq!(committed.operation, AuditOperation::Create);
    assert_eq!(
        (committed.old_value.clone(), committed.new_value.clone()),
        (None, Some(json!(true)))
    );
    assert_ne!(committed.etag, "absent");
    assert_eq!(h.audit.operations(), vec!["create"]);
    assert!(matches!(
        h.published.events.lock().expect("lock").as_slice(),
        [ValueEvent::Changed { .. }]
    ));

    let read = h
        .base
        .resolve("strict", ScopeTarget::Tenant(h.base.tree.b))
        .await
        .expect("resolves");
    assert_eq!(read.value, json!(true));
    assert_eq!(read.source, EffectiveSource::Inherited);

    // A re-set presents the tag the write returned and is recorded as a change.
    let again = h
        .write(
            &admin,
            "strict",
            Some(h.base.tree.a),
            Change::Set(json!(false)),
            Some(&committed.etag),
        )
        .await
        .expect("commits");
    assert_eq!(again.operation, AuditOperation::Change);
    assert_eq!(again.old_value, Some(json!(true)));
    assert_eq!(h.audit.operations(), vec!["create", "change"]);
}

#[tokio::test]
async fn a_stale_or_missing_tag_stores_nothing() {
    let h = WriteHarness::new().await;
    h.declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let admin = actor(h.base.tree.root);
    let first = h
        .write(
            &admin,
            "strict",
            None,
            Change::Set(json!(true)),
            Some("absent"),
        )
        .await
        .expect("commits");

    let stale = h
        .write(
            &admin,
            "strict",
            None,
            Change::Set(json!(false)),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(stale, Err(DomainError::PreconditionFailed { .. })),
        "{stale:?}"
    );
    let missing = h
        .write(&admin, "strict", None, Change::Set(json!(false)), None)
        .await;
    assert!(matches!(
        missing,
        Err(DomainError::PreconditionRequired { .. })
    ));

    let read = h
        .base
        .resolve("strict", ScopeTarget::Platform)
        .await
        .expect("resolves");
    assert_eq!(
        read.value,
        json!(true),
        "the stored value is the first writer's"
    );
    assert_eq!(h.audit.operations().len(), 1);
    let _ = first;
}

#[tokio::test]
async fn an_invalid_value_is_refused_with_field_detail_and_nothing_is_stored() {
    let h = WriteHarness::new().await;
    h.declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let admin = actor(h.base.tree.root);
    let refused = h
        .write(
            &admin,
            "strict",
            None,
            Change::Set(json!("not-a-bool")),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::Validation { .. })),
        "{refused:?}"
    );
    assert!(h.audit.operations().is_empty());
    assert!(h.published.events.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn the_gates_refuse_in_order() {
    let h = WriteHarness::new().await;
    let strict = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let global = h.declare("flag", scope_class::GLOBAL, json!(false)).await;
    let t = &h.base.tree;

    // Unknown key: not found. Retired: the distinct outcome.
    assert!(matches!(
        h.gate(&actor(t.root), "ghost", None).await,
        Err(DomainError::NotFound { .. })
    ));
    let retired = h
        .declare("gone", scope_class::CASCADING, json!(false))
        .await;
    h.base.retire(retired).await;
    assert!(matches!(
        h.gate(&actor(t.root), "gone", None).await,
        Err(DomainError::Retired { .. })
    ));

    // Outside the subtree, and a standalone descendant: denied.
    assert!(matches!(
        h.gate(&actor(t.a), "strict", Some(t.c)).await,
        Err(DomainError::Unauthorized { .. })
    ));
    assert!(matches!(
        h.gate(&actor(t.a), "strict", Some(t.s)).await,
        Err(DomainError::Unauthorized { .. })
    ));

    // A global setting takes no tenant-scoped value, root included as caller.
    assert!(matches!(
        h.gate(&actor(t.root), "flag", Some(t.a)).await,
        Err(DomainError::Conflict { .. })
    ));
    assert!(h.gate(&actor(t.root), "flag", None).await.is_ok());
    let _ = global;

    // A read-only tenant is refused as a writer; its overridable ancestor
    // writes at it and the value lands at the descendant.
    h.restrict(strict, t.b, TenantAccess::ReadOnly).await;
    assert!(matches!(
        h.gate(&actor(t.b), "strict", None).await,
        Err(DomainError::Unauthorized { .. })
    ));
    let committed = h
        .write(
            &actor(t.a),
            "strict",
            Some(t.b),
            Change::Set(json!(true)),
            Some("absent"),
        )
        .await
        .expect("the ancestor writes");
    assert_eq!(committed.tenant_id, t.b);

    // Hidden from the caller: absent, not forbidden.
    h.restrict(strict, t.c, TenantAccess::Hidden).await;
    assert!(matches!(
        h.gate(&actor(t.c), "strict", None).await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn step_up_is_asked_only_where_the_declaration_requires_it() {
    // No verifier bound: writes needing step-up refuse, others proceed.
    let h = WriteHarness::new().await;
    let needs = h
        .base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    h.declare("open", scope_class::CASCADING, json!(false))
        .await;
    let root = h.base.tree.root;

    let refused = h.gate(&actor(root), "guarded", None).await;
    match refused {
        Err(DomainError::StepUpRequired {
            reason,
            max_age_seconds,
            ..
        }) => {
            assert_eq!(reason, StepUpRefusal::NotConfigured.code());
            assert_eq!(max_age_seconds, 300);
        }
        other => panic!("expected a step-up refusal, got {other:?}"),
    }
    assert!(h.gate(&actor(root), "open", None).await.is_ok());

    // A service principal is refused before step-up is even consulted, and
    // writes freely where the flag is clear.
    assert!(matches!(
        h.gate(&service_actor(root), "guarded", None).await,
        Err(DomainError::Unauthorized { .. })
    ));
    assert!(h.gate(&service_actor(root), "open", None).await.is_ok());
    let _ = needs;

    // A verifier that accepts lets the interactive caller through; one that
    // refuses names its reason.
    let verified = WriteHarness::with_step_up(Arc::new(FixedStepUp::verified())).await;
    verified
        .base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    assert!(
        verified
            .gate(&actor(verified.base.tree.root), "guarded", None)
            .await
            .is_ok()
    );
    let stale =
        WriteHarness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Stale))).await;
    stale
        .base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    assert!(matches!(
        stale
            .gate(&actor(stale.base.tree.root), "guarded", None)
            .await,
        Err(DomainError::StepUpRequired {
            reason: "stale",
            ..
        })
    ));
}

#[tokio::test]
async fn revert_and_remove_clear_the_row_and_the_scope_falls_back() {
    let h = WriteHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let admin = actor(t.root);
    h.base.set(d, t.a, json!(true)).await;
    let own = h
        .write(
            &admin,
            "strict",
            Some(t.b),
            Change::Set(json!(false)),
            Some("absent"),
        )
        .await
        .expect("commits");

    let reverted = h
        .write(&admin, "strict", Some(t.b), Change::Revert, Some(&own.etag))
        .await
        .expect("reverts");
    assert_eq!(reverted.operation, AuditOperation::Revert);
    assert_eq!(
        (reverted.old_value.clone(), reverted.new_value.clone()),
        (Some(json!(false)), None)
    );
    assert_eq!(reverted.etag, "absent");
    let fallback = h
        .base
        .resolve("strict", ScopeTarget::Tenant(t.b))
        .await
        .expect("resolves");
    assert_eq!(
        (fallback.value.clone(), fallback.source),
        (json!(true), EffectiveSource::Inherited)
    );

    // Nothing left to revert: not found, and the ancestor's row is untouched.
    assert!(matches!(
        h.write(&admin, "strict", Some(t.b), Change::Revert, Some("absent"))
            .await,
        Err(DomainError::NotFound { .. })
    ));
    let at_a = h
        .base
        .resolve("strict", ScopeTarget::Tenant(t.a))
        .await
        .expect("resolves");
    assert_eq!(at_a.source, EffectiveSource::OwnOverride);
    assert_eq!(h.audit.operations(), vec!["create", "revert"]);
}

#[tokio::test]
async fn a_valid_re_set_clears_needs_review_and_evicts_the_cache() {
    let h = WriteHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    h.base.set_flagged(d, t.b, json!("bad")).await;
    let before = h
        .base
        .resolve("strict", ScopeTarget::Tenant(t.b))
        .await
        .expect("resolves");
    assert!(before.own_row.as_ref().is_some_and(|o| o.needs_review));
    let tag = before
        .own_row
        .as_ref()
        .map(|o| o.last_change_at.unix_timestamp_nanos().to_string())
        .expect("own row");

    h.write(
        &actor(t.root),
        "strict",
        Some(t.b),
        Change::Set(json!(true)),
        Some(&tag),
    )
    .await
    .expect("commits");

    assert!(
        h.base
            .cache
            .get(h.base.key("strict").as_str(), t.b)
            .is_none(),
        "evicted at the target"
    );
    let after = h
        .base
        .resolve("strict", ScopeTarget::Tenant(t.b))
        .await
        .expect("resolves");
    assert_eq!(after.source, EffectiveSource::OwnOverride);
    assert!(after.own_row.as_ref().is_some_and(|o| !o.needs_review));
    let conn = h.base.db.conn().expect("connection");
    let row = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), d, t.b)
        .await
        .expect("lookup")
        .expect("row");
    assert!(!row.needs_review && row.needs_review_detail.is_none());
}

#[tokio::test]
async fn a_secret_write_is_unavailable_while_nothing_is_bound_and_no_plaintext_lands() {
    let h = WriteHarness::new().await;
    let d = h
        .base
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    h.clear_step_up_as(d, "secret").await;
    let refused = h
        .write(
            &actor(h.base.tree.root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "{refused:?}"
    );
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), d, h.base.tree.root)
            .await
            .expect("lookup")
            .is_none()
    );
}

#[tokio::test]
async fn the_impact_walk_counts_only_descendants_the_candidate_would_change() {
    let h = WriteHarness::new().await;
    let d = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let t = &h.base.tree;
    let conn = h.base.db.conn().expect("connection");
    let declaration = h
        .base
        .resolver
        .find_declaration(&conn, &h.base.key("strict"))
        .await
        .expect("lookup")
        .expect("declared");

    // Setting `true` at `a`: `b` inherits and changes; `s` is standalone and
    // never counted; `c` is a sibling and not below `a` at all.
    let report = h
        .writer
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Tenant(t.a),
            &json!(true),
            None,
        )
        .await
        .expect("walks");
    assert_eq!(
        report
            .changed
            .iter()
            .map(|e| e.tenant_id)
            .collect::<Vec<_>>(),
        vec![t.b]
    );
    assert_eq!(
        (report.total_changed, report.scanned, report.truncated),
        (1, 1, false)
    );

    // An own row at `b` shields it; a candidate equal to the current value
    // changes nothing.
    h.base.set(d, t.b, json!(true)).await;
    h.base.cache.invalidate_key(h.base.key("strict").as_str());
    let shielded = h
        .writer
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Tenant(t.a),
            &json!(true),
            None,
        )
        .await
        .expect("walks");
    assert_eq!(shielded.total_changed, 0);
    let same = h
        .writer
        .impact(
            &conn,
            &declaration,
            ScopeTarget::Platform,
            &json!(false),
            Some(1),
        )
        .await
        .expect("walks");
    assert_eq!(
        same.total_changed, 0,
        "everything already resolves to the candidate"
    );
}

#[tokio::test]
async fn a_record_that_cannot_be_written_rolls_the_value_back() {
    let base = ResolutionHarness::new().await;
    let d = base
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let writer: ValueWriter<
        DeclarationRepo,
        ValueRepo,
        AccessRepo,
        crate::test_support::FailingSink,
    > = ValueWriter::new(
        ValueRepo,
        Arc::clone(&base.resolver),
        Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        crate::test_support::FailingSink,
        Arc::new(FixedStepUp::verified()),
        Arc::new(NoSecretManager),
        Arc::new(RecordingPublisher::default()),
        Arc::new(NoMetrics),
    );
    let writer = Arc::new(writer);
    let actor = WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(base.tree.root)
            .subject_type(USER_SUBJECT_TYPE)
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: Some("t".to_owned()),
    };
    let conn = base.db.conn().expect("connection");
    let gated = writer
        .gate(
            &conn,
            &actor,
            &base.key("strict"),
            None,
            StepUpPolicy::Verify,
            "set",
        )
        .await
        .expect("gated");
    let staged = writer
        .stage(&gated, Change::Set(json!(true)))
        .await
        .expect("staged");
    let outcome = base
        .db
        .db()
        .transaction_ref_mapped::<_, Committed, DomainError>(|tx| {
            let writer = Arc::clone(&writer);
            let gated = gated.clone();
            let staged = staged.clone();
            let actor = actor.clone();
            Box::pin(async move {
                writer
                    .commit_in(tx, &gated, &staged, Some("absent"), &actor, Uuid::new_v4())
                    .await
            })
        })
        .await;
    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), d, base.tree.root)
            .await
            .expect("lookup")
            .is_none(),
        "the value rolled back with its record"
    );
}

async fn declare_secret(h: &WriteHarness) -> Uuid {
    let d = h
        .base
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    h.clear_step_up_as(d, "secret").await;
    d
}

#[tokio::test]
async fn a_secret_write_stores_only_the_reference_and_masks_both_images() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    let d = declare_secret(&h).await;
    let root = h.base.tree.root;
    let committed = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await
        .expect("stored");
    assert!(committed.released_secret.is_none());

    let conn = h.base.db.conn().expect("connection");
    let row = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), d, root)
        .await
        .expect("lookup")
        .expect("row");
    assert!(row.value.is_none());
    let reference = row.secret_ref.clone().expect("the row holds the reference");
    assert_eq!(committed.new_value, Some(json!(reference)));
    assert_eq!(
        secrets
            .entries
            .lock()
            .expect("lock")
            .get(&reference)
            .map(String::as_str),
        Some("hunter2")
    );
    {
        let records = h.audit.records.lock().expect("lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].post_image, Some(AuditValue::Masked));
        assert!(
            !serde_json::to_string(&*records)
                .expect("json")
                .contains("hunter2")
        );
    }

    // A second set creates a new entry, points the row at it, and releases the
    // superseded one after the commit: one row, one live entry.
    let again = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter3")),
            Some(&committed.etag),
        )
        .await
        .expect("re-set");
    let newer = again
        .new_value
        .as_ref()
        .and_then(Value::as_str)
        .expect("a reference")
        .to_owned();
    assert_ne!(newer, reference);
    assert_eq!(again.released_secret.as_deref(), Some(reference.as_str()));
    assert_eq!(secrets.held(), vec![newer.clone()]);
    assert_eq!(
        secrets
            .entries
            .lock()
            .expect("lock")
            .get(&newer)
            .map(String::as_str),
        Some("hunter3")
    );
    assert_eq!(*secrets.deleted.lock().expect("lock"), vec![reference]);
}

#[tokio::test]
async fn a_set_refused_on_its_tag_releases_the_entry_it_created_and_keeps_the_live_one() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    declare_secret(&h).await;
    let root = h.base.tree.root;
    let live = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await
        .expect("stored");
    let live_ref = live
        .new_value
        .as_ref()
        .and_then(Value::as_str)
        .expect("a reference")
        .to_owned();

    // Stale tag: the plaintext already went to the store, so the refusal
    // releases that entry and the live one is untouched.
    let refused = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("intruder")),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::PreconditionFailed { .. })),
        "{refused:?}"
    );
    assert_eq!(secrets.held(), vec![live_ref.clone()]);
    assert_eq!(
        secrets
            .entries
            .lock()
            .expect("lock")
            .get(&live_ref)
            .map(String::as_str),
        Some("hunter2")
    );
    let deleted = secrets.deleted.lock().expect("lock");
    assert_eq!(deleted.len(), 1);
    assert_ne!(deleted[0], live_ref);
}

#[tokio::test]
async fn removing_a_secret_releases_its_entry_after_the_commit() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    declare_secret(&h).await;
    let root = h.base.tree.root;
    let set = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await
        .expect("stored");
    let reference = set
        .new_value
        .as_ref()
        .and_then(Value::as_str)
        .expect("a reference")
        .to_owned();

    let removed = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Remove,
            Some(&set.etag),
        )
        .await
        .expect("removed");
    assert_eq!(removed.released_secret.as_deref(), Some(reference.as_str()));
    assert_eq!(
        *secrets.deleted.lock().expect("lock"),
        vec![reference.clone()]
    );
    assert!(
        !secrets
            .entries
            .lock()
            .expect("lock")
            .contains_key(&reference)
    );

    // Set again afterwards: the entry is created anew under the same reference.
    h.write(
        &actor(root),
        "api_token",
        None,
        Change::Set(json!("fresh")),
        Some("absent"),
    )
    .await
    .expect("set again");
    assert_eq!(secrets.held().len(), 1);
    assert!(!secrets.held().contains(&reference));
}

#[tokio::test]
async fn a_store_that_cannot_answer_refuses_the_write_and_nothing_lands() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    let d = declare_secret(&h).await;
    let root = h.base.tree.root;
    secrets.go_down();
    let refused = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await;
    assert!(
        matches!(refused, Err(DomainError::Unavailable { .. })),
        "{refused:?}"
    );
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), d, root)
            .await
            .expect("lookup")
            .is_none()
    );
    assert!(h.audit.records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_release_does_not_fail_the_removal() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    let d = declare_secret(&h).await;
    let root = h.base.tree.root;
    let set = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await
        .expect("stored");
    secrets.go_down();
    let removed = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Remove,
            Some(&set.etag),
        )
        .await
        .expect("the removal stands");
    assert!(removed.released_secret.is_some());
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), d, root)
            .await
            .expect("lookup")
            .is_none()
    );
    assert!(secrets.deleted.lock().expect("lock").is_empty());
}

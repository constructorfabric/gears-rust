// Created: 2026-09-07 by Virtuozzo International GmbH
//! The write path over the resolution harness: gates, commit, fallthrough.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::{Value, json};
use settings_service_sdk::EffectiveSource;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use std::sync::atomic::Ordering;

use super::{
    Change, Committed, Gated, StagePrecondition, Staged, StepUpPolicy, ValueWriter, WriteActor,
};
use crate::audit::{AuditOperation, AuditValue};
use crate::domain::access::{AccessRepository, RestrictionDraft, TenantAccess};
use crate::domain::error::DomainError;
use crate::domain::ports::{NoMetrics, NoSecretManager, SecretManager, ValueEvent};
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::stepup::{StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    FixedStepUp, RecordingAudit, RecordingPublisher, RecordingSecrets, ResolutionHarness, SECRET,
    resolution_catalogue,
};

type Writer =
    ValueWriter<DeclarationRepo, ValueRepo, AccessRepo, Arc<RecordingAudit>, PendingSecretRepo>;

struct WriteHarness {
    base: ResolutionHarness,
    writer: Arc<Writer>,
    audit: Arc<RecordingAudit>,
    published: Arc<RecordingPublisher>,
}

impl WriteHarness {
    async fn new() -> Self {
        Self::with_step_up(Arc::new(FixedStepUp::refusing(
            StepUpRefusal::NotConfigured,
        )))
        .await
    }

    async fn with_step_up(step_up: Arc<dyn StepUpVerifier>) -> Self {
        Self::build(step_up, Arc::new(NoSecretManager)).await
    }

    /// A harness whose Secret Manager keeps plaintext in memory and remembers
    /// what it released.
    async fn with_secrets() -> (Self, Arc<RecordingSecrets>) {
        let secrets = Arc::new(RecordingSecrets::default());
        let harness = Self::build(
            Arc::new(FixedStepUp::refusing(StepUpRefusal::NotConfigured)),
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
            PendingSecretRepo,
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
        step_up_token: Some(SecretString::from("token".to_owned())),
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
        self.set_metadata(id, classification, false).await;
    }

    /// Rewrite the declaration's metadata as an administrator's PATCH would,
    /// with no tag: what a concurrent edit leaves behind.
    async fn set_metadata(&self, id: Uuid, classification: &str, requires_step_up: bool) {
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
                    requires_step_up,
                    anonymous_exposable: false,
                },
                None,
                true,
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
        self.commit_gated(actor, gated, change, if_match).await
    }

    /// The coordinator's sequence after the gate: stage outside, commit
    /// inside one transaction, discard on a refusal so a staged secret never
    /// outlives its write, the after-commit step on success.
    async fn commit_gated(
        &self,
        actor: &WriteActor,
        gated: Gated,
        change: Change,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        let staged = {
            let conn = self.base.db.conn().expect("connection");
            self.writer
                .stage(
                    &conn,
                    &gated,
                    actor,
                    change,
                    StagePrecondition::Judge(if_match),
                )
                .await?
        };
        self.commit_gated_staged(actor, gated, staged, if_match)
            .await
    }

    /// The commit alone, for a stage the test made earlier.
    async fn commit_gated_staged(
        &self,
        actor: &WriteActor,
        gated: Gated,
        staged: Staged,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
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
                let conn = self.base.db.conn().expect("connection");
                self.writer.discard(&conn, &gated, &staged).await;
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
                None,
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
async fn the_row_takes_the_classification_of_the_declaration_as_it_is_at_commit() {
    let h = WriteHarness::new().await;
    let id = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let admin = actor(h.base.tree.root);
    let tenant = h.base.tree.a;
    let gated = h
        .gate(&admin, "strict", Some(tenant))
        .await
        .expect("gated while public");

    // An administrator tightens the classification between the gate and the
    // commit. The resync that rides with that change can only touch rows that
    // exist at the time; this write's row does not yet.
    h.clear_step_up_as(id, "pii").await;

    let committed = h
        .commit_gated(&admin, gated, Change::Set(json!(true)), Some("absent"))
        .await
        .expect("commits");
    assert_eq!(committed.data_classification, "pii");
    let conn = h.base.db.conn().expect("connection");
    let row = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(
        row.data_classification, "pii",
        "stamped from the row as it is inside the transaction, not the gate's snapshot"
    );
}

#[tokio::test]
async fn a_write_gated_before_a_restriction_is_refused_at_commit_and_lands_nowhere() {
    let h = WriteHarness::new().await;
    let a = h.base.tree.a;
    let delegate = actor(a);

    // `read_only`: the writer is refused, as it would have been at the gate.
    let strict = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let gated = h
        .gate(&delegate, "strict", None)
        .await
        .expect("overridable at the gate");
    h.restrict(strict, a, TenantAccess::ReadOnly).await;
    let err = h
        .commit_gated(&delegate, gated, Change::Set(json!(true)), Some("absent"))
        .await
        .expect_err("refused at commit");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
    {
        let conn = h.base.db.conn().expect("connection");
        assert!(
            ValueRepo
                .find_one(&conn, &AccessScope::allow_all(), strict, a)
                .await
                .expect("lookup")
                .is_none(),
            "nothing lands past a restriction"
        );
    }

    // `hidden`: absent, as at the gate — a writer learns nothing it may not see.
    let quiet = h
        .declare("quiet", scope_class::CASCADING, json!(false))
        .await;
    let gated = h
        .gate(&delegate, "quiet", None)
        .await
        .expect("overridable at the gate");
    h.restrict(quiet, a, TenantAccess::Hidden).await;
    let err = h
        .commit_gated(&delegate, gated, Change::Set(json!(true)), Some("absent"))
        .await
        .expect_err("refused at commit");
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), quiet, a)
            .await
            .expect("lookup")
            .is_none()
    );
    assert!(h.audit.operations().is_empty(), "nothing to record");
}

#[tokio::test]
async fn a_write_gated_before_step_up_became_required_is_refused_at_commit() {
    // No verifier is configured: a write that needs step-up cannot get one.
    let h = WriteHarness::new().await;
    let id = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let admin = actor(h.base.tree.root);
    let tenant = h.base.tree.a;
    let gated = h
        .gate(&admin, "strict", Some(tenant))
        .await
        .expect("no step-up asked");

    // An administrator switches the requirement on between the gate and the
    // commit; no verified step-up stands behind this write.
    h.set_metadata(id, "public", true).await;
    let err = h
        .commit_gated(&admin, gated, Change::Set(json!(true)), Some("absent"))
        .await
        .expect_err("refused at commit");
    assert!(
        matches!(
            err,
            DomainError::StepUpRequired {
                reason: "missing",
                ..
            }
        ),
        "{err:?}"
    );
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), id, tenant)
            .await
            .expect("lookup")
            .is_none()
    );
}

#[tokio::test]
async fn a_write_gated_before_a_retire_is_refused_at_commit_and_lands_nowhere() {
    let h = WriteHarness::new().await;
    let id = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let admin = actor(h.base.tree.root);
    let gated = h
        .gate(&admin, "strict", Some(h.base.tree.a))
        .await
        .expect("active at the gate");

    // The declaration retires between the gate and the commit — an admin
    // retire or a module's major upgrade; either way the row is not live.
    {
        use crate::domain::declaration::DeclarationRepository;
        let conn = h.base.db.conn().expect("connection");
        DeclarationRepo
            .set_status(&conn, &AccessScope::allow_all(), id, "retired", None)
            .await
            .expect("retired");
    }

    let err = h
        .commit_gated(&admin, gated, Change::Set(json!(true)), Some("absent"))
        .await
        .expect_err("refused at commit");
    assert!(matches!(err, DomainError::Retired { .. }), "{err:?}");
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), id, h.base.tree.a)
            .await
            .expect("lookup")
            .is_none(),
        "nothing lands on a retired declaration"
    );
    assert!(h.audit.operations().is_empty(), "nothing to record");
}

#[tokio::test]
async fn the_row_write_itself_is_conditional_on_the_version_the_tag_was_compared_against() {
    use crate::domain::value::ValueDraft;
    let h = WriteHarness::new().await;
    let id = h
        .declare("strict", scope_class::CASCADING, json!(false))
        .await;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;
    let conn = h.base.db.conn().expect("connection");
    let all = AccessScope::allow_all();
    let row = ValueRepo
        .find_one(&conn, &all, id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    let stale = row.last_change_at - time::Duration::seconds(1);

    // The comparison ran against a read; a row that moved since finds no
    // match at the write and the writer gets the same `412` a stale tag gets.
    let refused = ValueRepo
        .update(&conn, &all, row.id, Some(json!(false)), None, "b", stale)
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let refused = ValueRepo
        .delete(&conn, &all, id, tenant, stale)
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let kept = ValueRepo
        .find_one(&conn, &all, id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(kept.value, Some(json!(true)), "untouched");

    // A first row is guarded by the unique index; the second first-writer
    // compared the absent-state tag, so its collision is a stale precondition.
    let duplicate = ValueRepo
        .insert(
            &conn,
            &all,
            ValueDraft {
                declaration_id: id,
                tenant_id: tenant,
                value: Some(json!(false)),
                secret_ref: None,
                data_classification: "public".to_owned(),
                needs_review: false,
                needs_review_detail: None,
                set_by: "b".to_owned(),
            },
        )
        .await
        .expect_err("second first writer");
    assert!(
        matches!(duplicate, DomainError::PreconditionFailed { .. }),
        "{duplicate:?}"
    );

    // At the version it read, the write lands.
    ValueRepo
        .update(
            &conn,
            &all,
            row.id,
            Some(json!(false)),
            None,
            "b",
            row.last_change_at,
        )
        .await
        .expect("current version");
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
async fn a_step_up_challenge_is_issued_only_for_a_write_the_caller_may_otherwise_make() {
    // No verifier is bound, so any write that reaches the step-up gate is
    // challenged; the question is which writes reach it.
    let h = WriteHarness::new().await;
    let guarded = h
        .base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    h.base
        .declare("gflag", scope_class::GLOBAL, json!(false))
        .await;
    let t = &h.base.tree;

    // A target outside the caller's subtree: refused, not challenged — the
    // caller learns nothing about what the setting would have asked of it.
    assert!(matches!(
        h.gate(&actor(t.a), "guarded", Some(t.c)).await,
        Err(DomainError::Unauthorized { .. })
    ));
    // A tenant-scoped write to a global setting: the conflict, not the challenge.
    assert!(matches!(
        h.gate(&actor(t.root), "gflag", Some(t.a)).await,
        Err(DomainError::Conflict { .. })
    ));
    // A read-only tenant: refused as a writer before anything is asked of it.
    h.restrict(guarded, t.b, TenantAccess::ReadOnly).await;
    assert!(matches!(
        h.gate(&actor(t.b), "guarded", None).await,
        Err(DomainError::Unauthorized { .. })
    ));
    // Only a caller entitled to the write is challenged for step-up.
    assert!(matches!(
        h.gate(&actor(t.a), "guarded", None).await,
        Err(DomainError::StepUpRequired { .. })
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

/// A caller labelled as the test says — or not labelled at all.
fn actor_labelled(tenant: Uuid, subject_type: Option<&str>) -> WriteActor {
    let mut ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant);
    if let Some(label) = subject_type {
        ctx = ctx.subject_type(label);
    }
    WriteActor {
        ctx: ctx.build().expect("context"),
        request_id: "req".to_owned(),
        step_up_token: Some(SecretString::from("token".to_owned())),
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
    }
}

#[tokio::test]
async fn a_person_is_recognised_in_both_vocabularies_and_nothing_else_is() {
    // The authorization design says the GTS type; a deployed realm maps
    // Keycloak's bare `user`. Both are a person: the request reaches the
    // verifier and gets its challenge, never the service-principal denial.
    let h =
        WriteHarness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Missing))).await;
    h.base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    let root = h.base.tree.root;
    for label in [USER_SUBJECT_TYPE, "user"] {
        let refused = h
            .gate(&actor_labelled(root, Some(label)), "guarded", None)
            .await;
        assert!(
            matches!(
                refused,
                Err(DomainError::StepUpRequired {
                    reason: "missing",
                    ..
                })
            ),
            "{label}: {refused:?}"
        );
    }

    // No label, or any other type — a machine's, an unrelated resource's, or a
    // near miss — is a service principal for step-up: refused before the
    // verifier is consulted. Absence of a label is no evidence of a person.
    for label in [
        None,
        Some("gts.cf.core.security.subject_service.v1~"),
        Some("gts.cf.core.hosts.host.v1~"),
        Some("User"),
    ] {
        let refused = h.gate(&actor_labelled(root, label), "guarded", None).await;
        assert!(
            matches!(refused, Err(DomainError::Unauthorized { .. })),
            "{label:?}: {refused:?}"
        );
    }

    // With a verifier that accepts, the `user`-labelled caller commits.
    let verified = WriteHarness::with_step_up(Arc::new(FixedStepUp::verified())).await;
    verified
        .base
        .declare("guarded", scope_class::CASCADING, json!(false))
        .await;
    let committed = verified
        .write(
            &actor_labelled(verified.base.tree.root, Some("user")),
            "guarded",
            None,
            Change::Set(json!(true)),
            Some("absent"),
        )
        .await
        .expect("a person labelled `user` completes a step-up-gated write");
    assert_eq!(committed.new_value, Some(json!(true)));
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
        PendingSecretRepo,
    > = ValueWriter::new(
        ValueRepo,
        Arc::clone(&base.resolver),
        Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        crate::test_support::FailingSink,
        Arc::new(FixedStepUp::verified()),
        Arc::new(NoSecretManager),
        PendingSecretRepo,
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
        step_up_token: Some(SecretString::from("t".to_owned())),
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
        .stage(
            &conn,
            &gated,
            &actor,
            Change::Set(json!(true)),
            StagePrecondition::Judge(Some("absent")),
        )
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

/// Every pending row, whatever its expiry.
async fn all_pending(h: &WriteHarness) -> Vec<crate::domain::secrets::pending::PendingSecret> {
    use crate::domain::secrets::pending::PendingSecretRepository;
    let conn = h.base.db.conn().expect("connection");
    crate::infra::storage::pending_secret_repo::PendingSecretRepo
        .list_expired(
            &conn,
            &AccessScope::allow_all(),
            time::OffsetDateTime::now_utc() + time::Duration::days(1),
            1_000,
        )
        .await
        .expect("listing")
}

#[tokio::test]
async fn a_store_that_lands_the_entry_and_loses_the_answer_leaves_a_row_the_sweep_reclaims() {
    let (h, secrets) = WriteHarness::with_secrets().await;
    let d = declare_secret(&h).await;
    let root = h.base.tree.root;

    // The create reaches the store; the answer does not reach us. The write
    // is refused as unavailable — and the entry exists, unknown to anyone.
    secrets.lose_next_answer();
    let err = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter2")),
            Some("absent"),
        )
        .await
        .expect_err("the answer was lost");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    let held = secrets.held();
    assert_eq!(held.len(), 1, "the entry landed");

    // What makes it not an orphan: the intent was recorded before the create,
    // as a pending row naming the very reference, for the sweep to reclaim.
    let rows = all_pending(&h).await;
    assert_eq!(rows.len(), 1, "one intent row");
    assert_eq!(rows[0].secret_ref, held[0]);
    assert_eq!((rows[0].declaration_id, rows[0].tenant_id), (d, root));

    // A committed write consumes its row inside the transaction; a write
    // refused on its tag releases both the entry and the row.
    h.write(
        &actor(root),
        "api_token",
        None,
        Change::Set(json!("hunter3")),
        Some("absent"),
    )
    .await
    .expect("stored");
    assert_eq!(all_pending(&h).await.len(), 1, "only the lost one remains");
    let before = secrets.held().len();
    let err = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter4")),
            Some("stale"),
        )
        .await
        .expect_err("stale tag");
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "{err:?}"
    );
    assert_eq!(
        secrets.held().len(),
        before,
        "the refused write's entry is released"
    );
    assert_eq!(all_pending(&h).await.len(), 1, "and its row went with it");
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
async fn a_set_refused_on_its_tag_touches_no_entry_and_a_race_releases_the_one_it_made() {
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
    let stores_after_live = secrets.stores.load(Ordering::SeqCst);

    // A stale tag is judged before the plaintext goes anywhere: no entry is
    // created, so none is released, and the live one is untouched.
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
    assert_eq!(secrets.stores.load(Ordering::SeqCst), stores_after_live);
    assert!(secrets.deleted.lock().expect("lock").is_empty());
    assert_eq!(secrets.held(), vec![live_ref.clone()]);

    // The race the commit's own check exists for: the tag was current when the
    // stage judged it and the row moved before the commit took its lock. The
    // entry the stage created is released and the live one is untouched.
    let gated = h
        .gate(&actor(root), "api_token", None)
        .await
        .expect("gated");
    let staged = {
        let conn = h.base.db.conn().expect("connection");
        h.writer
            .stage(
                &conn,
                &gated,
                &actor(root),
                Change::Set(json!("racer")),
                StagePrecondition::Judge(Some(&live.etag)),
            )
            .await
            .expect("staged while the tag was current")
    };
    assert_eq!(secrets.stores.load(Ordering::SeqCst), stores_after_live + 1);
    let moved = h
        .write(
            &actor(root),
            "api_token",
            None,
            Change::Set(json!("hunter3")),
            Some(&live.etag),
        )
        .await
        .expect("another writer lands first");
    let raced = h
        .commit_gated_staged(&actor(root), gated, staged, Some(&live.etag))
        .await;
    assert!(
        matches!(raced, Err(DomainError::PreconditionFailed { .. })),
        "{raced:?}"
    );
    let moved_ref = moved
        .new_value
        .as_ref()
        .and_then(Value::as_str)
        .expect("a reference")
        .to_owned();
    assert_eq!(secrets.held(), vec![moved_ref.clone()]);
    let deleted = secrets.deleted.lock().expect("lock");
    assert!(
        deleted.iter().any(|r| r != &live_ref && r != &moved_ref),
        "{deleted:?}"
    );
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

#[tokio::test]
async fn a_rejection_event_carries_the_wire_message_and_the_diagnostic_stays_in_the_log() {
    // The event may travel further than this process's log does.
    let h = WriteHarness::new().await;
    let internal = DomainError::Internal {
        diagnostic: "postgres at 10.0.0.5:5432 refused the connection".to_owned(),
    };
    let change_set = Uuid::new_v4();
    h.writer
        .after_rejection(
            "k",
            h.base.tree.root,
            &actor(h.base.tree.root),
            &internal,
            change_set,
        )
        .await;
    let events = h.published.events.lock().expect("lock");
    match events.as_slice() {
        [
            ValueEvent::ChangeFailed {
                reason,
                change_set_id,
                ..
            },
        ] => {
            assert_eq!(reason, "internal error");
            assert_eq!(*change_set_id, change_set, "the event names its change set");
        }
        other => panic!("one rejection event expected, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rejection_event_names_the_field_and_code_of_a_validation_failure_and_no_more() {
    // A validation message may name part of what was submitted — an enum
    // member, a reference, a number — and the event goes further than the
    // answer did: to the log, and in R2 to a broker. It carries where and why.
    let h = WriteHarness::new().await;
    let refused = DomainError::Validation {
        field: "value/member".to_owned(),
        code: crate::field::VALUE_NOT_IN_ENUM,
        message: "`jane.doe@example.com` is not a registered member of `staff`".to_owned(),
    };
    h.writer
        .after_rejection(
            "k",
            h.base.tree.root,
            &actor(h.base.tree.root),
            &refused,
            Uuid::new_v4(),
        )
        .await;
    let events = h.published.events.lock().expect("lock");
    match events.as_slice() {
        [ValueEvent::ChangeFailed { reason, .. }] => {
            assert!(!reason.contains("jane.doe"), "{reason}");
            assert!(reason.contains("value/member"), "{reason}");
            assert!(reason.contains(crate::field::VALUE_NOT_IN_ENUM), "{reason}");
        }
        other => panic!("one rejection event expected, got {other:?}"),
    }
}

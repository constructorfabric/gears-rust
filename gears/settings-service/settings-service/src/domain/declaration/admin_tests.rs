// Created: 2026-09-08 by Virtuozzo International GmbH
//! Administrative authoring over the resolution harness: what is composed,
//! what is derived, what is refused, and what needs a fresh authentication.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::{Map, Value, json};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{CreateDeclaration, DeclarationAdmin, FieldClass, classify_field, etag_of};
use crate::audit::AuditOperation;
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::stepup::{StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::domain::writes::WriteActor;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    BOOL, FixedStepUp, RecordingAudit, RecordingRegistrar, ResolutionHarness, SECRET, TEXT,
    resolution_catalogue,
};

type Admin = DeclarationAdmin<DeclarationRepo, CategoryRepo, ValueRepo, Arc<RecordingAudit>>;

struct Harness {
    base: ResolutionHarness,
    admin: Admin,
    audit: Arc<RecordingAudit>,
    registrar: Arc<RecordingRegistrar>,
}

impl Harness {
    async fn new() -> Self {
        Self::with_step_up(Arc::new(FixedStepUp::refusing(
            StepUpRefusal::NotConfigured,
        )))
        .await
    }

    /// A harness whose step-up verifier accepts whatever the caller presents.
    async fn verified() -> Self {
        Self::with_step_up(Arc::new(FixedStepUp::verified())).await
    }

    async fn with_step_up(step_up: Arc<dyn StepUpVerifier>) -> Self {
        let base = ResolutionHarness::new().await;
        let audit = Arc::new(RecordingAudit::default());
        let registrar = Arc::new(RecordingRegistrar::default());
        let admin = DeclarationAdmin::new(
            DeclarationRepo,
            CategoryRepo,
            ValueRepo,
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            Arc::clone(&registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
            step_up,
            Arc::clone(&audit),
            Arc::clone(&base.cache),
        );
        Self {
            base,
            admin,
            audit,
            registrar,
        }
    }

    fn request(&self, name: &str) -> CreateDeclaration {
        CreateDeclaration {
            value_type_id: BOOL.to_owned(),
            vendor: "acme".to_owned(),
            name: name.to_owned(),
            category_id: self.base.category_id(),
            default_value: json!(false),
            scope_class: "cascading".to_owned(),
            description: Some("a demo setting".to_owned()),
            mode: None,
            requires_step_up: None,
            anonymous_exposable: None,
            domain_affinity: None,
            licence_feature: None,
            data_classification: None,
        }
    }

    async fn create(
        &self,
        request: CreateDeclaration,
        actor: &WriteActor,
    ) -> Result<super::Created, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.admin
            .create(&conn, &AccessScope::allow_all(), request, actor)
            .await
    }

    async fn update(
        &self,
        id: Uuid,
        if_match: Option<&str>,
        patch: Value,
        actor: &WriteActor,
    ) -> Result<Declaration, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        let map: Map<String, Value> = patch.as_object().expect("an object").clone();
        self.admin
            .update(&conn, &AccessScope::allow_all(), id, if_match, &map, actor)
            .await
    }

    async fn retire(
        &self,
        id: Uuid,
        if_match: Option<&str>,
        actor: &WriteActor,
    ) -> Result<Declaration, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.admin
            .retire(&conn, &AccessScope::allow_all(), id, if_match, actor)
            .await
    }

    async fn load(&self, id: Uuid) -> Declaration {
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .find(
                &conn,
                &AccessScope::allow_all(),
                &crate::domain::category::visibility::domain_visibility(&AccessScope::allow_all()),
                id,
            )
            .await
            .expect("lookup")
            .expect("row")
    }
}

fn admin_actor() -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::from_u128(0xadd1))
            .subject_tenant_id(Uuid::new_v4())
            .subject_type(USER_SUBJECT_TYPE)
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: Some(SecretString::from("token".to_owned())),
    }
}

fn service_actor() -> WriteActor {
    WriteActor {
        ctx: SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .subject_type("gts.cf.core.security.subject_service.v1~")
            .build()
            .expect("context"),
        request_id: "req".to_owned(),
        step_up_token: None,
    }
}

#[tokio::test]
async fn a_create_composes_the_key_registers_the_type_and_records_it() {
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let d = &created.declaration;
    assert!(!created.reactivated);
    assert_eq!(
        d.key,
        "gts.cf.core.settings.setting_type.v1~acme.settings.network.retry_policy.v1~"
    );
    assert_eq!(d.leaf_slug, "retry_policy");
    assert_eq!(d.source, "admin_authored");
    assert_eq!(d.status, "active");
    assert!(d.owner_module.is_none());
    // Unsupplied gates take their protective defaults.
    assert!(d.requires_step_up);
    assert!(!d.anonymous_exposable);
    assert_eq!(d.mode, "standard");
    assert_eq!(d.data_classification, "public");
    assert!(!d.has_secret_trait);
    // The composed type is registered before the row exists.
    let registered = h.registrar.registered.lock().expect("lock").clone();
    assert_eq!(registered, vec![(d.key.clone(), BOOL.to_owned())]);
    assert_eq!(h.audit.operations(), vec!["create"]);
}

#[tokio::test]
async fn a_bad_segment_a_missing_category_and_a_bad_scope_class_are_all_refused() {
    let h = Harness::new().await;
    let mut bad_name = h.request("Retry Policy");
    bad_name.name = "Retry Policy".to_owned();
    let err = h
        .create(bad_name, &admin_actor())
        .await
        .expect_err("segment");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::SETTING_KEY_SEGMENT),
        "{err:?}"
    );

    let mut missing = h.request("ok_name");
    missing.category_id = Uuid::new_v4();
    let err = h
        .create(missing, &admin_actor())
        .await
        .expect_err("category");
    assert!(
        matches!(&err, DomainError::NotFound { resource } if *resource == "category"),
        "{err:?}"
    );

    let mut bad_class = h.request("ok_name");
    bad_class.scope_class = "everywhere".to_owned();
    let err = h
        .create(bad_class, &admin_actor())
        .await
        .expect_err("scope");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::SCOPE_CLASS_INVALID),
        "{err:?}"
    );

    // Nothing landed, and no type was registered for a refused create.
    assert!(h.registrar.registered.lock().expect("lock").is_empty());
    assert!(h.audit.records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn an_invalid_schema_default_is_refused_with_field_level_detail() {
    let h = Harness::new().await;
    let mut request = h.request("flag");
    request.default_value = json!("not a boolean");
    let err = h
        .create(request, &admin_actor())
        .await
        .expect_err("default");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    assert!(h.registrar.registered.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn the_secret_classification_is_derived_and_its_default_must_be_a_placeholder() {
    let h = Harness::new().await;

    // Derived from the trait, with the author supplying nothing.
    let mut secret = h.request("api_token");
    secret.value_type_id = SECRET.to_owned();
    secret.default_value = json!("");
    let created = h.create(secret, &admin_actor()).await.expect("created");
    assert!(created.declaration.has_secret_trait);
    assert_eq!(created.declaration.data_classification, "secret");

    // A live credential as the default is refused.
    let mut with_default = h.request("other_token");
    with_default.value_type_id = SECRET.to_owned();
    with_default.default_value = json!("hunter2");
    let err = h
        .create(with_default, &admin_actor())
        .await
        .expect_err("default");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::SECRET_DEFAULT_NOT_EMPTY),
        "{err:?}"
    );

    // `secret` on a non-secret type, and `pii` on a secret one, are both refused.
    let mut author_secret = h.request("plain");
    author_secret.data_classification = Some("secret".to_owned());
    let err = h
        .create(author_secret, &admin_actor())
        .await
        .expect_err("author");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::CLASSIFICATION_CONFLICT),
        "{err:?}"
    );

    let mut secret_pii = h.request("token_pii");
    secret_pii.value_type_id = SECRET.to_owned();
    secret_pii.default_value = json!("");
    secret_pii.data_classification = Some("pii".to_owned());
    let err = h
        .create(secret_pii, &admin_actor())
        .await
        .expect_err("conflict");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::CLASSIFICATION_CONFLICT),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_anonymous_surface_refuses_a_sensitive_setting() {
    let h = Harness::new().await;
    let mut request = h.request("contact");
    request.data_classification = Some("pii".to_owned());
    request.anonymous_exposable = Some(true);
    let err = h
        .create(request, &admin_actor())
        .await
        .expect_err("exposed");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::EXPOSABLE_NOT_SENSITIVE),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_second_create_at_an_active_key_is_a_conflict_not_a_second_row() {
    let h = Harness::new().await;
    h.create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let err = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect_err("conflict");
    match err {
        DomainError::Conflict { detail } => {
            assert!(
                detail.starts_with(super::conflict::KEY_CONFLICT),
                "{detail}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(h.audit.operations(), vec!["create"]);
}

#[tokio::test]
async fn a_patch_applies_descriptive_metadata_and_refuses_the_behaviour_affecting_fields() {
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);

    let updated = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"description": "a better description", "mode": "advanced"}),
            &admin_actor(),
        )
        .await
        .expect("updated");
    assert_eq!(updated.description.as_deref(), Some("a better description"));
    assert_eq!(updated.mode, "advanced");
    assert_eq!(h.audit.operations(), vec!["create", "change"]);

    // Behaviour-affecting, and unknown, both refused before anything is written.
    for field in ["default_value", "scope_class", "value_type_id", "wobble"] {
        let tag = etag_of(&h.load(id).await);
        let err = h
            .update(
                id,
                Some(tag.as_str()),
                json!({ field: json!("anything") }),
                &admin_actor(),
            )
            .await
            .expect_err("immutable");
        assert!(
            matches!(&err, DomainError::Validation { code, field: f, .. }
                if *code == crate::field::DECLARATION_FIELD_IMMUTABLE && f == field),
            "{field}: {err:?}"
        );
    }
    assert_eq!(h.load(id).await.default_value, json!(false));
}

#[tokio::test]
async fn the_row_write_itself_is_conditional_on_the_version_the_tag_was_compared_against() {
    use crate::domain::declaration::DeclarationMetadata;
    let h = Harness::verified().await;
    let current = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created")
        .declaration;
    let stale = current.updated_at - time::Duration::seconds(1);
    let conn = h.base.db.conn().expect("connection");
    let all = AccessScope::allow_all();
    let metadata = || DeclarationMetadata {
        mode: current.mode.clone(),
        description: Some("moved".to_owned()),
        domain_affinity: None,
        licence_feature: None,
        data_classification: current.data_classification.clone(),
        requires_step_up: current.requires_step_up,
        anonymous_exposable: current.anonymous_exposable,
    };

    // The comparison ran against a read; a row that moved since finds no
    // match at the write, and the writer gets the same `412` a stale tag gets.
    let refused = DeclarationRepo
        .update_metadata(&conn, &all, current.id, metadata(), Some(stale), true)
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let refused = DeclarationRepo
        .set_status(&conn, &all, current.id, "retired", Some(stale))
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let kept = h.load(current.id).await;
    assert_eq!(kept.status, "active");
    assert_eq!(kept.description.as_deref(), Some("a demo setting"));

    // At the version read, the write lands; without a version — a caller
    // under its own transaction, like a revive — it is unconditional.
    DeclarationRepo
        .set_status(&conn, &all, current.id, "retired", Some(current.updated_at))
        .await
        .expect("current version");
    assert_eq!(h.load(current.id).await.status, "retired");
    DeclarationRepo
        .update_metadata(&conn, &all, current.id, metadata(), None, true)
        .await
        .expect("unconditional");
    assert_eq!(
        h.load(current.id).await.description.as_deref(),
        Some("moved")
    );
}

#[tokio::test]
async fn a_patch_needs_the_tag_and_refuses_a_stale_one() {
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let stale = etag_of(&created.declaration);

    let err = h
        .update(id, None, json!({"description": "x"}), &admin_actor())
        .await
        .expect_err("no tag");
    assert!(
        matches!(err, DomainError::PreconditionRequired { .. }),
        "{err:?}"
    );

    h.update(
        id,
        Some(stale.as_str()),
        json!({"description": "first"}),
        &admin_actor(),
    )
    .await
    .expect("first edit");

    let err = h
        .update(
            id,
            Some(stale.as_str()),
            json!({"description": "second"}),
            &admin_actor(),
        )
        .await
        .expect_err("stale");
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "{err:?}"
    );
    assert_eq!(h.load(id).await.description.as_deref(), Some("first"));
}

#[tokio::test]
async fn tightening_is_immediate_and_loosening_needs_step_up() {
    // Tightening: no step-up verifier configured, and it still goes through.
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;

    let tag = etag_of(&created.declaration);
    let tightened = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"data_classification": "pii", "requires_step_up": true}),
            &admin_actor(),
        )
        .await
        .expect("tightened");
    assert_eq!(tightened.data_classification, "pii");

    // Loosening, with nothing able to verify: refused, and the flag stands.
    for patch in [
        json!({"data_classification": "public"}),
        json!({"requires_step_up": false}),
        json!({"anonymous_exposable": true}),
    ] {
        let tag = etag_of(&h.load(id).await);
        let err = h
            .update(id, Some(tag.as_str()), patch.clone(), &admin_actor())
            .await
            .expect_err("step-up");
        assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
    }
    let current = h.load(id).await;
    assert_eq!(current.data_classification, "pii");
    assert!(current.requires_step_up);
    assert!(!current.anonymous_exposable);

    // With a verifier that accepts, the same edit lands.
    let verified = Harness::verified().await;
    let created = verified
        .create(verified.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    let loosened = verified
        .update(
            created.declaration.id,
            Some(tag.as_str()),
            json!({"requires_step_up": false}),
            &admin_actor(),
        )
        .await
        .expect("loosened");
    assert!(!loosened.requires_step_up);
}

#[tokio::test]
async fn a_classification_change_resyncs_the_stored_values() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;

    let tag = etag_of(&created.declaration);
    h.update(
        id,
        Some(tag.as_str()),
        json!({"data_classification": "pii"}),
        &admin_actor(),
    )
    .await
    .expect("tightened");

    let conn = h.base.db.conn().expect("connection");
    let row = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.data_classification, "pii");
}

#[tokio::test]
async fn a_contributed_declaration_is_not_admin_editable_or_retirable() {
    let h = Harness::verified().await;
    // The harness declares as a module would.
    let id = h
        .base
        .declare(
            "contributed",
            crate::domain::resolution::scope_class::CASCADING,
            json!(true),
        )
        .await;
    let tag = etag_of(&h.load(id).await);

    let err = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"description": "mine now"}),
            &admin_actor(),
        )
        .await
        .expect_err("contributed");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::CONTRIBUTED_IMMUTABLE),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }

    let err = h
        .retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("contributed");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::CONTRIBUTED_IMMUTABLE),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.load(id).await.status, "active");
}

#[tokio::test]
async fn retire_needs_step_up_keeps_every_value_and_answers_with_the_retired_row() {
    // Without a verifier: refused, and the declaration stays live.
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);
    let err = h
        .retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("step-up");
    assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
    assert_eq!(h.load(id).await.status, "active");

    // With one: retired, values retained.
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;

    let err = h
        .retire(id, None, &admin_actor())
        .await
        .expect_err("no tag");
    assert!(
        matches!(err, DomainError::PreconditionRequired { .. }),
        "{err:?}"
    );

    let tag = etag_of(&h.load(id).await);
    let retired = h
        .retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    assert_eq!(retired.status, "retired");
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), id, tenant)
            .await
            .expect("lookup")
            .is_some(),
        "retire never deletes values"
    );
    assert_eq!(h.audit.operations(), vec!["create", "remove"]);
    let records = h.audit.records.lock().expect("lock");
    let last = records.last().expect("a record");
    assert_eq!(last.operation, AuditOperation::Remove);
    assert!(last.pre_image.is_some() && last.post_image.is_some());
}

#[tokio::test]
async fn re_declaring_a_retired_key_revives_it_with_its_values() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    let revived = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("revived");
    assert!(revived.reactivated);
    assert_eq!(revived.declaration.id, id, "the row keeps its identity");
    assert_eq!(revived.declaration.status, "active");
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_one(&conn, &AccessScope::allow_all(), id, tenant)
            .await
            .expect("lookup")
            .is_some()
    );
    // The revive confirms the setting's own type under the value type it goes
    // live with, before anything is written; the registry answers an
    // identical second registration as the one it already holds.
    let key = revived.declaration.key.clone();
    assert_eq!(
        *h.registrar.registered.lock().expect("lock"),
        vec![(key.clone(), BOOL.to_owned()), (key, BOOL.to_owned())]
    );
}

#[tokio::test]
async fn a_revive_may_not_flip_the_secret_boundary_or_the_scope_class() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    // A secret type holds its values by reference; the inline `true` would be
    // plaintext left under a secret setting, so the boundary is refused.
    let mut secreted = h.request("retry_policy");
    secreted.value_type_id = SECRET.to_owned();
    secreted.default_value = json!("");
    let err = h
        .create(secreted, &admin_actor())
        .await
        .expect_err("secretness");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::SECRETNESS_CHANGED),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }

    let mut rescoped = h.request("retry_policy");
    rescoped.scope_class = "global".to_owned();
    let err = h
        .create(rescoped, &admin_actor())
        .await
        .expect_err("rescope");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::SCOPE_CLASS_CHANGED),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }

    // Nothing was reactivated, and the retained value is as it was.
    assert_eq!(h.load(id).await.status, "retired");
    let conn = h.base.db.conn().expect("connection");
    let row = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.value, Some(json!(true)));
    assert!(!row.needs_review);
}

#[tokio::test]
async fn a_revive_may_not_retype_the_setting_because_its_registered_type_cannot_follow() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    // The setting's own GTS type is registered with its payload narrowed to
    // the boolean, and the registry does not replace a registered type: a
    // revive as text would leave the two disagreeing, so it is refused.
    let mut retyped = h.request("retry_policy");
    retyped.value_type_id = TEXT.to_owned();
    retyped.default_value = json!("gentle");
    let err = h.create(retyped, &admin_actor()).await.expect_err("retype");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::VALUE_TYPE_CHANGED),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }

    // Nothing was reactivated, retyped or re-registered, and the retained
    // value is as it was.
    let row = h.load(id).await;
    assert_eq!(row.status, "retired");
    assert_eq!(row.value_type_id, BOOL);
    assert_eq!(row.default_value, json!(false));
    assert_eq!(h.registrar.registered.lock().expect("lock").len(), 1);
    let conn = h.base.db.conn().expect("connection");
    let value = ValueRepo
        .find_one(&conn, &AccessScope::allow_all(), id, tenant)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(value.value, Some(json!(true)));
    assert!(!value.needs_review);
}

#[tokio::test]
async fn a_revive_re_validates_every_retained_value_against_its_type() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let (a, b) = (h.base.tree.a, h.base.tree.b);
    // A text that never read as a boolean, and a boolean flagged while the
    // setting sat retired: the revive re-validates both, keeping the first
    // flagged and clearing the second.
    h.base.set_flagged(id, a, json!("aggressive")).await;
    h.base.set_flagged(id, b, json!(true)).await;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    let revived = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("revived");
    assert!(revived.reactivated);
    assert_eq!(revived.declaration.value_type_id, BOOL);

    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let flagged = ValueRepo
        .find_one(&conn, &scope, id, a)
        .await
        .expect("lookup")
        .expect("row");
    assert!(flagged.needs_review, "text is not a boolean");
    assert!(flagged.needs_review_detail.is_some(), "the flag says why");
    assert_eq!(
        flagged.value,
        Some(json!("aggressive")),
        "flagged, not discarded"
    );
    let cleared = ValueRepo
        .find_one(&conn, &scope, id, b)
        .await
        .expect("lookup")
        .expect("row");
    assert!(!cleared.needs_review, "a boolean validates again");
    assert_eq!(cleared.needs_review_detail, None);
}

#[tokio::test]
async fn a_revive_needs_step_up_and_a_service_principal_never_gets_it() {
    // A revive with no verifier configured is refused.
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    h.retire(created.declaration.id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    // A service principal cannot re-authenticate at all: refused as a denial,
    // not as a challenge it could never answer.
    let err = h
        .create(h.request("retry_policy"), &service_actor())
        .await
        .expect_err("machine");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");

    // And a verifier that refuses yields the challenge.
    let refusing =
        Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Missing))).await;
    let created = refusing
        .create(refusing.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    let err = refusing
        .retire(created.declaration.id, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("refused");
    assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
}

#[tokio::test]
async fn a_person_labelled_user_reaches_the_step_up_gate_of_a_declaration_action() {
    // The declaration path has its own step-up gate; it must read the label
    // the same way the value path does.
    let refusing =
        Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Missing))).await;
    let created = refusing
        .create(refusing.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    let labelled = |subject_type: Option<&str>| {
        let mut ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4());
        if let Some(label) = subject_type {
            ctx = ctx.subject_type(label);
        }
        WriteActor {
            ctx: ctx.build().expect("context"),
            request_id: "req".to_owned(),
            step_up_token: Some(SecretString::from("token".to_owned())),
        }
    };
    let err = refusing
        .retire(
            created.declaration.id,
            Some(tag.as_str()),
            &labelled(Some("user")),
        )
        .await
        .expect_err("challenged, not denied");
    assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
    let err = refusing
        .retire(created.declaration.id, Some(tag.as_str()), &labelled(None))
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
}

#[tokio::test]
async fn a_metadata_update_evicts_the_key_so_a_reclassification_masks_on_the_next_read() {
    use crate::domain::resolution::ScopeTarget;
    use settings_service_sdk::SettingKey;
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let key = SettingKey::parse(&created.declaration.key).expect("key");
    let tenant = h.base.tree.a;
    h.base.set(id, tenant, json!(true)).await;

    // A read warms the cache under the declaration's current class.
    {
        let conn = h.base.db.conn().expect("connection");
        let warm = h
            .base
            .resolver
            .resolve(&conn, &key, ScopeTarget::Tenant(tenant))
            .await
            .expect("resolves");
        assert_eq!(warm.data_classification, "public");
    }

    // The administrator tightens the classification; the next read must
    // mask by the new class at once, not when the TTL runs out.
    let tag = etag_of(&created.declaration);
    h.update(
        id,
        Some(tag.as_str()),
        json!({"data_classification": "pii"}),
        &admin_actor(),
    )
    .await
    .expect("tightened");
    let conn = h.base.db.conn().expect("connection");
    let fresh = h
        .base
        .resolver
        .resolve(&conn, &key, ScopeTarget::Tenant(tenant))
        .await
        .expect("resolves");
    assert_eq!(fresh.data_classification, "pii", "the cache was evicted");
}

#[tokio::test]
async fn retiring_evicts_the_key_so_a_cached_read_cannot_keep_serving_it() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let key = created.declaration.key.clone();
    h.base
        .cache
        .seed(Arc::new(crate::domain::resolution::cache::tests_entry(
            &key,
            h.base.tree.a,
        )));
    assert!(!h.base.cache.is_empty());
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    assert!(h.base.cache.is_empty(), "the key is evicted on retire");
}

#[test]
fn every_field_falls_into_exactly_one_class() {
    let declaration = Declaration {
        id: Uuid::new_v4(),
        key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.x.v1~".to_owned(),
        leaf_slug: "x".to_owned(),
        value_type_id: BOOL.to_owned(),
        category_id: Uuid::new_v4(),
        scope_class: "cascading".to_owned(),
        mode: "standard".to_owned(),
        status: "active".to_owned(),
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        description: None,
        default_value: json!(false),
        has_secret_trait: false,
        data_classification: "pii".to_owned(),
        requires_step_up: true,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    };
    let cases = [
        ("description", json!("x"), FieldClass::Immediate),
        ("mode", json!("advanced"), FieldClass::Immediate),
        ("domain_affinity", json!(null), FieldClass::Immediate),
        ("requires_step_up", json!(true), FieldClass::Immediate),
        ("requires_step_up", json!(false), FieldClass::StepUp),
        ("anonymous_exposable", json!(false), FieldClass::Immediate),
        ("anonymous_exposable", json!(true), FieldClass::StepUp),
        ("data_classification", json!("public"), FieldClass::StepUp),
        (
            "data_classification",
            json!("secret"),
            FieldClass::Immutable,
        ),
        ("default_value", json!(true), FieldClass::Immutable),
        ("scope_class", json!("global"), FieldClass::Immutable),
        ("owner_module", json!("someone"), FieldClass::Immutable),
        ("unheard_of", json!(1), FieldClass::Immutable),
    ];
    for (name, value, expected) in cases {
        assert_eq!(
            classify_field(name, &value, &declaration).expect("classified"),
            expected,
            "{name} = {value}"
        );
    }

    // A tightening classification change on a `public` setting is immediate.
    let public = Declaration {
        data_classification: "public".to_owned(),
        ..declaration.clone()
    };
    assert_eq!(
        classify_field("data_classification", &json!("pii"), &public).expect("classified"),
        FieldClass::Immediate
    );
    // On a secret setting the class is derived and never author-changed.
    let secret = Declaration {
        has_secret_trait: true,
        data_classification: "secret".to_owned(),
        ..declaration
    };
    assert_eq!(
        classify_field("data_classification", &json!("public"), &secret).expect("classified"),
        FieldClass::Immutable
    );
}

#[tokio::test]
async fn an_authn_resolver_outage_on_a_declaration_gate_is_unavailable_not_a_challenge() {
    // The declaration path has its own step-up gate; it must tell an outage
    // from a refused token the same way the value path does.
    let down = Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Unavailable(
        "authn resolver down".to_owned(),
    ))))
    .await;
    let created = down
        .create(down.request("retry_policy"), &admin_actor())
        .await
        .expect("a new declaration asks no step-up");
    let tag = etag_of(&created.declaration);
    let err = down
        .retire(created.declaration.id, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("refused");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
}

#[tokio::test]
async fn a_descriptive_patch_leaves_the_definition_recency_alone_and_a_reclassification_moves_it() {
    // `last_change_at` is the definition arm of the recency a reader sees. A
    // description is not what a reader is served, so it moves only the tag;
    // a classification is, so it moves both.
    let h = Harness::new().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let before = h.load(id).await;

    let renamed = h
        .update(
            id,
            Some(etag_of(&before).as_str()),
            json!({ "description": "a better description", "mode": "advanced" }),
            &admin_actor(),
        )
        .await
        .expect("updated");
    assert_eq!(
        renamed.last_change_at, before.last_change_at,
        "a description or a mode is not the definition"
    );
    assert!(renamed.updated_at > before.updated_at, "the tag moved");

    let reclassified = h
        .update(
            id,
            Some(etag_of(&renamed).as_str()),
            json!({ "data_classification": "pii" }),
            &admin_actor(),
        )
        .await
        .expect("reclassified");
    assert!(
        reclassified.last_change_at > renamed.last_change_at,
        "what a reader is served changed"
    );
    assert!(reclassified.updated_at > renamed.updated_at);
}

#[tokio::test]
async fn a_secret_placeholder_must_be_an_instance_of_the_type_as_well_as_empty() {
    // DESIGN §4.2: the placeholder is an empty value *of the declared type* —
    // `""` for a string-shaped secret, `null` only for a type admitting it. An
    // empty array or `null` for a string type is empty, but not the type.
    let h = Harness::new().await;
    for (name, default) in [
        ("as_array", json!([])),
        ("as_null", json!(null)),
        ("as_object", json!({})),
    ] {
        let mut request = h.request(name);
        request.value_type_id = SECRET.to_owned();
        request.default_value = default.clone();
        let err = h
            .create(request, &admin_actor())
            .await
            .expect_err("not an instance of a string type");
        assert!(
            matches!(&err, DomainError::Validation { field, .. } if field != "value_type_id"),
            "{default}: {err:?}"
        );
    }
    let mut request = h.request("as_empty_string");
    request.value_type_id = SECRET.to_owned();
    request.default_value = json!("");
    h.create(request, &admin_actor())
        .await
        .expect("the empty string is both empty and a string");
}

// ── Evolve by re-declaring ───────────────────────────────────────────────────

impl Harness {
    /// The declaration stored under `key`, whatever its status.
    async fn by_key(&self, key: &str) -> Option<Declaration> {
        let conn = self.base.db.conn().expect("connection");
        DeclarationRepo
            .find_by_key(&conn, &AccessScope::allow_all(), key)
            .await
            .expect("lookup")
    }
}

#[tokio::test]
async fn a_behaviour_change_on_an_active_path_evolves_it_to_the_next_major() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let v1 = created.declaration;
    assert!(v1.key.ends_with(".retry_policy.v1~"), "{}", v1.key);
    let (a, b) = (h.base.tree.a, h.base.tree.b);
    h.base.set(v1.id, a, json!(true)).await;
    h.base.set_flagged(v1.id, b, json!("aggressive")).await;

    // Re-declared as text: a new major carries the new shape.
    let mut retyped = h.request("retry_policy");
    retyped.value_type_id = TEXT.to_owned();
    retyped.default_value = json!("gentle");
    let evolved = h.create(retyped, &admin_actor()).await.expect("evolved");
    assert!(evolved.evolved, "the answer says it evolved");
    assert!(!evolved.reactivated);
    let v2 = evolved.declaration;
    assert!(v2.key.ends_with(".retry_policy.v2~"), "{}", v2.key);
    assert_ne!(v2.id, v1.id, "a new declaration, not the old row retyped");
    assert_eq!(v2.value_type_id, TEXT);
    assert_eq!(v2.status, "active");
    assert_eq!(
        h.load(v1.id).await.status,
        "retired",
        "exactly one major is active"
    );
    assert_eq!(evolved.retired.as_deref(), Some(v1.key.as_str()));

    // Every value moved to the new key and was re-validated there; the old
    // key keeps its own.
    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let moved = ValueRepo
        .find_one(&conn, &scope, v2.id, a)
        .await
        .expect("lookup")
        .expect("copied");
    assert_eq!(moved.value, Some(json!(true)), "copied, not coerced");
    assert!(moved.needs_review, "`true` is not text");
    let cleared = ValueRepo
        .find_one(&conn, &scope, v2.id, b)
        .await
        .expect("lookup")
        .expect("copied");
    assert!(!cleared.needs_review, "text validates under the new type");
    assert_eq!(
        ValueRepo
            .find_all(&conn, &scope, v1.id)
            .await
            .expect("lookup")
            .len(),
        2
    );

    // The new key is a new type, registered before it was written.
    assert_eq!(
        h.registrar.registered.lock().expect("lock").last(),
        Some(&(v2.key.clone(), TEXT.to_owned()))
    );
}

#[tokio::test]
async fn evolution_follows_the_active_major_and_a_repeat_of_it_mints_nothing() {
    let h = Harness::verified().await;
    let v1 = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created")
        .declaration;
    let mut as_text = h.request("retry_policy");
    as_text.value_type_id = TEXT.to_owned();
    as_text.default_value = json!("gentle");
    let v2 = h
        .create(as_text.clone(), &admin_actor())
        .await
        .expect("to v2")
        .declaration;

    // The response was lost and the same request comes again: it matches the
    // active v2, so it is a conflict, never an accidental v3.
    let err = h
        .create(as_text.clone(), &admin_actor())
        .await
        .expect_err("a repeat");
    match err {
        DomainError::Conflict { detail } => {
            assert!(
                detail.starts_with(super::conflict::KEY_CONFLICT),
                "{detail}"
            );
        }
        other => panic!("{other:?}"),
    }

    // A later shape change evolves the active v2, not the retired v1.
    let mut firmer = as_text;
    firmer.default_value = json!("firm");
    let v3 = h
        .create(firmer, &admin_actor())
        .await
        .expect("to v3")
        .declaration;
    assert!(v3.key.ends_with(".retry_policy.v3~"), "{}", v3.key);
    assert_eq!(h.load(v2.id).await.status, "retired");
    assert_eq!(h.load(v1.id).await.status, "retired", "v1 stays retired");
    assert!(h.by_key(&v3.key.replace(".v3~", ".v4~")).await.is_none());
}

#[tokio::test]
async fn a_metadata_only_redeclaration_of_an_active_path_is_a_conflict() {
    let h = Harness::verified().await;
    h.create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let mut described = h.request("retry_policy");
    described.description = Some("another text".to_owned());
    let err = h
        .create(described, &admin_actor())
        .await
        .expect_err("PATCH it");
    match err {
        DomainError::Conflict { detail } => {
            assert!(
                detail.starts_with(super::conflict::KEY_CONFLICT),
                "{detail}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn evolution_needs_step_up_and_may_not_cross_the_secret_boundary() {
    // With no verifier configured, an evolve is refused and nothing moves.
    let h = Harness::new().await;
    let v1 = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("a fresh create needs no step-up")
        .declaration;
    let mut as_text = h.request("retry_policy");
    as_text.value_type_id = TEXT.to_owned();
    as_text.default_value = json!("gentle");
    let err = h
        .create(as_text, &admin_actor())
        .await
        .expect_err("no step-up");
    assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
    assert_eq!(h.load(v1.id).await.status, "active");

    // Values stored inline are not re-interpreted as secret references.
    let h = Harness::verified().await;
    let v1 = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created")
        .declaration;
    h.base.set(v1.id, h.base.tree.a, json!(true)).await;
    let mut secreted = h.request("retry_policy");
    secreted.value_type_id = SECRET.to_owned();
    secreted.default_value = json!("");
    let err = h
        .create(secreted, &admin_actor())
        .await
        .expect_err("secretness");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::SECRETNESS_CHANGED),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.load(v1.id).await.status, "active", "nothing moved");
    assert!(h.by_key(&v1.key.replace(".v1~", ".v2~")).await.is_none());
}

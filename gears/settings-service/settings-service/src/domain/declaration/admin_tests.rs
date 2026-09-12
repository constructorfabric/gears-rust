// Created: 2026-09-08 by Constructor Tech
//! Administrative authoring over the resolution harness: what is composed,
//! what is derived, what is refused, and what needs a fresh authentication.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{CreateDeclaration, DeclarationAdmin, FieldClass, classify_field, etag_of};
use crate::audit::AuditOperation;
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::stepup::{NoStepUpVerifier, StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::domain::writes::WriteActor;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    BOOL, FixedStepUp, RecordingAudit, RecordingRegistrar, ResolutionHarness, SECRET,
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
        Self::with_step_up(Arc::new(NoStepUpVerifier::default())).await
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
        step_up_token: Some("token".to_owned()),
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
    // No second type registration: the key is the one already registered.
    assert_eq!(h.registrar.registered.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn a_revive_may_not_change_the_value_type_the_scope_class_or_the_default() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    h.retire(created.declaration.id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    let mut retyped = h.request("retry_policy");
    retyped.value_type_id = SECRET.to_owned();
    retyped.default_value = json!("");
    let err = h.create(retyped, &admin_actor()).await.expect_err("retype");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::VALUE_TYPE_CHANGED),
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

    let mut refloored = h.request("retry_policy");
    refloored.default_value = json!(true);
    let err = h
        .create(refloored, &admin_actor())
        .await
        .expect_err("refloor");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::DEFAULT_CHANGED),
            "{detail}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.load(created.declaration.id).await.status, "retired");
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
        .populate(Arc::new(crate::domain::resolution::cache::tests_entry(
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

//! Administrative authoring over the resolution harness: what is composed,
//! what is derived, what is refused, and what needs a fresh authentication.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use secrecy::SecretString;
use serde_json::{Map, Value, json};
use settings_service_sdk::SettingKey;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{CreateDeclaration, DeclarationAdmin, FieldClass, classify_field, etag_of};
use crate::audit::AuditOperation;
use crate::domain::declaration::{
    Declaration, DeclarationDraft, DeclarationRepository, DeclarationStatus,
};
use crate::domain::error::DomainError;
use crate::domain::ports::SecretManager as _;
use crate::domain::ports::StepUpOperation;
use crate::domain::stepup::{StepUpRefusal, StepUpVerifier, USER_SUBJECT_TYPE};
use crate::domain::value::ValueRepository;
use crate::domain::writes::WriteActor;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    BOOL, FixedStepUp, RecordingAudit, RecordingMetrics, RecordingRegistrar, RecordingSecrets,
    ResolutionHarness, SECRET, TEXT, resolution_catalogue,
};

type Admin = DeclarationAdmin<
    DeclarationRepo,
    CategoryRepo,
    ValueRepo,
    Arc<RecordingAudit>,
    PendingSecretRepo,
>;

struct Harness {
    base: ResolutionHarness,
    admin: Admin,
    audit: Arc<RecordingAudit>,
    registrar: Arc<RecordingRegistrar>,
    secrets: Arc<RecordingSecrets>,
    metrics: Arc<RecordingMetrics>,
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
        let secrets = Arc::new(RecordingSecrets::default());
        let metrics = Arc::new(RecordingMetrics::default());
        let admin = DeclarationAdmin::new(
            DeclarationRepo,
            CategoryRepo,
            ValueRepo,
            Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            Arc::clone(&registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
            step_up,
            Arc::clone(&audit),
            Arc::clone(&base.cache),
            PendingSecretRepo,
            Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
            Arc::clone(&metrics) as Arc<dyn crate::domain::ports::WriteMetrics>,
        );
        Self {
            base,
            admin,
            audit,
            registrar,
            secrets,
            metrics,
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
            display_name: None,
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

    async fn purge(
        &self,
        id: Uuid,
        if_match: Option<&str>,
        actor: &WriteActor,
    ) -> Result<super::PurgedDeclaration, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.admin
            .purge(&conn, &AccessScope::allow_all(), id, if_match, actor)
            .await
    }

    /// An admin-authored declaration, retired; answers its id and current tag.
    async fn retired(&self, name: &str) -> (Uuid, String) {
        let created = self
            .create(self.request(name), &admin_actor())
            .await
            .expect("created");
        let id = created.declaration.id;
        let tag = etag_of(&created.declaration);
        self.retire(id, Some(tag.as_str()), &admin_actor())
            .await
            .expect("retired");
        let tag = etag_of(&self.load(id).await);
        (id, tag.as_str().to_owned())
    }

    /// How many declarations the setting has had, whatever their status.
    async fn versions_of(&self, key: &str) -> usize {
        let conn = self.base.db.conn().expect("connection");
        let prefix = key.strip_suffix("1~").expect("a v1 key");
        DeclarationRepo
            .find_by_key_prefix(&conn, &AccessScope::allow_all(), prefix)
            .await
            .expect("lookup")
            .len()
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
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
        visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
    assert_eq!(d.status, DeclarationStatus::Active);
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
        display_name: None,
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
        .set_status(
            &conn,
            &all,
            current.id,
            DeclarationStatus::Retired,
            Some(stale),
        )
        .await
        .expect_err("moved");
    assert!(
        matches!(refused, DomainError::PreconditionFailed { .. }),
        "{refused:?}"
    );
    let kept = h.load(current.id).await;
    assert_eq!(kept.status, DeclarationStatus::Active);
    assert_eq!(kept.description.as_deref(), Some("a demo setting"));

    // At the version read, the write lands; without a version — a caller
    // under its own transaction, like a revive — it is unconditional.
    DeclarationRepo
        .set_status(
            &conn,
            &all,
            current.id,
            DeclarationStatus::Retired,
            Some(current.updated_at),
        )
        .await
        .expect("current version");
    assert_eq!(h.load(current.id).await.status, DeclarationStatus::Retired);
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
    assert_eq!(h.load(id).await.status, DeclarationStatus::Active);
}

/// A module's declaration at the key an administrative create composes.
/// Nothing reserves the `settings` package an admin key is composed with, so
/// a module may contribute exactly `acme.settings.network.<name>.v1~`.
async fn contributed_at_the_admin_key(h: &Harness, name: &str) -> Uuid {
    let key =
        SettingKey::contributed("acme", "settings", "network", name, NonZeroU32::MIN).expect("key");
    assert_eq!(
        key.as_str(),
        SettingKey::compose("acme", "network", name)
            .expect("admin key")
            .as_str(),
        "the module's key is the one an administrator composes"
    );
    let conn = h.base.db.conn().expect("connection");
    DeclarationRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            DeclarationDraft {
                key: key.to_string(),
                leaf_slug: name.to_owned(),
                value_type_id: BOOL.to_owned(),
                category_id: h.base.category_id(),
                default_value: json!(true),
                scope_class: "cascading".to_owned(),
                mode: "standard".to_owned(),
                requires_step_up: true,
                anonymous_exposable: false,
                domain_affinity: None,
                has_secret_trait: false,
                data_classification: "public".to_owned(),
                source: "module_contributed".to_owned(),
                owner_module: Some("module-x".to_owned()),
                licence_feature: None,
                display_name: None,
                description: None,
                created_by: "module-x".to_owned(),
            },
        )
        .await
        .expect("contributed")
        .id
}

/// Requests that differ from the contributed row in whatever an
/// administrator might send: the same shape, a retype, a value type the
/// registry does not know, a classification the type contradicts.
fn requests_at_a_contributed_key(
    h: &Harness,
    name: &str,
) -> Vec<(&'static str, CreateDeclaration)> {
    let same = h.request(name);
    let mut retype = h.request(name);
    retype.value_type_id = TEXT.to_owned();
    retype.default_value = json!("");
    let mut unknown = h.request(name);
    unknown.value_type_id = "gts.cf.core.settings.type_nowhere.v1~".to_owned();
    let mut contradicted = h.request(name);
    contradicted.data_classification = Some("secret".to_owned());
    vec![
        ("the same shape", same),
        ("a retype", retype),
        ("an unknown value type", unknown),
        ("a contradicted classification", contradicted),
    ]
}

fn assert_contributed_immutable(err: DomainError, what: &str) {
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::CONTRIBUTED_IMMUTABLE),
            "{what}: {detail}"
        ),
        other => panic!("{what}: the owner's refusal first, got {other:?}"),
    }
}

#[tokio::test]
async fn a_retired_contributed_declaration_is_not_revived_by_an_administrative_create() {
    // Who may change the row at all is judged before anything about the
    // request is resolved: whatever the administrator sends, the answer is
    // the owner's — not a retype, an unknown type or a classification
    // conflict standing in front of it.
    let h = Harness::verified().await;
    let id = contributed_at_the_admin_key(&h, "proxy").await;
    h.base.retire(id).await;

    for (what, request) in requests_at_a_contributed_key(&h, "proxy") {
        let err = h.create(request, &admin_actor()).await.expect_err(what);
        assert_contributed_immutable(err, what);
    }
    let row = h.load(id).await;
    assert_eq!(row.status, DeclarationStatus::Retired);
    assert_eq!(row.default_value, json!(true));
    assert_eq!(row.owner_module.as_deref(), Some("module-x"));
    assert!(h.audit.records().is_empty(), "nothing audited");
}

#[tokio::test]
async fn an_active_contributed_declaration_is_not_evolved_by_an_administrative_create() {
    // The same answer on the evolution path, and ahead of step-up: a change
    // that cannot go through is refused, not challenged. The harness has no
    // step-up verifier, so a challenge would be the answer if it came first.
    let h = Harness::new().await;
    let id = contributed_at_the_admin_key(&h, "proxy").await;

    for (what, request) in requests_at_a_contributed_key(&h, "proxy") {
        let err = h.create(request, &admin_actor()).await.expect_err(what);
        assert_contributed_immutable(err, what);
    }
    let row = h.load(id).await;
    assert_eq!(row.status, DeclarationStatus::Active);
    assert_eq!(row.default_value, json!(true));
    assert!(
        h.by_key(&row.key.replace(".v1~", ".v2~")).await.is_none(),
        "no major minted"
    );
    assert!(h.audit.records().is_empty(), "nothing audited");
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
    assert_eq!(h.load(id).await.status, DeclarationStatus::Active);

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
    assert_eq!(retired.status, DeclarationStatus::Retired);
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
    assert_eq!(revived.declaration.status, DeclarationStatus::Active);
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
    assert_eq!(h.load(id).await.status, DeclarationStatus::Retired);
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
    assert_eq!(row.status, DeclarationStatus::Retired);
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
            visibility: crate::domain::category::DomainVisibility::Unrestricted,
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
        status: DeclarationStatus::Active,
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        display_name: None,
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
    assert_eq!(v2.status, DeclarationStatus::Active);
    assert_eq!(
        h.load(v1.id).await.status,
        DeclarationStatus::Retired,
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
    assert_eq!(h.load(v2.id).await.status, DeclarationStatus::Retired);
    assert_eq!(
        h.load(v1.id).await.status,
        DeclarationStatus::Retired,
        "v1 stays retired"
    );
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
    assert_eq!(h.load(v1.id).await.status, DeclarationStatus::Active);

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
    assert_eq!(
        h.load(v1.id).await.status,
        DeclarationStatus::Active,
        "nothing moved"
    );
    assert!(h.by_key(&v1.key.replace(".v1~", ".v2~")).await.is_none());
}

#[tokio::test]
async fn a_revive_that_cannot_happen_says_why_before_the_default_is_judged() {
    // A retype of a retired setting is refused whatever its default: asking
    // the administrator to fix a default for a revive that cannot happen would
    // be the wrong refusal first.
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
    retyped.value_type_id = TEXT.to_owned();
    retyped.default_value = json!(42);
    let err = h
        .create(retyped, &admin_actor())
        .await
        .expect_err("refused");
    match err {
        DomainError::Conflict { detail } => assert!(
            detail.starts_with(super::conflict::VALUE_TYPE_CHANGED),
            "{detail}"
        ),
        other => panic!("the revive's own refusal first, got {other:?}"),
    }
}

// ── The display name ─────────────────────────────────────────────────────────

/// The declaration `classify_field` is asked about, without a label.
fn classified_declaration() -> Declaration {
    Declaration {
        id: Uuid::new_v4(),
        key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.x.v1~".to_owned(),
        leaf_slug: "x".to_owned(),
        value_type_id: BOOL.to_owned(),
        category_id: Uuid::new_v4(),
        scope_class: "cascading".to_owned(),
        mode: "standard".to_owned(),
        status: DeclarationStatus::Active,
        domain_affinity: None,
        licence_feature: None,
        owner_module: None,
        display_name: None,
        description: None,
        default_value: json!(false),
        has_secret_trait: false,
        data_classification: "pii".to_owned(),
        requires_step_up: true,
        anonymous_exposable: false,
        source: "admin_authored".to_owned(),
        last_change_at: time::OffsetDateTime::now_utc(),
        updated_at: time::OffsetDateTime::now_utc(),
    }
}

/// Whether a refusal is the validation error that names `display_name`.
fn names_display_name(err: &DomainError) -> bool {
    matches!(err, DomainError::Validation { field, .. } if field == "display_name")
}

#[test]
fn a_display_name_edit_is_immediate_and_null_clears_it() {
    // A label changes no effective value, so it applies at once like the
    // description, and an explicit null is the way to remove it.
    let declaration = classified_declaration();
    let padded_to_the_bound = format!("  {}  ", "x".repeat(256));
    for value in [
        json!("Minimum TLS version"),
        json!("x"),
        json!("\u{451}".repeat(256)),
        json!(padded_to_the_bound),
        json!(null),
    ] {
        assert_eq!(
            classify_field("display_name", &value, &declaration).expect("classified"),
            FieldClass::Immediate,
            "{value}"
        );
    }
}

#[test]
fn a_display_name_of_the_wrong_shape_or_length_is_a_validation_error_naming_it() {
    // The bound is counted in characters after trimming, not bytes: 256 `ё`
    // are 512 bytes and pass, 257 characters do not.
    let declaration = classified_declaration();
    for value in [
        json!(7),
        json!(true),
        json!(["x"]),
        json!({}),
        json!(""),
        json!("   "),
        json!("x".repeat(257)),
        json!("\u{451}".repeat(257)),
    ] {
        let err = classify_field("display_name", &value, &declaration).expect_err("refused");
        assert!(names_display_name(&err), "{value}: {err:?}");
    }
}

#[tokio::test]
async fn a_created_display_name_is_stored_trimmed_and_an_absent_one_stays_null() {
    // The trimmed value is what is stored and what search matches. Nothing is
    // derived from the leaf name when the label is left out.
    let h = Harness::new().await;
    let mut request = h.request("with_label");
    request.display_name = Some("  Minimum TLS version  ".to_owned());
    let created = h.create(request, &admin_actor()).await.expect("created");
    assert_eq!(
        created.declaration.display_name.as_deref(),
        Some("Minimum TLS version")
    );
    assert_eq!(
        h.load(created.declaration.id).await.display_name.as_deref(),
        Some("Minimum TLS version")
    );

    let bare = h
        .create(h.request("without_label"), &admin_actor())
        .await
        .expect("created");
    assert_eq!(bare.declaration.display_name, None, "never derived");
}

#[tokio::test]
async fn a_create_with_an_empty_or_over_long_display_name_is_refused_naming_it() {
    let h = Harness::new().await;
    for (n, bad) in ["", "   ", &"x".repeat(257)].into_iter().enumerate() {
        let mut request = h.request(&format!("refused_{n}"));
        request.display_name = Some(bad.to_owned());
        let err = h
            .create(request, &admin_actor())
            .await
            .expect_err("refused");
        assert!(names_display_name(&err), "{n}: {err:?}");
    }
}

#[tokio::test]
async fn a_display_name_is_set_cleared_and_left_alone_by_a_patch() {
    // Absent is unchanged, present sets, null clears: the three states a
    // partial update has to tell apart.
    let h = Harness::new().await;
    let created = h
        .create(h.request("labelled"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;

    let tag = etag_of(&h.load(id).await);
    let set = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"display_name": "Minimum TLS version"}),
            &admin_actor(),
        )
        .await
        .expect("set");
    assert_eq!(set.display_name.as_deref(), Some("Minimum TLS version"));

    let tag = etag_of(&set);
    let untouched = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"description": "another"}),
            &admin_actor(),
        )
        .await
        .expect("a patch without the field");
    assert_eq!(
        untouched.display_name.as_deref(),
        Some("Minimum TLS version"),
        "absent from the body means unchanged"
    );

    let tag = etag_of(&untouched);
    let cleared = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"display_name": null}),
            &admin_actor(),
        )
        .await
        .expect("cleared");
    assert_eq!(cleared.display_name, None, "an explicit null clears it");
}

#[tokio::test]
async fn a_display_name_edit_moves_the_edit_tag_and_not_the_value_resolution_tag() {
    // Same rule as `mode` and `description`: a label changes no resolution, so
    // `last_change_at` stays and only the tag a console holds moves.
    let h = Harness::new().await;
    let created = h
        .create(h.request("labelled"), &admin_actor())
        .await
        .expect("created");
    let before = h.load(created.declaration.id).await;
    let tag = etag_of(&before);

    let after = h
        .update(
            before.id,
            Some(tag.as_str()),
            json!({"display_name": "Label"}),
            &admin_actor(),
        )
        .await
        .expect("edited");
    assert!(after.updated_at > before.updated_at, "the tag moved");
    assert_eq!(after.last_change_at, before.last_change_at);
}

#[tokio::test]
async fn a_patch_with_an_invalid_display_name_is_refused_and_changes_nothing() {
    let h = Harness::new().await;
    let mut request = h.request("labelled");
    request.display_name = Some("Keep me".to_owned());
    let created = h.create(request, &admin_actor()).await.expect("created");
    let id = created.declaration.id;

    for bad in [json!(""), json!("   "), json!("x".repeat(257)), json!(3)] {
        let tag = etag_of(&h.load(id).await);
        let err = h
            .update(
                id,
                Some(tag.as_str()),
                json!({ "display_name": bad }),
                &admin_actor(),
            )
            .await
            .expect_err("refused");
        assert!(names_display_name(&err), "{bad}: {err:?}");
    }
    assert_eq!(
        h.load(id).await.display_name.as_deref(),
        Some("Keep me"),
        "a refused edit leaves the stored label"
    );
}

#[tokio::test]
async fn a_contributed_declarations_display_name_is_not_admin_editable() {
    // The owning gear is the sole authority over its metadata; its label
    // arrives by re-registration, never by `PATCH`.
    let h = Harness::verified().await;
    let id = h
        .base
        .declare_labelled(
            "contributed",
            crate::domain::resolution::scope_class::CASCADING,
            json!(true),
            "Module label",
        )
        .await;
    let tag = etag_of(&h.load(id).await);

    for patch in [
        json!({"display_name": "mine now"}),
        json!({"display_name": null}),
    ] {
        let err = h
            .update(id, Some(tag.as_str()), patch.clone(), &admin_actor())
            .await
            .expect_err("contributed");
        assert_contributed_immutable(err, &patch.to_string());
    }
    assert_eq!(
        h.load(id).await.display_name.as_deref(),
        Some("Module label")
    );
}

#[test]
fn a_display_name_with_a_control_character_is_a_validation_error_naming_it() {
    // A label is one line of text a console renders; NUL, line breaks, tabs
    // and the C1 range are refused, whatever the surrounding text.
    let declaration = classified_declaration();
    for value in [
        json!("a\u{0}b"),
        json!("a\nb"),
        json!("a\tb"),
        json!("a\u{85}b"),
        json!("a\u{7f}b"),
    ] {
        let err = classify_field("display_name", &value, &declaration).expect_err("refused");
        assert!(names_display_name(&err), "{value}: {err:?}");
    }
}

#[tokio::test]
async fn a_create_with_a_control_character_in_the_display_name_is_refused_naming_it() {
    let h = Harness::new().await;
    let mut request = h.request("control");
    request.display_name = Some("a\u{0}b".to_owned());
    let err = h
        .create(request, &admin_actor())
        .await
        .expect_err("refused");
    assert!(names_display_name(&err), "{err:?}");
}

#[tokio::test]
async fn a_patch_stores_the_trimmed_boundary_value_and_a_one_character_value() {
    // The accepted boundary holds on a PATCH as on a create: 256 multi-byte
    // characters pass once the padding is trimmed, and the stored row carries
    // the trimmed value; a single character is enough.
    let h = Harness::new().await;
    let created = h
        .create(h.request("boundary"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let core = "\u{451}".repeat(256);

    let tag = etag_of(&h.load(id).await);
    let padded = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"display_name": format!("  {core}\t ")}),
            &admin_actor(),
        )
        .await
        .expect("accepted");
    assert_eq!(padded.display_name.as_deref(), Some(core.as_str()));
    assert_eq!(
        h.load(id).await.display_name.as_deref(),
        Some(core.as_str()),
        "the stored row carries the trimmed value"
    );

    let tag = etag_of(&padded);
    let one = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"display_name": "x"}),
            &admin_actor(),
        )
        .await
        .expect("accepted");
    assert_eq!(one.display_name.as_deref(), Some("x"));
}

/// The `display_name` a recorded image carries, whatever its class wrapper.
fn image_display_name(image: Option<&crate::audit::AuditValue>) -> Value {
    let image = serde_json::to_value(image.expect("an image")).expect("serialises");
    image["value"]["display_name"].clone()
}

#[tokio::test]
async fn the_audit_images_carry_the_display_name_before_and_after_each_edit() {
    // The record is the only trace of a label edit, so both snapshots must
    // hold it: trimmed on create, old and new on a change, null once cleared.
    let h = Harness::new().await;
    let mut request = h.request("audited");
    request.display_name = Some("  Old label  ".to_owned());
    let created = h.create(request, &admin_actor()).await.expect("created");
    let id = created.declaration.id;

    let tag = etag_of(&h.load(id).await);
    let renamed = h
        .update(
            id,
            Some(tag.as_str()),
            json!({"display_name": "New label"}),
            &admin_actor(),
        )
        .await
        .expect("renamed");
    let tag = etag_of(&renamed);
    h.update(
        id,
        Some(tag.as_str()),
        json!({"display_name": null}),
        &admin_actor(),
    )
    .await
    .expect("cleared");

    let records = h.audit.records();
    assert_eq!(records.len(), 3, "{records:?}");
    assert_eq!(
        image_display_name(records[0].post_image.as_ref()),
        json!("Old label"),
        "the create post-image carries the trimmed label"
    );
    assert_eq!(
        image_display_name(records[1].pre_image.as_ref()),
        json!("Old label"),
        "the change pre-image carries the old label"
    );
    assert_eq!(
        image_display_name(records[1].post_image.as_ref()),
        json!("New label"),
        "the change post-image carries the new label"
    );
    assert_eq!(
        image_display_name(records[2].pre_image.as_ref()),
        json!("New label")
    );
    assert_eq!(
        image_display_name(records[2].post_image.as_ref()),
        Value::Null,
        "clearing records null"
    );
}

// ── The revival report ───────────────────────────────────────────────────────

/// A validator that resolves every type's traits and judges every value as
/// the real one does, except that one value cannot be judged: the registry
/// is gone by the time it is asked about it.
struct FailsOnOneValue {
    inner: Arc<dyn crate::domain::validation::TypeValidator>,
    unjudgeable: Value,
}

#[async_trait::async_trait]
impl crate::domain::validation::TypeValidator for FailsOnOneValue {
    async fn validate_value(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<crate::domain::validation::ValidationResult, DomainError> {
        if *value == self.unjudgeable {
            return Err(DomainError::Unavailable {
                detail: "types registry down".to_owned(),
            });
        }
        self.inner.validate_value(value_type_id, value).await
    }

    async fn resolve_traits(
        &self,
        value_type_id: &str,
    ) -> Result<crate::domain::validation::TraitSet, DomainError> {
        self.inner.resolve_traits(value_type_id).await
    }
}

#[tokio::test]
async fn a_validator_that_goes_down_at_the_row_re_validation_fails_the_report_unavailable() {
    // The traits and the Schema Default are judged first and succeed; it is
    // the re-validation of a retained row that cannot be answered, and the
    // whole report fails with it rather than listing the rows it could judge.
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    let (root, a) = (h.base.tree.root, h.base.tree.a);
    h.base.set(id, root, json!(true)).await;
    h.base.set(id, a, json!("unjudgeable")).await;

    let flaky = DeclarationAdmin::new(
        DeclarationRepo,
        CategoryRepo,
        ValueRepo,
        Arc::new(FailsOnOneValue {
            inner: Arc::new(GtsTypeValidator::new(resolution_catalogue())),
            unjudgeable: json!("unjudgeable"),
        }),
        Arc::clone(&h.registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
        Arc::new(FixedStepUp::verified()),
        Arc::clone(&h.audit),
        Arc::clone(&h.base.cache),
        PendingSecretRepo,
        Arc::clone(&h.secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::clone(&h.metrics) as Arc<dyn crate::domain::ports::WriteMetrics>,
    );
    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let request = h.request("retry_policy");
    let report = |tenants: Vec<Uuid>| {
        let (flaky, conn, scope, request) = (&flaky, &conn, &scope, &request);
        async move {
            let plan = flaky
                .plan_revival(conn, scope, request, &OpenGate)
                .await
                .expect("planned");
            flaky
                .revival_report(conn, scope, plan, request, 100, || async move {
                    Ok(super::Walk {
                        tenants,
                        truncated: false,
                    })
                })
                .await
        }
    };

    let err = report(vec![root, a])
        .await
        .expect_err("the row cannot be judged");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");

    // The same admin reports a walk that does not reach the unjudgeable row.
    let whole = report(vec![root]).await.expect("reported");
    assert_eq!(whole.rows.len(), 1);
    assert!(!whole.rows[0].needs_review);

    // Advisory: the declaration is still retired and nothing was audited
    // beyond the create and the retire.
    assert_eq!(h.load(id).await.status, DeclarationStatus::Retired);
    assert_eq!(h.audit.operations(), vec!["create", "remove"]);
}

/// A read gate that lets every caller read every declaration.
struct OpenGate;

#[async_trait::async_trait]
impl super::RevivalReadGate for OpenGate {
    async fn may_read(&self, _declaration: &Declaration) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// A read gate that refuses every declaration to the caller.
struct ClosedGate;

#[async_trait::async_trait]
impl super::RevivalReadGate for ClosedGate {
    async fn may_read(&self, _declaration: &Declaration) -> Result<bool, DomainError> {
        Ok(false)
    }
}

#[tokio::test]
async fn a_refusing_read_gate_answers_nothing_to_revive_like_an_absent_declaration() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    h.retire(created.declaration.id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();

    let refused = h
        .admin
        .plan_revival(&conn, &scope, &h.request("retry_policy"), &ClosedGate)
        .await
        .expect_err("refused");
    let absent = h
        .admin
        .plan_revival(&conn, &scope, &h.request("never_declared"), &OpenGate)
        .await
        .expect_err("absent");
    assert_eq!(format!("{refused:?}"), format!("{absent:?}"));
    assert!(matches!(
        refused,
        DomainError::NotFound {
            resource: super::NOTHING_TO_REVIVE
        }
    ));
    assert!(
        h.admin
            .plan_revival(&conn, &scope, &h.request("retry_policy"), &OpenGate)
            .await
            .is_ok()
    );
}

#[test]
fn a_listed_value_is_masked_by_the_strictest_class_and_an_unknown_one_fails_closed() {
    use super::{DataClassification as C, strictest_classification as strictest};
    // A stored string becomes the enum through `from_stored`, which is where
    // an unknown one ranks as the strictest.
    let stored = |s: &str| C::from_stored(s);
    assert_eq!(strictest([C::Public, C::Public]), C::Public);
    assert_eq!(strictest([C::Public, C::Pii]), C::Pii);
    assert_eq!(strictest([C::Pii, C::Public, C::Secret]), C::Secret);
    assert_eq!(strictest([C::Pii, C::Secret]), C::Secret);
    assert_eq!(
        strictest([C::Public, stored("restricted")]),
        C::Secret,
        "unknown"
    );
    assert_eq!(
        strictest([stored(""), C::Public]),
        C::Secret,
        "empty is unknown"
    );
    assert_eq!(strictest(std::iter::empty()), C::Public);
}

/// A validator that records every value it was asked to judge.
struct CountingValidator {
    inner: Arc<dyn crate::domain::validation::TypeValidator>,
    judged: std::sync::Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl crate::domain::validation::TypeValidator for CountingValidator {
    async fn validate_value(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<crate::domain::validation::ValidationResult, DomainError> {
        self.judged.lock().expect("lock").push(value.clone());
        self.inner.validate_value(value_type_id, value).await
    }

    async fn resolve_traits(
        &self,
        value_type_id: &str,
    ) -> Result<crate::domain::validation::TraitSet, DomainError> {
        self.inner.resolve_traits(value_type_id).await
    }
}

#[tokio::test]
async fn rows_holding_the_same_value_are_judged_once_and_each_gets_its_own_verdict() {
    let h = Harness::verified().await;
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");

    // The same text twice; one object with its keys in two orders; the
    // integer `1` and the float `1.0`; the one valid boolean twice.
    let held = [
        json!("x"),
        json!("x"),
        json!({"a": 1, "b": 2}),
        serde_json::from_str(r#"{"b": 2, "a": 1}"#).expect("json"),
        json!(1),
        json!(1.0),
        json!(true),
        json!(true),
    ];
    let mut tenants = Vec::new();
    for value in &held {
        let tenant = Uuid::new_v4();
        h.base.set(id, tenant, value.clone()).await;
        tenants.push(tenant);
    }

    let counting = Arc::new(CountingValidator {
        inner: Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        judged: std::sync::Mutex::new(Vec::new()),
    });
    let admin = DeclarationAdmin::new(
        DeclarationRepo,
        CategoryRepo,
        ValueRepo,
        Arc::clone(&counting) as Arc<dyn crate::domain::validation::TypeValidator>,
        Arc::clone(&h.registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
        Arc::new(FixedStepUp::verified()),
        Arc::clone(&h.audit),
        Arc::clone(&h.base.cache),
        PendingSecretRepo,
        Arc::clone(&h.secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::clone(&h.metrics) as Arc<dyn crate::domain::ports::WriteMetrics>,
    );
    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let request = h.request("retry_policy");
    let plan = admin
        .plan_revival(&conn, &scope, &request, &OpenGate)
        .await
        .expect("planned");
    let walk_tenants = tenants.clone();
    let report = admin
        .revival_report(&conn, &scope, plan, &request, 100, || async move {
            Ok(super::Walk {
                tenants: walk_tenants,
                truncated: false,
            })
        })
        .await
        .expect("reported");

    // The Schema Default is judged by the create's checks, once; every
    // other judgement is of a retained value.
    let judged: Vec<Value> = counting
        .judged
        .lock()
        .expect("lock")
        .iter()
        .filter(|v| **v != request.default_value)
        .cloned()
        .collect();
    let distinct = judged.len();
    assert_eq!(
        distinct, 5,
        "`x`, the object, `1`, `1.0` and `true`, each once: {judged:?}"
    );
    assert_eq!(report.rows.len(), held.len());
    for (row, value) in report.rows.iter().zip(&held) {
        assert_eq!(row.value.as_ref(), Some(value));
        assert_eq!(
            row.needs_review,
            *value != json!(true),
            "a boolean validates, everything else is flagged: {value}"
        );
        assert_eq!(row.needs_review_detail.is_some(), row.needs_review);
    }
    assert_eq!(report.total_needs_review, 6);
}

#[test]
fn equal_objects_are_one_verdict_key_whatever_their_key_order() {
    // Built member by member in the opposite order: with `serde_json`'s
    // `preserve_order` on, as a workspace build has it, the two print
    // differently and are still equal, nested objects included — and the
    // verdicts, keyed by the value, hold them as one key.
    let mut inner = Map::new();
    inner.insert("q".to_owned(), json!(1));
    inner.insert("p".to_owned(), json!(2));
    let mut outer = Map::new();
    outer.insert("x".to_owned(), json!([Value::Object(inner)]));
    outer.insert("y".to_owned(), json!(1));
    let reversed = json!({ "outer": Value::Object(outer) });
    let ordered = json!({ "outer": { "y": 1, "x": [{ "p": 2, "q": 1 }] } });
    assert_eq!(ordered, reversed);
    let mut verdicts: HashMap<Value, Option<String>> = HashMap::new();
    verdicts.insert(ordered, None);
    verdicts.insert(reversed.clone(), Some("flagged".to_owned()));
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts.get(&reversed), Some(&Some("flagged".to_owned())));
}

/// The real value repository, recording the tenant batch of every listing
/// read it is asked for.
#[derive(Clone, Default)]
struct BatchRecordingValues {
    listings: Arc<std::sync::Mutex<Vec<Vec<Uuid>>>>,
}

#[async_trait::async_trait]
impl ValueRepository for BatchRecordingValues {
    async fn resync_classification<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        data_classification: &str,
    ) -> Result<u64, DomainError> {
        ValueRepo
            .resync_classification(conn, scope, declaration_id, data_classification)
            .await
    }

    async fn find_in_tenants<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo
            .find_in_tenants(conn, scope, declaration_id, tenant_ids)
            .await
    }

    async fn find_many_in_tenants<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_ids: &[Uuid],
        tenant_ids: &[Uuid],
    ) -> Result<Vec<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo
            .find_many_in_tenants(conn, scope, declaration_ids, tenant_ids)
            .await
    }

    async fn count_in_tenants<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<u64, DomainError> {
        ValueRepo
            .count_in_tenants(conn, scope, declaration_id, tenant_ids)
            .await
    }

    async fn distinct_values_in_tenants<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<crate::domain::value::DistinctValue>, DomainError> {
        ValueRepo
            .distinct_values_in_tenants(conn, scope, declaration_id, tenant_ids)
            .await
    }

    async fn list_in_tenants<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_ids: &[Uuid],
    ) -> Result<Vec<crate::domain::value::ListedValue>, DomainError> {
        self.listings
            .lock()
            .expect("lock")
            .push(tenant_ids.to_vec());
        ValueRepo
            .list_in_tenants(conn, scope, declaration_id, tenant_ids)
            .await
    }

    async fn find_all<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo.find_all(conn, scope, declaration_id).await
    }

    async fn lock_all<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
    ) -> Result<Vec<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo.lock_all(conn, scope, declaration_id).await
    }

    async fn flag<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        detail: Option<String>,
    ) -> Result<(), DomainError> {
        ValueRepo.flag(conn, scope, id, detail).await
    }

    async fn find_one<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Option<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo
            .find_one(conn, scope, declaration_id, tenant_id)
            .await
    }

    async fn count_flagged<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        source: &str,
    ) -> Result<u64, DomainError> {
        ValueRepo.count_flagged(conn, scope, source).await
    }

    async fn list_review<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        request: &crate::domain::value::ReviewRequest<'_>,
    ) -> Result<toolkit_odata::Page<crate::domain::value::StoredValue>, DomainError> {
        ValueRepo.list_review(conn, request).await
    }

    async fn insert<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        draft: crate::domain::value::ValueDraft,
    ) -> Result<crate::domain::value::StoredValue, DomainError> {
        ValueRepo.insert(conn, scope, draft).await
    }

    async fn update<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        value: Option<Value>,
        secret_ref: Option<String>,
        set_by: &str,
        set_by_tenant: Option<Uuid>,
        expected: time::OffsetDateTime,
    ) -> Result<crate::domain::value::StoredValue, DomainError> {
        ValueRepo
            .update(
                conn,
                scope,
                id,
                value,
                secret_ref,
                set_by,
                set_by_tenant,
                expected,
            )
            .await
    }

    async fn delete<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_id: Uuid,
        tenant_id: Uuid,
        expected: time::OffsetDateTime,
    ) -> Result<(), DomainError> {
        ValueRepo
            .delete(conn, scope, declaration_id, tenant_id, expected)
            .await
    }

    async fn secret_refs<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, DomainError> {
        ValueRepo.secret_refs(conn, declaration_id).await
    }

    async fn references_held_by_others<C: toolkit_db::secure::DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
        refs: &[String],
    ) -> Result<std::collections::HashSet<String>, DomainError> {
        ValueRepo
            .references_held_by_others(conn, declaration_id, refs)
            .await
    }
}

/// A retired `retry_policy` declaration, its report asked of an admin whose
/// value repository records the listing batches, over `walk`, with `limit`.
async fn report_over(
    h: &Harness,
    walk: Vec<Uuid>,
    limit: usize,
) -> (super::RevivalReport, Vec<Vec<Uuid>>) {
    let values = BatchRecordingValues::default();
    let admin = DeclarationAdmin::new(
        DeclarationRepo,
        CategoryRepo,
        values.clone(),
        Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        Arc::clone(&h.registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
        Arc::new(FixedStepUp::verified()),
        Arc::clone(&h.audit),
        Arc::clone(&h.base.cache),
        PendingSecretRepo,
        Arc::clone(&h.secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::clone(&h.metrics) as Arc<dyn crate::domain::ports::WriteMetrics>,
    );
    let conn = h.base.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let request = h.request("retry_policy");
    let plan = admin
        .plan_revival(&conn, &scope, &request, &OpenGate)
        .await
        .expect("planned");
    let report = admin
        .revival_report(&conn, &scope, plan, &request, limit, || async move {
            Ok(super::Walk {
                tenants: walk,
                truncated: false,
            })
        })
        .await
        .expect("reported");
    let batches = values.listings.lock().expect("lock").clone();
    (report, batches)
}

async fn retired_retry_policy(h: &Harness) -> Uuid {
    let created = h
        .create(h.request("retry_policy"), &admin_actor())
        .await
        .expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    id
}

#[tokio::test]
async fn a_small_limit_lists_the_first_rows_in_walk_order_and_still_counts_every_row() {
    let h = Harness::verified().await;
    let id = retired_retry_policy(&h).await;
    // The walk's order is not the order the rows were written in; the
    // valid value is held by many tenants and an invalid one only by the
    // last tenants of the walk, which are counted but not listed.
    let walk: Vec<Uuid> = (0..6).map(|_| Uuid::new_v4()).collect();
    for tenant in walk.iter().rev().take(3) {
        h.base.set(id, *tenant, json!("not a boolean")).await;
    }
    for tenant in walk.iter().take(3).rev() {
        h.base.set(id, *tenant, json!(true)).await;
    }

    let (report, batches) = report_over(&h, walk.clone(), 2).await;
    let listed: Vec<Uuid> = report.rows.iter().map(|r| r.tenant_id).collect();
    assert_eq!(listed, walk[..2].to_vec(), "the first two, in walk order");
    assert!(report.rows.iter().all(|r| !r.needs_review));
    assert_eq!(report.total, 6);
    assert_eq!(
        report.total_needs_review, 3,
        "the invalid rows after the listed ones are counted"
    );
    assert_eq!(report.scanned, 6);
    assert_eq!(batches.len(), 1, "the one batch fills the limit");

    // A limit past the rows lists them all, each with its own verdict.
    let (all, _) = report_over(&h, walk.clone(), 100).await;
    assert_eq!(all.rows.len(), 6);
    assert_eq!(
        all.rows.iter().filter(|r| r.needs_review).count(),
        3,
        "the verdicts are per row"
    );
    assert_eq!(all.rows[5].tenant_id, walk[5]);
}

#[tokio::test]
async fn the_listing_stops_after_the_first_tenant_batch_that_fills_the_limit() {
    let h = Harness::verified().await;
    let id = retired_retry_policy(&h).await;
    let batch = super::REVIVAL_TENANT_BATCH;
    let walk: Vec<Uuid> = (0..batch + 100).map(|_| Uuid::new_v4()).collect();
    // Three rows in the first batch, three in the second.
    for at in [0, 1, 2, batch + 50, batch + 51, batch + 52] {
        h.base.set(id, walk[at], json!(true)).await;
    }

    let (report, batches) = report_over(&h, walk.clone(), 2).await;
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.total, 6);
    assert_eq!(batches.len(), 1, "the first batch fills the limit");
    assert_eq!(batches[0], walk[..batch].to_vec());

    // Asked for more than the first batch holds, the listing goes on to the
    // second, in walk order, and stops there.
    let (report, batches) = report_over(&h, walk.clone(), 4).await;
    let listed: Vec<Uuid> = report.rows.iter().map(|r| r.tenant_id).collect();
    assert_eq!(
        listed,
        vec![walk[0], walk[1], walk[2], walk[batch + 50]],
        "the walk's order across batches"
    );
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[1], walk[batch..].to_vec());

    // A limit of nothing lists nothing and reads no batch, yet counts.
    let (report, batches) = report_over(&h, walk, 0).await;
    assert!(report.rows.is_empty());
    assert_eq!(report.total, 6);
    assert!(batches.is_empty());
}
// ── Purge ────────────────────────────────────────────────────────────────────

fn conflict_detail(err: DomainError) -> String {
    match err {
        DomainError::Conflict { detail } => detail,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_purge_refuses_an_active_and_a_contributed_declaration_and_wants_a_current_tag() {
    let h = Harness::verified().await;

    // Active: refused as not retired, and still there.
    let created = h
        .create(h.request("live"), &admin_actor())
        .await
        .expect("created");
    let live = created.declaration.id;
    let tag = etag_of(&created.declaration);
    let err = h
        .purge(live, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("active");
    assert!(
        conflict_detail(err).starts_with(super::conflict::NOT_RETIRED),
        "the status refusal"
    );
    assert_eq!(h.load(live).await.status, DeclarationStatus::Active);

    // Contributed, active or retired: the kind is judged before the status.
    let contributed = h
        .base
        .declare(
            "owned",
            crate::domain::resolution::scope_class::CASCADING,
            json!(true),
        )
        .await;
    let tag = etag_of(&h.load(contributed).await);
    let err = h
        .purge(contributed, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("contributed");
    assert!(conflict_detail(err).starts_with(super::conflict::CONTRIBUTED_IMMUTABLE));
    h.base.retire(contributed).await;
    let tag = etag_of(&h.load(contributed).await);
    let err = h
        .purge(contributed, Some(tag.as_str()), &admin_actor())
        .await
        .expect_err("contributed, retired");
    assert!(conflict_detail(err).starts_with(super::conflict::CONTRIBUTED_IMMUTABLE));

    // Retired: the tag is required, and a stale one is refused.
    let (id, tag) = h.retired("gone").await;
    let err = h.purge(id, None, &admin_actor()).await.expect_err("no tag");
    assert!(
        matches!(err, DomainError::PreconditionRequired { .. }),
        "{err:?}"
    );
    let err = h
        .purge(id, Some("\"stale\""), &admin_actor())
        .await
        .expect_err("stale tag");
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "{err:?}"
    );
    assert_eq!(
        h.load(id).await.status,
        DeclarationStatus::Retired,
        "nothing was removed"
    );

    // And a current one purges.
    h.purge(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");
}

#[tokio::test]
async fn a_purge_collects_every_reference_held_at_any_scope_and_subject_and_records_a_remove() {
    let h = Harness::verified().await;
    let (id, tag) = h.retired("tokens").await;
    let key = h.load(id).await.key;
    let t = &h.base.tree;
    h.base.set(id, t.root, json!(true)).await;
    h.base.set_secret(id, t.a, "value-a").await;
    h.base.set_secret(id, t.b, "value-b").await;
    h.base.set_subject_secret(id, t.c, "value-subject").await;
    h.base.stage_secret(id, t.s, "staged-s").await;
    // The same entry named twice is released once.
    h.base.stage_secret(id, t.a, "value-a").await;

    let purged = h
        .purge(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");

    assert_eq!(purged.key, key);
    let mut refs: Vec<_> = purged
        .secrets
        .iter()
        .map(|e| (e.tenant, e.secret_ref.as_str()))
        .collect();
    refs.sort();
    let mut expected = vec![
        (t.a, "value-a"),
        (t.b, "value-b"),
        (t.c, "value-subject"),
        (t.s, "staged-s"),
    ];
    expected.sort();
    assert_eq!(refs, expected);
    let conn = h.base.db.conn().expect("connection");
    assert!(
        ValueRepo
            .find_all(&conn, &AccessScope::allow_all(), id)
            .await
            .expect("rows")
            .is_empty()
    );
    let records = h.audit.records();
    let last = records.last().expect("a record");
    assert_eq!(last.operation, AuditOperation::Remove);
    assert!(last.pre_image.is_some() && last.post_image.is_none());
    assert_eq!(last.declaration_key, key);
}

#[tokio::test]
async fn every_declaration_action_counts_its_step_up_under_its_own_operation() {
    let h = Harness::verified().await;

    // update: loosening a gate needs step-up.
    let created = h
        .create(h.request("tuned"), &admin_actor())
        .await
        .expect("created");
    let tag = etag_of(&created.declaration);
    h.update(
        created.declaration.id,
        Some(tag.as_str()),
        json!({"requires_step_up": false}),
        &admin_actor(),
    )
    .await
    .expect("updated");

    // evolve: a changed default on the active declaration.
    let mut evolved = h.request("tuned");
    evolved.default_value = json!(true);
    let evolved = h.create(evolved, &admin_actor()).await.expect("evolved");
    assert!(evolved.evolved);

    // retire, reactivate, purge on another one.
    let (id, _) = h.retired("cycled").await;
    let revived = h
        .create(h.request("cycled"), &admin_actor())
        .await
        .expect("revived");
    assert!(revived.reactivated);
    let tag = etag_of(&revived.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired again");
    let tag = etag_of(&h.load(id).await);
    h.purge(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");

    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Update),
        vec!["verified"]
    );
    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Evolve),
        vec!["verified"]
    );
    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Reactivate),
        vec!["verified"]
    );
    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Retire),
        vec!["verified"; 2]
    );
    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Purge),
        vec!["verified"]
    );
}

#[tokio::test]
async fn a_refused_step_up_is_counted_by_its_code_and_a_service_principal_as_its_own_result() {
    let h = Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Stale))).await;
    // Judged before anything is read: no row at this id, still the challenge.
    let err = h
        .purge(Uuid::new_v4(), Some("\"any\""), &admin_actor())
        .await
        .expect_err("challenge");
    assert!(matches!(err, DomainError::StepUpRequired { .. }), "{err:?}");
    assert_eq!(h.metrics.step_ups_of(StepUpOperation::Purge), vec!["stale"]);

    let err = h
        .purge(Uuid::new_v4(), Some("\"any\""), &service_actor())
        .await
        .expect_err("machine");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
    assert_eq!(
        h.metrics.step_ups_of(StepUpOperation::Purge),
        vec!["stale", "service_principal"]
    );

    // An outage of the verifier is counted too, and is no challenge.
    let down = Harness::with_step_up(Arc::new(FixedStepUp::refusing(StepUpRefusal::Unavailable(
        "authn down".to_owned(),
    ))))
    .await;
    let err = down
        .purge(Uuid::new_v4(), Some("\"any\""), &admin_actor())
        .await
        .expect_err("unavailable");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    assert_eq!(
        down.metrics.step_ups_of(StepUpOperation::Purge),
        vec!["unavailable"]
    );
}

// ── The release ──────────────────────────────────────────────────────────────

fn held(n: usize) -> super::PurgedDeclaration {
    super::PurgedDeclaration {
        id: Uuid::new_v4(),
        key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.x.v1~".to_owned(),
        secrets: (0..n)
            .map(|i| super::HeldSecret {
                tenant: Uuid::new_v4(),
                secret_ref: format!("ref-{i}"),
            })
            .collect(),
        kept_secrets: Vec::new(),
    }
}

#[tokio::test]
async fn the_release_counts_ok_for_every_entry_and_failed_when_the_store_refuses() {
    let h = Harness::verified().await;
    let purged = held(3);
    for entry in &purged.secrets {
        h.secrets.seed(&entry.secret_ref, "plaintext");
    }

    let summary = h.admin.release_purged(purged.clone(), "req-1").await;

    assert_eq!((summary.released, summary.failed), (3, 0));
    assert!(h.secrets.held().is_empty());
    assert_eq!(
        *h.metrics.purge_releases.lock().expect("lock"),
        vec!["ok"; 3]
    );

    h.secrets.go_down();
    let summary = h.admin.release_purged(purged, "req-2").await;
    assert_eq!((summary.released, summary.failed), (0, 3));
    assert_eq!(
        h.metrics
            .purge_releases
            .lock()
            .expect("lock")
            .iter()
            .filter(|r| **r == "failed")
            .count(),
        3
    );
}

/// A store that answers every release after a pause and remembers how many
/// were in flight at once; optionally holds each one at a gate.
struct SlowStore {
    in_flight: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    started: std::sync::atomic::AtomicUsize,
    released: std::sync::atomic::AtomicUsize,
    gate: Option<tokio::sync::Semaphore>,
}

impl SlowStore {
    fn new(gated: bool) -> Arc<Self> {
        Arc::new(Self {
            in_flight: 0.into(),
            peak: 0.into(),
            started: 0.into(),
            released: 0.into(),
            gate: gated.then(|| tokio::sync::Semaphore::new(0)),
        })
    }
}

#[async_trait::async_trait]
impl crate::domain::ports::SecretManager for SlowStore {
    fn mint_reference(&self, _key: &str, _tenant: Uuid) -> String {
        unreachable!("a release mints nothing")
    }
    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
        _plaintext: &Value,
    ) -> Result<(), DomainError> {
        unreachable!("a release stores nothing")
    }
    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<SecretString, DomainError> {
        unreachable!("a release reads nothing")
    }
    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<(), DomainError> {
        use std::sync::atomic::Ordering::SeqCst;
        self.started.fetch_add(1, SeqCst);
        let now = self.in_flight.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now, SeqCst);
        if let Some(gate) = &self.gate {
            gate.acquire().await.expect("open gate").forget();
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        self.in_flight.fetch_sub(1, SeqCst);
        self.released.fetch_add(1, SeqCst);
        Ok(())
    }
}

/// The admin of a harness, over a Secret Manager of the test's choosing.
fn admin_over(h: &Harness, secrets: Arc<dyn crate::domain::ports::SecretManager>) -> Admin {
    DeclarationAdmin::new(
        DeclarationRepo,
        CategoryRepo,
        ValueRepo,
        Arc::new(GtsTypeValidator::new(resolution_catalogue())),
        Arc::clone(&h.registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
        Arc::new(FixedStepUp::verified()),
        Arc::clone(&h.audit),
        Arc::clone(&h.base.cache),
        PendingSecretRepo,
        secrets,
        Arc::clone(&h.metrics) as Arc<dyn crate::domain::ports::WriteMetrics>,
    )
}

#[tokio::test]
async fn the_release_runs_at_most_four_at_a_time() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = Harness::verified().await;
    let store = SlowStore::new(false);
    let admin = admin_over(
        &h,
        Arc::clone(&store) as Arc<dyn crate::domain::ports::SecretManager>,
    );

    let summary = admin.release_purged(held(10), "req").await;

    assert_eq!((summary.released, summary.failed), (10, 0));
    assert_eq!(store.released.load(SeqCst), 10);
    let peak = store.peak.load(SeqCst);
    assert!(peak <= 4, "never more than four in flight: {peak}");
    assert!(peak > 1, "and the releases do overlap: {peak}");
}

#[tokio::test]
async fn the_release_finishes_when_the_caller_that_started_it_is_dropped() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = Harness::verified().await;
    let store = SlowStore::new(true);
    let admin = admin_over(
        &h,
        Arc::clone(&store) as Arc<dyn crate::domain::ports::SecretManager>,
    );

    {
        let release = admin.release_purged(held(6), "req");
        tokio::pin!(release);
        // Run the caller until the first release has begun, then drop it.
        let begun = async {
            while store.started.load(SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            _ = &mut release => panic!("the gate is shut; nothing can have finished"),
            () = begun => {}
        }
    }
    store.gate.as_ref().expect("gated").add_permits(6);

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.released.load(SeqCst) < 6 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every entry is released although the caller is gone");
}

// ── What an evolution shares, and what a purge must not take ────────────────

/// The references still named by a value or staged row of `id`.
async fn refs_held_by(h: &Harness, id: Uuid) -> Vec<String> {
    let conn = h.base.db.conn().expect("connection");
    let mut refs: Vec<String> = ValueRepo
        .secret_refs(&conn, id)
        .await
        .expect("refs")
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    refs.sort();
    refs
}

#[tokio::test]
async fn a_purge_releases_none_of_the_entries_another_declaration_still_holds() {
    let h = Harness::verified().await;
    let (id, tag) = h.retired("old").await;
    let successor = h
        .base
        .declare(
            "new",
            crate::domain::resolution::scope_class::CASCADING,
            json!(true),
        )
        .await;
    let t = &h.base.tree;
    // What an evolution's copy leaves: the successor's row names the same
    // reference as the retired predecessor's.
    h.base.set_secret(id, t.a, "shared-value").await;
    h.base.set_secret(successor, t.a, "shared-value").await;
    // A reference another declaration holds only as a staged row.
    h.base.set_secret(id, t.b, "shared-staged").await;
    h.base.stage_secret(successor, t.b, "shared-staged").await;
    // And the ones nobody else holds.
    h.base.set_secret(id, t.c, "own-value").await;
    h.base.stage_secret(id, t.s, "own-staged").await;
    for reference in ["shared-value", "shared-staged", "own-value", "own-staged"] {
        h.secrets.seed(reference, "plaintext");
    }

    let purged = h
        .purge(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");

    let mut released: Vec<_> = purged
        .secrets
        .iter()
        .map(|e| e.secret_ref.as_str())
        .collect();
    released.sort_unstable();
    assert_eq!(released, vec!["own-staged", "own-value"]);
    assert_eq!(
        purged.kept_secrets.len(),
        2,
        "both shared references are counted"
    );
    let summary = h.admin.release_purged(purged, "req").await;
    assert_eq!((summary.released, summary.failed), (2, 0));
    assert_eq!(
        h.secrets.held(),
        vec!["shared-staged".to_owned(), "shared-value".to_owned()],
        "the entries the successor still holds are in the store"
    );
    assert_eq!(
        refs_held_by(&h, successor).await,
        vec!["shared-value".to_owned()],
        "and its row still names its own"
    );
}

#[tokio::test]
async fn after_an_evolution_of_a_secret_setting_a_purge_of_its_predecessor_keeps_the_successors_entries()
 {
    let h = Harness::verified().await;
    let mut first = h.request("api_token");
    first.value_type_id = SECRET.to_owned();
    first.default_value = json!("");
    let first = h.create(first, &admin_actor()).await.expect("created");
    let old = first.declaration.id;
    let t = &h.base.tree;
    h.base.set_secret(old, t.a, "tenant-a-entry").await;
    h.base.set_secret(old, t.b, "tenant-b-entry").await;
    h.secrets.seed("tenant-a-entry", "plaintext-a");
    h.secrets.seed("tenant-b-entry", "plaintext-b");

    // Evolve: a changed scope class on the active declaration.
    let mut next = h.request("api_token");
    next.value_type_id = SECRET.to_owned();
    next.default_value = json!("");
    next.scope_class = "local".to_owned();
    let evolved = h.create(next, &admin_actor()).await.expect("evolved");
    assert!(evolved.evolved);
    let successor = evolved.declaration.id;
    assert_eq!(
        refs_held_by(&h, successor).await,
        vec!["tenant-a-entry".to_owned(), "tenant-b-entry".to_owned()],
        "the evolution copied the references"
    );

    let tag = etag_of(&h.load(old).await);
    let purged = h
        .purge(old, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");
    assert_eq!(purged.secrets, Vec::<super::HeldSecret>::new());
    assert_eq!(purged.kept_secrets.len(), 2);
    let summary = h.admin.release_purged(purged, "req").await;

    assert_eq!((summary.released, summary.failed), (0, 0));
    assert_eq!(
        h.secrets.held(),
        vec!["tenant-a-entry".to_owned(), "tenant-b-entry".to_owned()],
        "the live successor keeps its secrets"
    );
    assert!(h.secrets.deleted.lock().expect("lock").is_empty());
    let key = h.load(successor).await.key;
    for (tenant, reference, plaintext) in [
        (t.a, "tenant-a-entry", "plaintext-a"),
        (t.b, "tenant-b-entry", "plaintext-b"),
    ] {
        let resolved = h
            .secrets
            .resolve_plaintext(&key, tenant, reference)
            .await
            .expect("the successor's secret still resolves");
        assert_eq!(secrecy::ExposeSecret::expose_secret(&resolved), plaintext);
    }
}

#[tokio::test]
async fn a_purge_outside_the_callers_administrative_domain_is_not_found_and_removes_nothing() {
    let h = Harness::verified().await;
    let mut request = h.request("domained");
    request.domain_affinity = Some("infra".to_owned());
    let created = h.create(request, &admin_actor()).await.expect("created");
    let id = created.declaration.id;
    let tag = etag_of(&created.declaration);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    let tag = etag_of(&h.load(id).await);
    h.base
        .set_secret(id, h.base.tree.a, "inside-the-domain")
        .await;
    h.secrets.seed("inside-the-domain", "plaintext");
    let billing = AccessScope::single(toolkit_security::ScopeConstraint::new(vec![
        toolkit_security::ScopeFilter::In(toolkit_security::InScopeFilter::new(
            crate::domain::category::visibility::DOMAIN_PROPERTY,
            vec!["billing".into()],
        )),
    ]));
    let mut actor = admin_actor();
    actor.visibility = crate::domain::category::domain_visibility(&billing);
    let conn = h.base.db.conn().expect("connection");

    let err = h
        .admin
        .purge(&conn, &billing, id, Some(tag.as_str()), &actor)
        .await
        .expect_err("outside the caller's domain");

    assert!(
        matches!(
            err,
            DomainError::NotFound {
                resource: "declaration"
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        h.load(id).await.status,
        DeclarationStatus::Retired,
        "the row stays"
    );
    assert_eq!(
        refs_held_by(&h, id).await,
        vec!["inside-the-domain".to_owned()]
    );
    assert_eq!(h.secrets.held(), vec!["inside-the-domain".to_owned()]);
    assert!(
        h.secrets.deleted.lock().expect("lock").is_empty(),
        "nothing is released"
    );
}

/// What a purge keeps for another declaration, checked again after the commit.
#[tokio::test]
async fn the_kept_references_of_a_purge_are_released_once_the_other_declaration_is_gone() {
    let h = Harness::verified().await;
    let (id, tag) = h.retired("old").await;
    let other = h
        .base
        .declare(
            "new",
            crate::domain::resolution::scope_class::CASCADING,
            json!(true),
        )
        .await;
    let t = &h.base.tree;
    h.base.set_secret(id, t.a, "shared-value").await;
    h.base.set_secret(other, t.a, "shared-value").await;
    h.base.stage_secret(id, t.b, "own-staged").await;
    h.secrets.seed("shared-value", "plaintext");
    h.secrets.seed("own-staged", "plaintext");
    let mut purged = h
        .purge(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("purged");
    assert_eq!(purged.id, id);
    assert_eq!(purged.kept_secrets.len(), 1);
    let conn = h.base.db.conn().expect("connection");

    // The other declaration still holds it: nothing moves.
    h.admin.reclaim_kept(&conn, &mut purged).await;
    assert_eq!(purged.kept_secrets.len(), 1, "still held, still kept");
    assert_eq!(purged.secrets.len(), 1);

    // Its own purge commits meanwhile: the rows are gone.
    let current = h.load(other).await;
    DeclarationRepo
        .delete(&conn, &AccessScope::allow_all(), other, current.updated_at)
        .await
        .expect("the other declaration is deleted");
    h.admin.reclaim_kept(&conn, &mut purged).await;
    assert_eq!(purged.kept_secrets, Vec::<super::HeldSecret>::new());
    let mut released: Vec<_> = purged
        .secrets
        .iter()
        .map(|e| e.secret_ref.as_str())
        .collect();
    released.sort_unstable();
    assert_eq!(released, vec!["own-staged", "shared-value"]);
    let summary = h.admin.release_purged(purged, "req").await;
    assert_eq!((summary.released, summary.failed), (2, 0));
    assert_eq!(h.secrets.held(), Vec::<String>::new());
}

/// A declaration outside the caller's administrative domain is not found by a
/// revive or an evolution either, whatever was read about it before the lock:
/// both read it again by id under the lock, with the caller's visibility.
#[tokio::test]
async fn a_revive_and_an_evolution_outside_the_callers_domain_are_not_found_and_change_nothing() {
    let h = Harness::verified().await;
    let billing = AccessScope::single(toolkit_security::ScopeConstraint::new(vec![
        toolkit_security::ScopeFilter::In(toolkit_security::InScopeFilter::new(
            crate::domain::category::visibility::DOMAIN_PROPERTY,
            vec!["billing".into()],
        )),
    ]));
    let mut actor = admin_actor();
    actor.visibility = crate::domain::category::domain_visibility(&billing);
    let conn = h.base.db.conn().expect("connection");
    let not_found = |err: DomainError| {
        assert!(
            matches!(
                err,
                DomainError::NotFound {
                    resource: "declaration"
                }
            ),
            "{err:?}"
        );
    };
    let derived = || super::DerivedClassification {
        has_secret_trait: false,
        data_classification: crate::domain::declaration::DataClassification::Public,
    };
    let metadata = || crate::domain::declaration::DeclarationMetadata {
        mode: "standard".to_owned(),
        display_name: None,
        description: None,
        domain_affinity: Some("infra".to_owned()),
        licence_feature: None,
        data_classification: "public".to_owned(),
        requires_step_up: true,
        anonymous_exposable: false,
    };

    // An evolution of an active declaration.
    let mut request = h.request("domained");
    request.domain_affinity = Some("infra".to_owned());
    let created = h.create(request, &admin_actor()).await.expect("created");
    let id = created.declaration.id;
    let key = SettingKey::parse(&created.declaration.key).expect("a key");
    let mut evolving = h.request("domained");
    evolving.scope_class = "local".to_owned();
    let err = h
        .admin
        .evolve(
            &conn,
            &billing,
            &key,
            h.load(id).await,
            &[],
            &evolving,
            derived(),
            metadata(),
            &actor,
        )
        .await
        .expect_err("outside the caller's domain");
    not_found(err);
    let after = h.load(id).await;
    assert_eq!(after.status, DeclarationStatus::Active);
    assert_eq!(after.scope_class, "cascading");
    assert_eq!(h.versions_of(&after.key).await, 1, "no successor");

    // A revive of a retired one.
    let tag = etag_of(&after);
    h.retire(id, Some(tag.as_str()), &admin_actor())
        .await
        .expect("retired");
    let err = h
        .admin
        .revive(
            &conn,
            &billing,
            &key,
            h.load(id).await,
            &h.request("domained"),
            derived(),
            metadata(),
            &actor,
        )
        .await
        .expect_err("outside the caller's domain");
    not_found(err);
    assert_eq!(
        h.load(id).await.status,
        DeclarationStatus::Retired,
        "still retired"
    );
}

// ── The release's log lines ──────────────────────────────────────────────────

#[tokio::test]
#[tracing_test::traced_test]
async fn the_refused_release_logs_an_error_with_the_key_the_tenant_and_the_error_never_the_secret()
{
    let h = Harness::verified().await;
    let purged = held(2);
    for entry in &purged.secrets {
        h.secrets.seed(&entry.secret_ref, "plaintext-of-the-entry");
    }
    h.secrets.go_down();
    let key = purged.key.clone();
    let tenants: Vec<String> = purged
        .secrets
        .iter()
        .map(|e| e.tenant.to_string())
        .collect();

    let summary = h.admin.release_purged(purged, "req-error").await;
    assert_eq!((summary.released, summary.failed), (0, 2));

    logs_assert(|lines: &[&str]| {
        let errors: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("a credential store entry was not released"))
            .collect();
        if errors.len() != 2 {
            return Err(format!("one error line per refused entry: {lines:?}"));
        }
        for tenant in &tenants {
            let line = errors
                .iter()
                .find(|l| l.contains(tenant.as_str()))
                .ok_or_else(|| format!("a line carries tenant {tenant}"))?;
            if !line.contains("ERROR") || !line.contains(&key) {
                return Err(format!("level and key on the line: {line}"));
            }
            if !line.contains("the fake store is down") {
                return Err(format!("the error's log-safe text on the line: {line}"));
            }
        }
        for line in lines {
            for secret in ["plaintext-of-the-entry", "ref-0", "ref-1"] {
                if line.contains(secret) {
                    return Err(format!("the secret or its reference leaked: {line}"));
                }
            }
        }
        Ok(())
    });
}

#[tokio::test]
#[tracing_test::traced_test]
async fn the_release_logs_one_info_line_when_it_starts_and_one_when_it_ends_with_the_request_id() {
    let h = Harness::verified().await;
    let mut purged = held(3);
    purged.kept_secrets = (0..2)
        .map(|i| super::HeldSecret {
            tenant: Uuid::new_v4(),
            secret_ref: format!("kept-{i}"),
        })
        .collect();
    for entry in &purged.secrets {
        h.secrets.seed(&entry.secret_ref, "plaintext");
    }

    let summary = h.admin.release_purged(purged, "req-info-42").await;
    assert_eq!((summary.released, summary.failed), (3, 0));

    logs_assert(|lines: &[&str]| {
        let start: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("declaration purge: releasing credential store entries"))
            .collect();
        let end: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("declaration purge: credential store release finished"))
            .collect();
        if start.len() != 1 || end.len() != 1 {
            return Err(format!("one start and one end line: {lines:?}"));
        }
        for line in [start[0], end[0]] {
            if !line.contains("INFO") || !line.contains("request_id=req-info-42") {
                return Err(format!("info level and the request id: {line}"));
            }
        }
        if !start[0].contains("entries=3") || !start[0].contains("kept=2") {
            return Err(format!(
                "the start line counts entries and kept: {}",
                start[0]
            ));
        }
        if !end[0].contains("released=3") || !end[0].contains("failed=0") {
            return Err(format!("the end line counts the outcome: {}", end[0]));
        }
        Ok(())
    });
}

// ── A release that does not finish ───────────────────────────────────────────

/// A store that panics on its second release.
struct PanicsOnTheSecond {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::domain::ports::SecretManager for PanicsOnTheSecond {
    fn mint_reference(&self, _key: &str, _tenant: Uuid) -> String {
        unreachable!("a release mints nothing")
    }
    async fn store_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
        _plaintext: &Value,
    ) -> Result<(), DomainError> {
        unreachable!("a release stores nothing")
    }
    async fn resolve_plaintext(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<SecretString, DomainError> {
        unreachable!("a release reads nothing")
    }
    async fn delete_secret(
        &self,
        _key: &str,
        _tenant: Uuid,
        _secret_ref: &str,
    ) -> Result<(), DomainError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        assert!(call != 2, "the store blew up on its second release");
        Ok(())
    }
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_release_task_that_panics_counts_every_entry_it_did_not_report_as_failed() {
    let h = Harness::verified().await;
    let admin = admin_over(&h, Arc::new(PanicsOnTheSecond { calls: 0.into() }));

    let summary = admin.release_purged(held(5), "req-panic").await;

    assert_eq!(
        summary.released + summary.failed,
        5,
        "every entry is accounted for: {summary:?}"
    );
    assert_eq!(summary.released, 1, "only the first was reported");
    assert_eq!(summary.failed, 4, "the unreached remainder is failed");
    let counts = h.metrics.purge_releases.lock().expect("lock").clone();
    assert_eq!(
        counts.iter().filter(|r| **r == "ok").count(),
        1,
        "{counts:?}"
    );
    assert_eq!(
        counts.iter().filter(|r| **r == "failed").count(),
        4,
        "{counts:?}"
    );
    logs_assert(|lines: &[&str]| {
        let errors: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("the release task did not finish"))
            .collect();
        match errors.as_slice() {
            [line]
                if line.contains("ERROR")
                    && line.contains("request_id=req-panic")
                    && line.contains("entries=5")
                    && line.contains("key=gts.cf.core.settings.setting_type.v1~acme") =>
            {
                Ok(())
            }
            other => Err(format!(
                "one error line with the key, id and count: {other:?}"
            )),
        }
    });
}

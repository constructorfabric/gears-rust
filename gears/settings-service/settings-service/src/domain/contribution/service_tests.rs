// Created: 2026-09-06 by Constructor Tech
//! The reconciler, run through the SDK contract over an in-memory database.
//!
//! Every test goes through `SettingsContributionClient` — the door a gear uses
//! from its init — so the per-item transactions, the real repositories and the
//! reconcile are exercised together, the way a boot exercises them.

use std::num::NonZeroU32;
use std::sync::Arc;

use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::api::SettingsContributionClient;
use settings_service_sdk::models::{
    ContributedClassification, ContributedDeclaration, ReconcileResult, ScopeClass, SettingMode,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::category::{CategoryKey, CategoryRepository};
use crate::domain::contribution::{ContributionService, reason};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::infra::contribution_client::ContributionClient;
use crate::infra::storage::category_repo::CategoryRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::infra::type_validator::GtsTypeValidator;
use crate::test_support::{
    FakeSource, FixedScope, RecordingAudit, RecordingRegistrar, sqlite_provider,
};

const BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
const PORT: &str = "gts.cf.toolkit.settings.type_port.v1~";
const SECRET: &str = "gts.cf.toolkit.settings.type_secret_string.v1~";
const STRING: &str = "gts.cf.toolkit.settings.type_string.v1~";
const MODULE: &str = "settings-demo";

fn catalogue() -> FakeSource {
    FakeSource::default()
        .with_type(BOOL, json!({ "$id": format!("gts://{BOOL}"), "type": "boolean" }))
        .with_type(
            PORT,
            json!({ "$id": format!("gts://{PORT}"), "type": "integer", "minimum": 1, "maximum": 65535 }),
        )
        .with_type(
            SECRET,
            json!({
                "$id": format!("gts://{SECRET}"),
                "type": "string",
                "x-gts-traits": { "secret": true }
            }),
        )
        .with_type(STRING, json!({ "$id": format!("gts://{STRING}"), "type": "string" }))
}

struct Harness {
    db: Arc<DBProvider<DbError>>,
    client: ContributionClient<DeclarationRepo, CategoryRepo, ValueRepo, Arc<RecordingAudit>>,
    audit: Arc<RecordingAudit>,
    registrar: Arc<RecordingRegistrar>,
}

impl Harness {
    async fn new() -> Self {
        Self::with_registrar(RecordingRegistrar::default()).await
    }

    async fn with_registrar(registrar: RecordingRegistrar) -> Self {
        let db = sqlite_provider().await;
        let audit = Arc::new(RecordingAudit::default());
        let registrar = Arc::new(registrar);
        let service = Arc::new(ContributionService::new(
            DeclarationRepo,
            CategoryRepo,
            ValueRepo,
            Arc::new(GtsTypeValidator::new(catalogue())),
            Arc::clone(&registrar) as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
            Arc::clone(&audit),
            Arc::new(FixedScope(Uuid::nil())),
        ));
        let client = ContributionClient::new(
            Arc::clone(&db),
            service,
            Arc::new(crate::domain::resolution::EffectiveCache::new(
                std::time::Duration::from_secs(30),
            )),
        );
        Self {
            db,
            client,
            audit,
            registrar,
        }
    }

    async fn register(&self, declarations: Vec<ContributedDeclaration>) -> ReconcileResult {
        self.client
            .register_declarations(
                &SecurityContext::anonymous(),
                MODULE.to_owned(),
                declarations,
            )
            .await
            .expect("register succeeds")
    }

    async fn register_as(
        &self,
        module: &str,
        declarations: Vec<ContributedDeclaration>,
    ) -> ReconcileResult {
        self.client
            .register_declarations(
                &SecurityContext::anonymous(),
                module.to_owned(),
                declarations,
            )
            .await
            .expect("register succeeds")
    }

    async fn retire_as(&self, module: &str, keys: Vec<SettingKey>) -> ReconcileResult {
        self.client
            .retire_declarations(&SecurityContext::anonymous(), module.to_owned(), keys)
            .await
            .expect("retire succeeds")
    }

    async fn stored(&self, key: &SettingKey) -> Option<Declaration> {
        let conn = self.db.conn().expect("connection");
        DeclarationRepo
            .find_by_key(&conn, &AccessScope::allow_all(), key.as_str())
            .await
            .expect("lookup")
    }

    async fn category_exists(&self, slug: &str) -> bool {
        let conn = self.db.conn().expect("connection");
        CategoryRepo
            .find_by_key(
                &conn,
                &AccessScope::allow_all(),
                &CategoryKey::parse(slug).expect("slug"),
            )
            .await
            .expect("lookup")
            .is_some()
    }
}

fn key(category: &str, name: &str, major: u32) -> SettingKey {
    SettingKey::contributed(
        "cf",
        "settings_demo",
        category,
        name,
        NonZeroU32::new(major).expect("non-zero major"),
    )
    .expect("well-formed key")
}

fn flag(category: &str, name: &str) -> ContributedDeclaration {
    ContributedDeclaration::new(
        key(category, name, 1),
        BOOL.to_owned(),
        json!(false),
        ScopeClass::Cascading,
    )
}

fn port(name: &str, default: Value) -> ContributedDeclaration {
    ContributedDeclaration::new(
        key("network", name, 1),
        PORT.to_owned(),
        default,
        ScopeClass::Global,
    )
}

fn codes(result: &ReconcileResult) -> Vec<&str> {
    result.errors.iter().map(|e| e.code.as_str()).collect()
}

#[tokio::test]
async fn a_fresh_set_registers_every_declaration_and_its_categories() {
    let h = Harness::new().await;
    let result = h
        .register(vec![
            flag("network", "proxy_enabled"),
            port("listen_port", json!(8080)),
            flag("limits", "strict"),
        ])
        .await;

    assert_eq!(
        (
            result.registered,
            result.updated,
            result.retired,
            result.reactivated
        ),
        (3, 0, 0, 0)
    );
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // Categories are vivified from the key's third segment, once each.
    assert!(h.category_exists("network").await);
    assert!(h.category_exists("limits").await);
    // The type is registered before the row exists, once per declaration.
    let registered = h.registrar.registered.lock().expect("lock").clone();
    assert_eq!(registered.len(), 3);
    assert!(
        registered
            .iter()
            .any(|(k, t)| k == key("network", "listen_port", 1).as_str() && t == PORT)
    );
    // One record per changed row, with the module as the actor.
    assert_eq!(h.audit.operations(), vec!["create"; 3]);
    let stored = h
        .stored(&key("network", "proxy_enabled", 1))
        .await
        .expect("row");
    assert_eq!(stored.source, "module_contributed");
    assert_eq!(stored.owner_module.as_deref(), Some(MODULE));
    assert_eq!(stored.status, "active");
    assert_eq!(stored.data_classification, "public");
    assert!(!stored.has_secret_trait);
    assert!(
        stored.requires_step_up,
        "step-up is the default until the caller opts out"
    );
}

#[tokio::test]
async fn a_second_boot_with_the_same_set_changes_nothing() {
    // Idempotence is the whole point of a reconcile: a restart converges.
    let h = Harness::new().await;
    let set = vec![
        flag("network", "proxy_enabled"),
        port("listen_port", json!(8080)),
    ];
    h.register(set.clone()).await;
    let before = h.stored(&key("network", "listen_port", 1)).await;

    let result = h.register(set).await;

    assert_eq!(
        (
            result.registered,
            result.updated,
            result.retired,
            result.reactivated
        ),
        (0, 0, 0, 0)
    );
    assert!(result.errors.is_empty());
    assert_eq!(h.stored(&key("network", "listen_port", 1)).await, before);
    assert_eq!(
        h.audit.operations().len(),
        2,
        "no record for a boot that changed nothing"
    );
}

#[tokio::test]
async fn changed_metadata_is_updated_in_place() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;

    let mut changed = flag("network", "proxy_enabled");
    changed.description = Some("Route egress through the proxy".to_owned());
    changed.mode = Some(SettingMode::Advanced);
    changed.requires_step_up = Some(false);
    let result = h.register(vec![changed]).await;

    assert_eq!(result.updated, 1);
    let stored = h
        .stored(&key("network", "proxy_enabled", 1))
        .await
        .expect("row");
    assert_eq!(
        stored.description.as_deref(),
        Some("Route egress through the proxy")
    );
    assert_eq!(stored.mode, "advanced");
    assert!(!stored.requires_step_up);
    assert_eq!(h.audit.operations().last().copied(), Some("change"));
}

#[tokio::test]
async fn a_retype_at_the_same_major_is_refused_and_the_row_kept() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;

    let mut retyped = flag("network", "proxy_enabled");
    retyped.value_type_id = STRING.to_owned();
    retyped.default_value = json!("off");
    let result = h.register(vec![retyped]).await;

    assert_eq!(codes(&result), vec![reason::VALUE_TYPE_CHANGED]);
    assert_eq!(
        result.errors[0].key,
        key("network", "proxy_enabled", 1).to_string()
    );
    let stored = h
        .stored(&key("network", "proxy_enabled", 1))
        .await
        .expect("row");
    assert_eq!(stored.value_type_id, BOOL);
    assert_eq!(stored.default_value, json!(false));
}

#[tokio::test]
async fn a_changed_default_or_scope_class_at_the_same_major_is_refused() {
    let h = Harness::new().await;
    h.register(vec![port("listen_port", json!(8080))]).await;

    let result = h.register(vec![port("listen_port", json!(9090))]).await;
    assert_eq!(codes(&result), vec![reason::BEHAVIOR_AFFECTING_CHANGE]);

    let mut rescoped = port("listen_port", json!(8080));
    rescoped.scope_class = ScopeClass::Local;
    let result = h.register(vec![rescoped]).await;
    assert_eq!(codes(&result), vec![reason::BEHAVIOR_AFFECTING_CHANGE]);

    let stored = h
        .stored(&key("network", "listen_port", 1))
        .await
        .expect("row");
    assert_eq!(
        (stored.default_value, stored.scope_class.as_str()),
        (json!(8080), "global")
    );
}

#[tokio::test]
async fn an_invalid_default_is_refused_with_field_detail() {
    let h = Harness::new().await;
    let result = h.register(vec![port("listen_port", json!(70000))]).await;

    assert_eq!(codes(&result), vec![reason::DEFAULT_INVALID]);
    assert!(
        result.errors[0].message.contains("value"),
        "{}",
        result.errors[0].message
    );
    assert!(h.stored(&key("network", "listen_port", 1)).await.is_none());
    assert!(
        !h.category_exists("network").await,
        "a refused item vivifies nothing"
    );
}

#[tokio::test]
async fn an_unknown_value_type_is_refused() {
    let h = Harness::new().await;
    let unknown = ContributedDeclaration::new(
        key("network", "mystery", 1),
        "gts.cf.toolkit.settings.type_nope.v1~".to_owned(),
        json!(1),
        ScopeClass::Global,
    );
    let result = h.register(vec![unknown]).await;
    assert_eq!(codes(&result), vec![reason::VALUE_TYPE_UNKNOWN]);
}

#[tokio::test]
async fn a_secret_type_is_classified_secret_and_takes_only_an_empty_default() {
    let h = Harness::new().await;
    let mut token = ContributedDeclaration::new(
        key("security", "api_token", 1),
        SECRET.to_owned(),
        json!(""),
        ScopeClass::Cascading,
    );
    token.data_classification = Some(ContributedClassification::Public);
    let result = h.register(vec![token]).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let stored = h
        .stored(&key("security", "api_token", 1))
        .await
        .expect("row");
    assert_eq!(
        stored.data_classification, "secret",
        "secret comes from the trait, never the caller"
    );
    assert!(stored.has_secret_trait);

    let leaked = ContributedDeclaration::new(
        key("security", "other_token", 1),
        SECRET.to_owned(),
        json!("hunter2"),
        ScopeClass::Cascading,
    );
    let result = h.register(vec![leaked]).await;
    assert_eq!(codes(&result), vec![reason::SECRET_DEFAULT_NOT_EMPTY]);

    let mut conflicted = ContributedDeclaration::new(
        key("security", "third_token", 1),
        SECRET.to_owned(),
        json!(""),
        ScopeClass::Cascading,
    );
    conflicted.data_classification = Some(ContributedClassification::Pii);
    let result = h.register(vec![conflicted]).await;
    assert_eq!(codes(&result), vec![reason::CLASSIFICATION_CONFLICT]);
}

#[tokio::test]
async fn a_sensitive_setting_cannot_be_anonymous_exposable() {
    let h = Harness::new().await;
    let mut email = ContributedDeclaration::new(
        key("notifications", "support_email", 1),
        STRING.to_owned(),
        json!(""),
        ScopeClass::Global,
    );
    email.data_classification = Some(ContributedClassification::Pii);
    email.anonymous_exposable = Some(true);
    let result = h.register(vec![email]).await;
    assert_eq!(codes(&result), vec![reason::EXPOSABLE_NOT_SENSITIVE]);

    let mut banner = ContributedDeclaration::new(
        key("notifications", "banner", 1),
        STRING.to_owned(),
        json!(""),
        ScopeClass::Global,
    );
    banner.anonymous_exposable = Some(true);
    let result = h.register(vec![banner]).await;
    assert!(result.errors.is_empty());
    assert!(
        h.stored(&key("notifications", "banner", 1))
            .await
            .expect("row")
            .anonymous_exposable
    );
}

#[tokio::test]
async fn one_refused_item_does_not_block_the_rest() {
    let h = Harness::new().await;
    let result = h
        .register(vec![
            flag("network", "proxy_enabled"),
            port("listen_port", json!(0)),
            flag("limits", "strict"),
        ])
        .await;

    assert_eq!(result.registered, 2);
    assert_eq!(codes(&result), vec![reason::DEFAULT_INVALID]);
    assert!(
        h.stored(&key("network", "proxy_enabled", 1))
            .await
            .is_some()
    );
    assert!(h.stored(&key("limits", "strict", 1)).await.is_some());
    assert!(h.stored(&key("network", "listen_port", 1)).await.is_none());
}

#[tokio::test]
async fn retire_then_re_register_revives_the_same_row() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;
    let original = h
        .stored(&key("network", "proxy_enabled", 1))
        .await
        .expect("row");

    let result = h
        .retire_as(MODULE, vec![key("network", "proxy_enabled", 1)])
        .await;
    assert_eq!(result.retired, 1);
    assert_eq!(
        h.stored(&key("network", "proxy_enabled", 1))
            .await
            .expect("row")
            .status,
        "retired"
    );

    // Retiring again counts nothing and is not an error.
    let result = h
        .retire_as(MODULE, vec![key("network", "proxy_enabled", 1)])
        .await;
    assert_eq!((result.retired, result.errors.len()), (0, 0));

    let result = h.register(vec![flag("network", "proxy_enabled")]).await;
    assert_eq!(result.reactivated, 1);
    let revived = h
        .stored(&key("network", "proxy_enabled", 1))
        .await
        .expect("row");
    assert_eq!(revived.id, original.id, "revived in place, not re-minted");
    assert_eq!(revived.status, "active");
    assert_eq!(h.audit.operations(), vec!["create", "remove", "change"]);
    // Type registration happened once: the retirement left the type in place.
    assert_eq!(h.registrar.registered.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn retire_refuses_unknown_keys_and_other_modules_keys() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;

    let result = h
        .retire_as(
            "someone-else",
            vec![
                key("network", "proxy_enabled", 1),
                key("network", "ghost", 1),
            ],
        )
        .await;

    assert_eq!(result.retired, 0);
    assert_eq!(codes(&result), vec![reason::NOT_OWNER, reason::NOT_FOUND]);
    assert_eq!(
        h.stored(&key("network", "proxy_enabled", 1))
            .await
            .expect("row")
            .status,
        "active"
    );
}

#[tokio::test]
async fn another_module_cannot_take_over_a_key() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;

    let mut theirs = flag("network", "proxy_enabled");
    theirs.description = Some("mine now".to_owned());
    let result = h.register_as("someone-else", vec![theirs]).await;

    assert_eq!(codes(&result), vec![reason::NOT_OWNER]);
    assert_eq!(
        h.stored(&key("network", "proxy_enabled", 1))
            .await
            .expect("row")
            .description,
        None
    );
}

#[tokio::test]
async fn a_higher_major_is_refused_until_the_upgrade_exists_and_a_lower_one_always() {
    let h = Harness::new().await;
    h.register(vec![flag("network", "proxy_enabled")]).await;

    let v2 = ContributedDeclaration::new(
        key("network", "proxy_enabled", 2),
        STRING.to_owned(),
        json!("off"),
        ScopeClass::Cascading,
    );
    let result = h.register(vec![v2]).await;
    assert_eq!(codes(&result), vec![reason::UPGRADE_UNSUPPORTED]);
    assert!(
        h.stored(&key("network", "proxy_enabled", 2))
            .await
            .is_none()
    );

    let v3 = ContributedDeclaration::new(
        key("limits", "quota", 3),
        PORT.to_owned(),
        json!(10),
        ScopeClass::Global,
    );
    h.register(vec![v3]).await;
    let v1 = ContributedDeclaration::new(
        key("limits", "quota", 1),
        PORT.to_owned(),
        json!(10),
        ScopeClass::Global,
    );
    let result = h.register(vec![v1]).await;
    assert_eq!(codes(&result), vec![reason::MAJOR_REGRESSION]);
}

#[tokio::test]
async fn a_registry_failure_rolls_the_item_back() {
    let h = Harness::with_registrar(RecordingRegistrar {
        fail: true,
        ..RecordingRegistrar::default()
    })
    .await;

    let err = h
        .client
        .register_declarations(
            &SecurityContext::anonymous(),
            MODULE.to_owned(),
            vec![flag("network", "proxy_enabled")],
        )
        .await
        .expect_err("an unreachable registry is a failure, not a refusal");

    assert!(
        matches!(
            err,
            toolkit_canonical_errors::CanonicalError::ServiceUnavailable { .. }
        ),
        "{err:?}"
    );
    assert!(
        h.stored(&key("network", "proxy_enabled", 1))
            .await
            .is_none()
    );
    // The category was vivified in the same transaction, so it is gone too.
    assert!(!h.category_exists("network").await);
    assert!(h.audit.operations().is_empty());
}

//! The machine path over the resolution harness: authorized, audited, the
//! declaration's empty default handed out as an ordinary value, never plaintext
//! anywhere else.

use std::sync::Arc;

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use settings_service_sdk::SecretHandle;
use toolkit_db::secure::SecureUpdateExt;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::SecretResolver;
use crate::audit::{AuditOperation, AuditValue};
use crate::domain::error::DomainError;
use crate::domain::ports::SecretResolveGate;
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::secrets::issue_handle;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::entity::setting_value::{self, Entity as ValueEntity};
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{
    AllowAllGate, DenyAllGate, FailingSink, RecordingAudit, RecordingSecrets, ResolutionHarness,
    SECRET,
};

type Resolver = SecretResolver<DeclarationRepo, ValueRepo, AccessRepo, Arc<RecordingAudit>>;

struct Harness {
    base: ResolutionHarness,
    secrets: Arc<RecordingSecrets>,
    audit: Arc<RecordingAudit>,
    resolver: Resolver,
}

impl Harness {
    async fn new(gate: Arc<dyn SecretResolveGate>) -> Self {
        let base = ResolutionHarness::new().await;
        let secrets = Arc::new(RecordingSecrets::default());
        let audit = Arc::new(RecordingAudit::default());
        let resolver = SecretResolver::new(
            Arc::clone(&base.resolver),
            Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
            gate,
            Arc::clone(&audit),
        );
        Self {
            base,
            secrets,
            audit,
            resolver,
        }
    }

    async fn declare_secret(&self, name: &str) -> Uuid {
        self.base
            .declare_typed(name, scope_class::CASCADING, json!(""), SECRET, "secret")
            .await
    }

    /// Configure a credential at `tenant` the way a completed set leaves it:
    /// the row with the reference, the store with the plaintext.
    async fn configure(&self, declaration: Uuid, tenant: Uuid, plaintext: &str) -> String {
        let reference = format!("seeded-{tenant}");
        self.secrets.seed(&reference, plaintext);
        self.base.set_secret(declaration, tenant, &reference).await;
        reference
    }

    fn handle(&self, name: &str, tenant: Uuid) -> SecretHandle {
        issue_handle(self.base.key(name).as_str(), &format!("/tenants/{tenant}"))
    }

    async fn resolve(&self, handle: &SecretHandle) -> Result<SecretString, DomainError> {
        let conn = self.base.db.conn().expect("connection");
        self.resolver.resolve(&conn, &caller(), handle).await
    }
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5e57))
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .expect("context")
}

#[tokio::test]
async fn an_authorized_caller_gets_the_plaintext_and_one_masked_secret_use_record() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "hunter2").await;

    // Inherited at `b`: the entry lives in `a`, the record names `b`.
    let plaintext = h
        .resolve(&h.handle("api_token", t.b))
        .await
        .expect("resolved");
    assert_eq!(plaintext.expose_secret(), "hunter2");

    let records = h.audit.records.lock().expect("lock");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.operation, AuditOperation::SecretUse);
    assert_eq!(record.tenant_id, Some(t.b));
    assert_eq!(record.actor, Uuid::from_u128(0x5e57).to_string());
    assert_eq!(record.post_image, Some(AuditValue::Masked));
    assert!(record.pre_image.is_none());
    let json = serde_json::to_string(&*records).expect("json");
    assert!(!json.contains("hunter2"));
}

#[tokio::test]
async fn a_credential_rotated_after_this_replica_cached_the_old_reference_resolves_to_the_new_plaintext()
 {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    let old_reference = h.configure(d, t.a, "old-credential").await;
    // This replica's cache learns the old reference.
    h.base
        .resolve("api_token", ScopeTarget::Tenant(t.a))
        .await
        .expect("warms the cache");

    // The rotation lands in the store; the invalidation has not reached this
    // replica, so nothing evicts the entry.
    let new_reference = "rotated-reference";
    h.secrets.seed(new_reference, "new-credential");
    let conn = h.base.db.conn().expect("connection");
    ValueEntity::update_many()
        .col_expr(
            setting_value::Column::SecretRef,
            Expr::value(new_reference.to_owned()),
        )
        .filter(setting_value::Column::DeclarationId.eq(d))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("rotated row");

    let plaintext = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect("resolved");
    assert_eq!(plaintext.expose_secret(), "new-credential");

    // The fresh read never populates the cache: it still holds the old one.
    let cached = h
        .base
        .cache
        .get(h.base.key("api_token").as_str(), t.a)
        .expect("still cached");
    assert_eq!(cached.value, json!(old_reference));
}

#[tokio::test]
async fn the_cache_holds_the_reference_and_never_the_plaintext() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    let reference = h.configure(d, t.a, "hunter2").await;
    h.resolve(&h.handle("api_token", t.a))
        .await
        .expect("resolved");
    let effective = h
        .base
        .resolve("api_token", ScopeTarget::Tenant(t.a))
        .await
        .expect("resolves");
    assert_eq!(effective.value, json!(reference));
    assert!(effective.secret_backed);
}

#[tokio::test]
async fn a_denied_caller_is_unauthorized_before_the_store_or_the_audit_store_is_touched() {
    let gate = Arc::new(DenyAllGate::default());
    let h = Harness::new(Arc::clone(&gate) as Arc<dyn SecretResolveGate>).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "hunter2").await;
    h.secrets.go_down();

    let err = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
    assert_eq!(gate.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(h.audit.records.lock().expect("lock").is_empty());

    // A secret at its default looks the same to a denied caller: the same
    // denial, asked first, and neither the store nor the audit store touched.
    h.declare_secret("other_token").await;
    let at_default = h
        .resolve(&h.handle("other_token", t.a))
        .await
        .expect_err("denied");
    assert_eq!(format!("{at_default:?}"), format!("{err:?}"));
    assert_eq!(gate.asked.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(h.audit.records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_secret_at_its_default_resolves_to_the_empty_value_without_asking_the_store() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    h.declare_secret("api_token").await;
    let t = &h.base.tree;
    // A store that cannot answer: the default's value is resolved without it.
    h.secrets.go_down();

    let plaintext = h
        .resolve(&h.handle("api_token", t.b))
        .await
        .expect("the default is an ordinary value");
    assert_eq!(plaintext.expose_secret(), "");
    // The one text rule both sides of an activation report hash.
    assert_eq!(
        plaintext.expose_secret(),
        crate::domain::activation::plaintext_text(&json!("")).expose_secret()
    );

    // Recorded like any resolution: one `secret_use`, the value masked,
    // nothing telling it apart from a credential's.
    let records = h.audit.records.lock().expect("lock");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.operation, AuditOperation::SecretUse);
    assert_eq!(record.tenant_id, Some(t.b));
    assert_eq!(record.actor, Uuid::from_u128(0x5e57).to_string());
    assert_eq!(record.post_image, Some(AuditValue::Masked));
    assert!(record.pre_image.is_none());
}

#[tokio::test]
async fn a_null_default_resolves_to_the_empty_text_not_the_text_null() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    // A nullable secret type's placeholder (a module-registered type may admit
    // `null`); the harness stores it as declared.
    h.base
        .declare_typed(
            "nullable_token",
            scope_class::CASCADING,
            json!(null),
            SECRET,
            "secret",
        )
        .await;
    let d = h.declare_secret("configured").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "null").await;

    let at_default = h
        .resolve(&h.handle("nullable_token", t.b))
        .await
        .expect("the default is an ordinary value");
    assert_eq!(
        at_default.expose_secret(),
        "",
        "no credential is configured: the consumer sees an empty one"
    );
    // A credential whose text is `null` is that text, told apart from it.
    let configured = h
        .resolve(&h.handle("configured", t.a))
        .await
        .expect("resolved");
    assert_eq!(configured.expose_secret(), "null");

    // Each resolution recorded as any other: one masked `secret_use`.
    let records = h.audit.records.lock().expect("lock");
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|r| r.operation == AuditOperation::SecretUse
                && r.post_image == Some(AuditValue::Masked))
    );
    assert_eq!(records[0].tenant_id, Some(t.b));
}

#[tokio::test]
async fn a_default_resolution_records_the_same_shape_as_a_credentials() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("configured").await;
    h.declare_secret("at_default").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "hunter2").await;
    h.resolve(&h.handle("configured", t.a))
        .await
        .expect("resolved");
    h.resolve(&h.handle("at_default", t.a))
        .await
        .expect("resolved");

    // Everything but the key, the request id and the caller's tenant (a fresh
    // one per call in this harness) is equal.
    let records = h.audit.records.lock().expect("lock");
    assert_eq!(records.len(), 2);
    let shape = |r: &crate::audit::AuditRecord| {
        format!(
            "{:?}",
            (
                r.operation,
                r.tenant_id,
                &r.actor,
                &r.actor_classification,
                r.actor_tenant_id.is_some(),
                &r.pre_image,
                &r.post_image,
                &r.outcome,
                r.change_set_id,
                r.retain_until,
            )
        )
    };
    assert_eq!(shape(&records[0]), shape(&records[1]));
}

#[tokio::test]
async fn a_winning_row_holding_no_reference_is_not_found_on_the_value() {
    // Not the default and naming nothing in the store: a row whose value is
    // inline, as one set before the declaration was classified secret.
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    h.base.set(d, t.a, json!("inline")).await;
    let err = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect_err("no reference");
    assert!(
        matches!(err, DomainError::NotFound { resource } if resource == settings_service_sdk::gts::VALUE_SCHEMA),
        "{err:?}"
    );
    assert!(h.audit.records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_configured_row_whose_entry_is_gone_is_not_found_on_the_value() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    let reference = h.configure(d, t.a, "hunter2").await;
    h.secrets.entries.lock().expect("lock").remove(&reference);
    let err = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect_err("gone");
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_store_that_cannot_answer_is_unavailable_and_nothing_is_recorded() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "hunter2").await;
    h.secrets.go_down();
    let err = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect_err("down");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    assert!(h.audit.records.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn malformed_handles_and_handles_for_public_settings_are_invalid_arguments() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    h.base
        .declare("plain", scope_class::CASCADING, json!(true))
        .await;
    let t = &h.base.tree;
    let err = h
        .resolve(&SecretHandle::new("not-a-handle"))
        .await
        .expect_err("malformed");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::SECRET_HANDLE_MALFORMED),
        "{err:?}"
    );
    let err = h
        .resolve(&h.handle("plain", t.a))
        .await
        .expect_err("not a secret");
    assert!(
        matches!(&err, DomainError::Validation { code, .. } if *code == crate::field::NOT_A_SECRET),
        "{err:?}"
    );
    let err = h
        .resolve(&h.handle("never_declared", t.a))
        .await
        .expect_err("no declaration");
    assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_retired_secret_is_retired_not_resolved() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    let d = h.declare_secret("api_token").await;
    let t = &h.base.tree;
    h.configure(d, t.a, "hunter2").await;
    h.base.retire(d).await;
    let err = h
        .resolve(&h.handle("api_token", t.a))
        .await
        .expect_err("retired");
    assert!(matches!(err, DomainError::Retired { .. }), "{err:?}");
}

#[tokio::test]
async fn a_resolution_whose_record_cannot_be_written_yields_no_plaintext() {
    // The record is what makes a plaintext read accountable: a read that
    // could not be recorded did not happen, whatever the store had already
    // answered — the plaintext is fetched and then discarded with the error.
    let base = ResolutionHarness::new().await;
    let secrets = Arc::new(RecordingSecrets::default());
    let resolver = SecretResolver::new(
        Arc::clone(&base.resolver),
        Arc::clone(&secrets) as Arc<dyn crate::domain::ports::SecretManager>,
        Arc::new(AllowAllGate),
        FailingSink,
    );
    let d = base
        .declare_typed(
            "api_token",
            scope_class::CASCADING,
            json!(""),
            SECRET,
            "secret",
        )
        .await;
    let tenant = base.tree.a;
    secrets.seed("seeded", "hunter2");
    base.set_secret(d, tenant, "seeded").await;
    let handle = issue_handle(
        base.key("api_token").as_str(),
        &format!("/tenants/{tenant}"),
    );

    let conn = base.db.conn().expect("connection");
    let err = resolver
        .resolve(&conn, &caller(), &handle)
        .await
        .expect_err("no record, no plaintext");
    assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
}

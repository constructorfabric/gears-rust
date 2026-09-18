// Created: 2026-09-07 by Constructor Tech
//! The machine path over the resolution harness: authorized, audited, never
//! the placeholder, never plaintext anywhere else.

use std::sync::Arc;

use serde_json::json;
use settings_service_sdk::SecretHandle;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::SecretResolver;
use crate::audit::{AuditOperation, AuditValue};
use crate::domain::error::DomainError;
use crate::domain::ports::SecretResolveGate;
use crate::domain::resolution::{ScopeTarget, scope_class};
use crate::domain::secrets::issue_handle;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{
    AllowAllGate, DenyAllGate, RecordingAudit, RecordingSecrets, ResolutionHarness, SECRET,
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

    async fn resolve(&self, handle: &SecretHandle) -> Result<String, DomainError> {
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
    assert_eq!(plaintext, "hunter2");

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

    // Unconfigured looks the same to a denied caller: still a denial.
    let d2 = h.declare_secret("other_token").await;
    let _ = d2;
    let err = h
        .resolve(&h.handle("other_token", t.a))
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::Unauthorized { .. }), "{err:?}");
}

#[tokio::test]
async fn an_unconfigured_secret_is_not_found_on_the_value_and_never_the_placeholder() {
    let h = Harness::new(Arc::new(AllowAllGate)).await;
    h.declare_secret("api_token").await;
    let t = &h.base.tree;
    let err = h
        .resolve(&h.handle("api_token", t.b))
        .await
        .expect_err("unconfigured");
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

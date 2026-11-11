//! The service's own plaintext fetch: the order of its three steps and what
//! each refusal leaves behind.

use std::sync::Arc;

use secrecy::ExposeSecret;
use settings_service_sdk::SettingKey;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::ServicePlaintext;
use crate::audit::{AuditOperation, AuditValue};
use crate::domain::activation::{PlaintextFetcher, PlaintextRequest};
use crate::domain::error::DomainError;
use crate::domain::ports::SecretResolveGate;
use crate::infra::storage::audit_store::AuditStore;
use crate::test_support::{
    AllowAllGate, DenyAllGate, FailingSink, RecordingSecrets, ResolutionHarness,
};

const REF: &str = "ref-1";

struct Fixture {
    harness: ResolutionHarness,
    secrets: Arc<RecordingSecrets>,
    key: SettingKey,
}

impl Fixture {
    async fn new() -> Self {
        let harness = ResolutionHarness::new().await;
        harness
            .declare(
                "pw",
                crate::domain::resolution::scope_class::CASCADING,
                serde_json::json!(""),
            )
            .await;
        let key = harness.key("pw");
        let secrets = Arc::new(RecordingSecrets::default());
        secrets
            .entries
            .lock()
            .expect("secrets lock")
            .insert(REF.to_owned(), "plaintext".to_owned());
        Self {
            harness,
            secrets,
            key,
        }
    }

    fn request(&self) -> PlaintextRequest<'_> {
        PlaintextRequest {
            declaration_id: Uuid::new_v4(),
            key: self.key.as_str(),
            audit_tenant: self.harness.tree.a,
            source_tenant: self.harness.tree.root,
            secret_ref: REF,
        }
    }

    fn over<S: crate::audit::AuditSink + 'static>(
        &self,
        gate: Arc<dyn SecretResolveGate>,
        sink: S,
    ) -> ServicePlaintext<S> {
        ServicePlaintext::with_sink(
            Arc::clone(&self.secrets) as Arc<dyn crate::domain::ports::SecretManager>,
            gate,
            Arc::clone(&self.harness.db),
            sink,
        )
    }

    async fn uses(&self) -> Vec<crate::audit::StoredAuditRecord> {
        let conn = self.harness.db.conn().expect("connection");
        AuditStore
            .history(
                &conn,
                &AccessScope::allow_all(),
                self.key.as_str(),
                self.harness.tree.a,
                &toolkit_odata::ODataQuery::default(),
            )
            .await
            .expect("history")
            .items
            .into_iter()
            .filter(|r| r.operation == AuditOperation::SecretUse)
            .collect()
    }
}

#[tokio::test]
async fn a_fetch_returns_the_plaintext_and_writes_one_masked_secret_use_record() {
    let f = Fixture::new().await;
    let fetcher = f.over(Arc::new(AllowAllGate), AuditStore);

    let plaintext = fetcher.fetch(f.request()).await.expect("fetched");

    assert_eq!(plaintext.expose_secret(), "plaintext");
    let uses = f.uses().await;
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].post_image, Some(AuditValue::Masked));
}

#[tokio::test]
async fn a_record_that_cannot_be_written_fails_the_fetch_unavailable_and_returns_no_plaintext() {
    let f = Fixture::new().await;
    let fetcher = f.over(Arc::new(AllowAllGate), FailingSink);

    let outcome = fetcher.fetch(f.request()).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{:?}",
        outcome.map(|_| "plaintext")
    );
    assert!(f.uses().await.is_empty(), "no record");
}

#[tokio::test]
async fn a_denied_decision_is_asked_before_the_store_and_writes_no_record() {
    let f = Fixture::new().await;
    let gate = Arc::new(DenyAllGate::default());
    let fetcher = f.over(Arc::clone(&gate) as Arc<dyn SecretResolveGate>, AuditStore);
    f.secrets.go_down();

    let outcome = fetcher.fetch(f.request()).await;

    // The gate's refusal, not the store's: the decision came first.
    assert!(
        matches!(outcome, Err(DomainError::Unauthorized { .. })),
        "{:?}",
        outcome.map(|_| "plaintext")
    );
    assert_eq!(gate.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(f.uses().await.is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_answer_fails_the_fetch_and_writes_no_record() {
    let f = Fixture::new().await;
    let fetcher = f.over(Arc::new(AllowAllGate), AuditStore);
    f.secrets.go_down();

    let outcome = fetcher.fetch(f.request()).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{:?}",
        outcome.map(|_| "plaintext")
    );
    assert!(f.uses().await.is_empty());
}

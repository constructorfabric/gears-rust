use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::test_util::MockCredStoreClient;
use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
use github_mirror_sdk::CredentialRef;
use tokio::sync::Semaphore;
use toolkit_security::SecurityContext;
use toolkit_utils::SecretString;
use uuid::Uuid;

use super::{ClientSettings, CredentialClients};
use crate::domain::error::DomainError;
use crate::domain::ports::credentials::GithubCredentials;
use crate::infra::github::cache::NoCache;

fn settings() -> ClientSettings {
    ClientSettings {
        api_base_url: "https://api.github.com".to_owned(),
        cache: Arc::new(NoCache),
        permits: Arc::new(Semaphore::new(8)),
        max_concurrent_requests: NonZeroUsize::new(8).unwrap(),
    }
}

fn clients_over(store: impl CredStoreClientV1 + 'static, capacity: usize) -> CredentialClients {
    CredentialClients::new(
        Arc::new(store),
        settings(),
        NonZeroUsize::new(capacity).unwrap(),
    )
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn reference(key: &str) -> CredentialRef {
    CredentialRef {
        tenant_id: Uuid::nil(),
        key: key.to_owned(),
    }
}

fn store(secrets: &[(&str, &str)]) -> MockCredStoreClient {
    MockCredStoreClient::with_secrets(
        secrets
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    )
}

/// A store that refuses the caller.
struct RefusingStore;

#[async_trait]
impl CredStoreClientV1 for RefusingStore {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        Err(CredStoreError::AccessDenied)
    }
}

#[test]
fn one_token_keeps_one_client() {
    let clients = clients_over(store(&[]), 4);

    let first = clients.client(SecretString::new("ghp_one")).unwrap();
    let again = clients.client(SecretString::new("ghp_one")).unwrap();

    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(clients.len(), 1);
}

#[test]
fn each_token_gets_its_own_client() {
    let clients = clients_over(store(&[]), 4);

    let one = clients.client(SecretString::new("ghp_one")).unwrap();
    let two = clients.client(SecretString::new("ghp_two")).unwrap();

    assert!(!Arc::ptr_eq(&one, &two));
    assert_eq!(clients.len(), 2);
}

#[test]
fn the_map_stays_within_its_bound_dropping_the_client_used_longest_ago() {
    let clients = clients_over(store(&[]), 2);
    let one = clients.client(SecretString::new("ghp_one")).unwrap();
    let _two = clients.client(SecretString::new("ghp_two")).unwrap();
    // Touch `one` so `two` is the one used longest ago.
    let _one_again = clients.client(SecretString::new("ghp_one")).unwrap();

    let _three = clients.client(SecretString::new("ghp_three")).unwrap();

    assert_eq!(clients.len(), 2);
    let one_now = clients.client(SecretString::new("ghp_one")).unwrap();
    assert!(
        Arc::ptr_eq(&one, &one_now),
        "the recently used client stays"
    );
}

#[tokio::test]
async fn two_references_to_one_token_share_its_client() {
    let clients = clients_over(store(&[("first", "ghp_same"), ("second", "ghp_same")]), 4);

    clients
        .client_for(&caller(), &reference("first"))
        .await
        .unwrap();
    clients
        .client_for(&caller(), &reference("second"))
        .await
        .unwrap();

    assert_eq!(
        clients.len(),
        1,
        "GitHub budgets per token, not per reference"
    );
}

#[tokio::test]
async fn a_key_with_no_secret_is_refused_without_a_client() {
    let clients = clients_over(store(&[]), 4);

    let err = clients
        .client_for(&caller(), &reference("missing"))
        .await
        .err()
        .expect("a missing secret must be refused");

    assert!(
        matches!(err, DomainError::Validation { ref field, ref message }
            if field == "credential_key" && message.contains("missing")),
        "{err:?}"
    );
    assert!(clients.is_empty());
}

#[tokio::test]
async fn a_malformed_key_is_refused_before_the_store_is_asked() {
    let clients = clients_over(RefusingStore, 4);

    let err = clients
        .client_for(&caller(), &reference("not a key"))
        .await
        .err()
        .expect("a malformed key must be refused");

    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "credential_key"),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_store_that_refuses_the_caller_is_a_forbidden_sync() {
    let clients = clients_over(RefusingStore, 4);

    let err = clients
        .client_for(&caller(), &reference("github"))
        .await
        .err()
        .expect("a refused read must fail");

    assert!(matches!(err, DomainError::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn an_empty_secret_holds_no_token() {
    let clients = clients_over(store(&[("github", "")]), 4);

    let err = clients
        .client_for(&caller(), &reference("github"))
        .await
        .err()
        .expect("an empty secret must be refused");

    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
}

#[tokio::test]
async fn no_error_carries_the_token() {
    let clients = clients_over(
        MockCredStoreClient::returning_raw_value(vec![0xff, 0xfe]),
        4,
    );

    let err = clients
        .client_for(&caller(), &reference("github"))
        .await
        .err()
        .expect("a secret that is not text must be refused");

    let shown = format!("{err:?} {err}");
    assert!(!shown.contains('\u{fffd}'), "{shown}");
}

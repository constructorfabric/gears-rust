// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for [`MockCredStoreClient`], the public double other gears use
//! in their own tests. Only the behaviour the mock documents is asserted: the
//! write half is a no-op that reports a placeholder validator, and `list`
//! returns the seeded references sorted, with values only when `secret` is selected.

use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::MockCredStoreClient;
use crate::{
    CredStoreClientV2, CredStoreError, CredentialPatch, CredentialWrite, Fallback, PutPrecondition,
    SecretRef, SecretValue, SecretWrite, SharingMode, Validator, WritePrecondition,
};

/// The validator every successful write of the mock reports.
fn placeholder_validator() -> Validator {
    Validator {
        id: Uuid::nil(),
        version: 1,
    }
}

/// The typed projection of the error of a result (`None` for `Ok`); the
/// success value is dropped unprinted.
fn projection<T>(result: Result<T, CanonicalError>) -> Option<CredStoreError> {
    result.err().map(CredStoreError::from)
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

fn reference(value: &str) -> SecretRef {
    SecretRef::new(value).expect("a valid reference")
}

fn write() -> CredentialWrite {
    CredentialWrite {
        secret_type: None,
        sharing: SharingMode::default(),
        fallback: Fallback::default(),
        expires_at: None,
        secret: SecretWrite::Set(SecretValue::from("new-value")),
    }
}

fn patch() -> CredentialPatch {
    CredentialPatch {
        sharing: Some(SharingMode::Tenant),
        ..CredentialPatch::default()
    }
}

fn seeded(pairs: &[(&str, &str)]) -> MockCredStoreClient {
    MockCredStoreClient::with_secrets(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    )
}

fn select(fields: &[&str]) -> ODataQuery {
    ODataQuery::new().with_select(fields.iter().map(|f| (*f).to_owned()).collect())
}

// ---------------------------------------------------------------------------
// put / patch / delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_put_create_only_reports_created() {
    let client = MockCredStoreClient::empty();

    let outcome = client
        .put(
            &ctx(),
            &reference("new-ref"),
            write(),
            PutPrecondition::CreateOnly,
        )
        .await
        .expect("put succeeds");

    assert!(outcome.created, "a create-only put is reported as a create");
    assert_eq!(outcome.validator, placeholder_validator());
}

#[tokio::test]
async fn mock_put_replace_reports_not_created() {
    let client = seeded(&[("existing-ref", "old-value")]);
    let cases: Vec<(&str, PutPrecondition)> = vec![
        ("Exists", PutPrecondition::Exists),
        ("Matches", PutPrecondition::Matches(placeholder_validator())),
    ];

    for (name, precondition) in cases {
        let outcome = client
            .put(&ctx(), &reference("existing-ref"), write(), precondition)
            .await
            .expect("put succeeds");

        assert!(
            !outcome.created,
            "{name}: a replacing put is not reported as a create"
        );
        assert_eq!(outcome.validator, placeholder_validator(), "{name}");
    }
}

#[tokio::test]
async fn mock_patch_returns_a_validator() {
    let client = seeded(&[("existing-ref", "old-value")]);
    let cases: Vec<(&str, WritePrecondition)> = vec![
        ("Exists", WritePrecondition::Exists),
        (
            "Matches",
            WritePrecondition::Matches {
                id: Uuid::nil(),
                version: 1,
            },
        ),
    ];

    for (name, precondition) in cases {
        let validator = client
            .patch(&ctx(), &reference("existing-ref"), patch(), precondition)
            .await
            .expect("patch succeeds");

        assert_eq!(validator, placeholder_validator(), "{name}");
    }
}

/// In the expired-secret behavior only the secret read fails; the record stays
/// readable and writes (a renewal is a `put` or a `patch`) are accepted.
#[tokio::test]
async fn mock_write_result_secret_expired_is_ok_for_writes() {
    let client = MockCredStoreClient::with_expired_secret();
    let key = reference("expired-ref");
    let secret_read = client.get_secret(&ctx(), &key).await;
    // Only the error is printed: a secret-bearing result never goes to a log.
    let projected = projection(secret_read);
    assert!(
        matches!(projected, Some(CredStoreError::SecretExpired)),
        "premise: the secret read reports the expiry, got error: {projected:?}"
    );

    let put = client
        .put(&ctx(), &key, write(), PutPrecondition::Exists)
        .await
        .expect("put is accepted");
    assert!(!put.created);
    assert_eq!(put.validator, placeholder_validator());

    let validator = client
        .patch(&ctx(), &key, patch(), WritePrecondition::Exists)
        .await
        .expect("patch is accepted");
    assert_eq!(validator, placeholder_validator());

    client
        .delete(&ctx(), &key, WritePrecondition::Exists)
        .await
        .expect("delete is accepted");
}

#[tokio::test]
async fn mock_always_failing_write_half_returns_internal() {
    let client = MockCredStoreClient::always_failing();
    let key = reference("any-ref");

    let put = client
        .put(&ctx(), &key, write(), PutPrecondition::CreateOnly)
        .await;
    let projected = projection(put);
    assert!(
        matches!(projected, Some(CredStoreError::Internal { .. })),
        "put: expected Internal, got: {projected:?}"
    );

    let patched = client
        .patch(&ctx(), &key, patch(), WritePrecondition::Exists)
        .await;
    let projected = projection(patched);
    assert!(
        matches!(projected, Some(CredStoreError::Internal { .. })),
        "patch: expected Internal, got: {projected:?}"
    );

    let deleted = client.delete(&ctx(), &key, WritePrecondition::Exists).await;
    let projected = projection(deleted);
    assert!(
        matches!(projected, Some(CredStoreError::Internal { .. })),
        "delete: expected Internal, got: {projected:?}"
    );
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_list_plain_mode_omits_secrets_and_sorts_by_reference() {
    // Eight references seeded out of order: the store is a hash map, so a
    // missing sort would show up as a differing order.
    let client = seeded(&[
        ("pear", "v-pear"),
        ("apple", "v-apple"),
        ("mango", "v-mango"),
        ("kiwi", "v-kiwi"),
        ("fig", "v-fig"),
        ("zebra", "v-zebra"),
        ("banana", "v-banana"),
        ("cherry", "v-cherry"),
    ]);

    let page = client
        .list(&ctx(), &ODataQuery::new())
        .await
        .expect("list succeeds");

    let references: Vec<&str> = page
        .items
        .iter()
        .map(|item| item.credential.reference.as_ref())
        .collect();
    assert_eq!(
        references,
        vec![
            "apple", "banana", "cherry", "fig", "kiwi", "mango", "pear", "zebra"
        ]
    );
    assert!(
        page.items.iter().all(|item| item.secret.is_none()),
        "a list without `secret` selected must never carry a value"
    );
}

#[tokio::test]
async fn mock_list_select_without_secret_stays_without_secrets() {
    let client = seeded(&[("alpha", "v-alpha"), ("beta", "v-beta")]);
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("reference only", vec!["reference"]),
        (
            "a field that merely starts with secret",
            vec!["reference", "secret_type"],
        ),
    ];

    for (what, fields) in cases {
        let page = client
            .list(&ctx(), &select(&fields))
            .await
            .expect("list succeeds");

        assert_eq!(page.items.len(), 2, "{what}");
        assert!(
            page.items.iter().all(|item| item.secret.is_none()),
            "{what}: a $select that does not name `secret` must not carry secrets"
        );
    }
}

#[tokio::test]
async fn mock_list_with_secret_selected_includes_secrets() {
    let client = seeded(&[("beta", "v-beta"), ("alpha", "v-alpha")]);
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("lowercase", vec!["reference", "secret"]),
        ("capitalised", vec!["reference", "Secret"]),
        ("uppercase, alone", vec!["SECRET"]),
    ];

    for (what, fields) in cases {
        let page = client
            .list(&ctx(), &select(&fields))
            .await
            .expect("list succeeds");

        let rows: Vec<(&str, Option<&[u8]>)> = page
            .items
            .iter()
            .map(|item| {
                (
                    item.credential.reference.as_ref(),
                    item.secret.as_ref().map(SecretValue::as_bytes),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("alpha", Some(b"v-alpha".as_slice())),
                ("beta", Some(b"v-beta".as_slice())),
            ],
            "{what}"
        );
        assert!(
            page.page_info.next_cursor.is_none(),
            "{what}: the mock never paginates"
        );
    }
}

#[tokio::test]
async fn mock_list_skips_invalid_references() {
    // `with_secrets` accepts any string as a key, but a reference that does
    // not parse as a `SecretRef` cannot be listed.
    let too_long = "a".repeat(256);
    let client = seeded(&[
        ("good-ref", "v-good"),
        ("bad:ref", "v-colon"),
        ("has space", "v-space"),
        ("", "v-empty"),
        (too_long.as_str(), "v-long"),
    ]);

    let page = client
        .list(&ctx(), &ODataQuery::new())
        .await
        .expect("list succeeds");

    let references: Vec<&str> = page
        .items
        .iter()
        .map(|item| item.credential.reference.as_ref())
        .collect();
    assert_eq!(references, vec!["good-ref"]);
}

#[tokio::test]
async fn mock_list_echoes_the_requested_limit() {
    let client = seeded(&[("only-ref", "v-only")]);

    let page = client
        .list(&ctx(), &ODataQuery::new().with_limit(100))
        .await
        .expect("list succeeds");

    assert_eq!(page.page_info.limit, 100);
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn mock_always_failing_list_returns_internal() {
    let client = MockCredStoreClient::always_failing();

    let result = client.list(&ctx(), &ODataQuery::new()).await;

    let projected = projection(result);
    assert!(
        matches!(projected, Some(CredStoreError::Internal { .. })),
        "expected Internal, got: {projected:?}"
    );
}

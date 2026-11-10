// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for [`CredStoreLocalClient`] and the `DomainError` →
//! canonical mapping behind it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{
    CredStoreClientV2, CredStoreError, Fallback, PatchField, PutPrecondition, SecretRef,
    SecretValue, SecretWrite, SharingMode, Validator, WritePrecondition,
};
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::{CredStoreLocalClient, DomainError, Service};
use crate::domain::ports::metrics::NoopMetrics;
use crate::domain::secret::service::ListSettings;
use crate::domain::secret::test_support::{
    FakeDir, FakePlugin, FakePluginSelector, FakeSecretRepo, catalog_type_resolver, make_ctx,
    mock_enforcer,
};

fn generic_write(value: &str) -> credstore_sdk::CredentialWrite {
    credstore_sdk::CredentialWrite {
        secret_type: Some(credstore_sdk::SecretType::generic().into()),
        sharing: SharingMode::Tenant,
        fallback: Fallback::Inherit,
        expires_at: None,
        secret: SecretWrite::Set(SecretValue::from(value)),
    }
}

fn replace_write(value: &str) -> credstore_sdk::CredentialWrite {
    credstore_sdk::CredentialWrite {
        secret_type: None,
        sharing: SharingMode::Tenant,
        fallback: Fallback::Inherit,
        expires_at: None,
        secret: SecretWrite::Set(SecretValue::from(value)),
    }
}

fn build_client(repo: Arc<FakeSecretRepo>, dir: Arc<FakeDir>) -> CredStoreLocalClient {
    let selector = Arc::new(FakePluginSelector::new(FakePlugin::new()));
    let svc = Arc::new(Service::new(
        repo,
        dir,
        mock_enforcer(),
        selector,
        catalog_type_resolver(),
        Arc::new(NoopMetrics),
        ListSettings { max_limit: 200 },
    ));
    CredStoreLocalClient::new(svc)
}

/// The in-process client answers `CanonicalError`; the opt-in
/// `CredStoreError` projection of it is what a consumer dispatches on.
fn projected(err: DomainError) -> CredStoreError {
    CredStoreError::from(CanonicalError::from(err))
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    reason = "a flat enumeration of every DomainError variant's canonical mapping and its \
              projection; splitting it would only scatter the one-to-one correspondence this \
              test is checking"
)]
fn domain_error_maps_to_canonical_and_projects() {
    assert!(matches!(
        projected(DomainError::NotFound),
        CredStoreError::NotFound
    ));
    assert!(matches!(
        projected(DomainError::Conflict),
        CredStoreError::AlreadyExists
    ));
    assert!(matches!(
        projected(DomainError::VersionConflict),
        CredStoreError::PreconditionFailed
    ));
    assert!(matches!(
        projected(DomainError::SecretExpired),
        CredStoreError::SecretExpired
    ));
    assert!(matches!(
        projected(DomainError::InvalidSecretRef {
            detail: "x".to_owned()
        }),
        CredStoreError::Invalid { .. }
    ));
    assert!(matches!(
        projected(DomainError::UnsupportedTransition {
            detail: "x".to_owned()
        }),
        CredStoreError::UnsupportedTransition { .. }
    ));
    assert!(matches!(
        projected(DomainError::AccessDenied { cause: None }),
        CredStoreError::PermissionDenied
    ));
    assert!(matches!(
        projected(DomainError::ServiceUnavailable {
            detail: "x".to_owned(),
            retry_after: Some(Duration::from_secs(1)),
            cause: None,
        }),
        CredStoreError::Unavailable {
            retry_after: Some(_),
            ..
        }
    ));
    assert!(matches!(
        projected(DomainError::internal("x")),
        CredStoreError::Internal { .. }
    ));
    assert!(matches!(
        projected(DomainError::TypeViolation {
            field: "type",
            reason: credstore_sdk::reason::aborted::TYPE_IMMUTABLE,
            detail: "x".to_owned(),
        }),
        CredStoreError::TypeConflict { .. }
    ));
    assert!(matches!(
        projected(DomainError::TypeViolation {
            field: "sharing",
            reason: credstore_sdk::field::SHARING_NOT_ALLOWED_FOR_TYPE,
            detail: "x".to_owned(),
        }),
        CredStoreError::Invalid { .. }
    ));
    assert!(matches!(
        projected(DomainError::InvalidRequest {
            field: "secret",
            reason: credstore_sdk::field::EMPTY_PATCH,
            detail: "x".to_owned(),
        }),
        CredStoreError::Invalid { .. }
    ));
}

#[tokio::test]
async fn local_client_round_trips_through_service() {
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let k = SecretRef::new("client-key").expect("ref");

    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let client = build_client(repo, dir);

    client
        .put(&ctx, &k, generic_write("v"), PutPrecondition::CreateOnly)
        .await
        .expect("create");
    // Rotation without a version in hand: the explicit LWW opt-in.
    client
        .put(&ctx, &k, replace_write("v2"), PutPrecondition::Exists)
        .await
        .expect("put");
    assert!(client.get_record(&ctx, &k).await.expect("get").is_some());
    assert!(
        client
            .get_secret(&ctx, &k)
            .await
            .expect("get_secret")
            .is_some()
    );
    client
        .delete(&ctx, &k, WritePrecondition::Exists)
        .await
        .expect("delete");
    assert!(client.get_record(&ctx, &k).await.expect("get").is_none());
}

#[tokio::test]
async fn create_only_is_create_only_put_never_creates() {
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let k = SecretRef::new("create-key").expect("ref");

    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let client = build_client(repo, dir);

    // First create succeeds.
    client
        .put(&ctx, &k, generic_write("v1"), PutPrecondition::CreateOnly)
        .await
        .expect("first create");
    // Second create-only of the same sharing class → Conflict.
    let err = client
        .put(&ctx, &k, generic_write("v2"), PutPrecondition::CreateOnly)
        .await
        .expect_err("second create conflicts");
    assert!(matches!(err, CanonicalError::AlreadyExists { .. }));
    // `put` under `Exists` replaces the existing credential (no conflict)…
    client
        .put(&ctx, &k, replace_write("v3"), PutPrecondition::Exists)
        .await
        .expect("put replaces");
    // …but never creates: on a missing reference the mandatory precondition
    // fails as an optimistic-lock failure regardless of the variant.
    let missing = SecretRef::new("create-key-missing").expect("ref");
    let err = client
        .put(&ctx, &missing, replace_write("v"), PutPrecondition::Exists)
        .await
        .expect_err("put on missing reference conflicts");
    assert!(
        matches!(err, CanonicalError::Aborted { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn precondition_guards_in_process_write_and_delete() {
    // The in-process client can now carry an optimistic-concurrency
    // precondition (the ClientHub equivalent of a REST `If-Match`), not just
    // the REST surface.
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let k = SecretRef::new("guarded-key").expect("ref");

    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let client = build_client(repo, dir);

    client
        .put(&ctx, &k, generic_write("v1"), PutPrecondition::CreateOnly)
        .await
        .expect("create");
    let observed = client
        .get_record(&ctx, &k)
        .await
        .expect("get")
        .expect("present");
    let observed_validator = observed
        .own
        .as_ref()
        .expect("own row has a strong validator")
        .validator;
    let stale = WritePrecondition::Matches {
        id: observed_validator.id,
        version: observed_validator.version,
    };

    // A guarded patch against the observed generation succeeds and bumps the
    // version, so the same validator is now stale.
    client
        .patch(
            &ctx,
            &k,
            credstore_sdk::CredentialPatch {
                secret_type: None,
                sharing: None,
                fallback: None,
                expires_at: PatchField::Absent,
                secret: PatchField::Set(SecretValue::from("v2")),
            },
            stale,
        )
        .await
        .expect("guarded patch matches current generation");

    // Re-using the stale validator is rejected as a conflict.
    let err = client
        .patch(
            &ctx,
            &k,
            credstore_sdk::CredentialPatch {
                secret_type: None,
                sharing: None,
                fallback: None,
                expires_at: PatchField::Absent,
                secret: PatchField::Set(SecretValue::from("v3")),
            },
            stale,
        )
        .await
        .expect_err("stale precondition conflicts");
    assert!(
        matches!(err, CanonicalError::Aborted { .. }),
        "got: {err:?}"
    );

    // A guarded delete: the stale validator conflicts, the current one succeeds.
    let current = client
        .get_record(&ctx, &k)
        .await
        .expect("get")
        .expect("present");
    let current_validator = current
        .own
        .as_ref()
        .expect("own row has a strong validator")
        .validator;
    let err = client
        .delete(&ctx, &k, stale)
        .await
        .expect_err("stale delete conflicts");
    assert!(
        matches!(err, CanonicalError::Aborted { .. }),
        "got: {err:?}"
    );
    client
        .delete(
            &ctx,
            &k,
            WritePrecondition::Matches {
                id: current_validator.id,
                version: current_validator.version,
            },
        )
        .await
        .expect("guarded delete matches current generation");
    assert!(client.get_record(&ctx, &k).await.expect("get").is_none());
}

#[tokio::test]
async fn patch_precondition_maps_sdk_matches_to_domain_version() {
    // Exercises `to_domain_precondition`'s `Matches` arm directly (the
    // `Exists` arm is already covered by the tests above).
    let tenant = Uuid::new_v4();
    let ctx = make_ctx(Uuid::new_v4(), tenant);
    let k = SecretRef::new("k").expect("ref");
    let repo = Arc::new(FakeSecretRepo::new());
    let dir = Arc::new(FakeDir::single(tenant));
    let client = build_client(repo, dir);

    client
        .put(&ctx, &k, generic_write("v1"), PutPrecondition::CreateOnly)
        .await
        .expect("create");
    let cred = client
        .get_record(&ctx, &k)
        .await
        .expect("get")
        .expect("present");
    let v = cred.own.as_ref().expect("some").validator;

    let out = client
        .put(
            &ctx,
            &k,
            replace_write("v2"),
            PutPrecondition::Matches(Validator {
                id: v.id,
                version: v.version,
            }),
        )
        .await
        .expect("matching put precondition");
    assert!(!out.created);
    assert_eq!(out.validator.version, v.version + 1);
}

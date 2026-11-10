// Updated: 2026-10-06 by Constructor Tech
//! Test-only [`CredStoreClientV2`] doubles, behind the `test-util` feature.
//!
//! A single configurable [`MockCredStoreClient`] covering the shapes consumers
//! exercise in tests, so each gear no longer hand-rolls its own:
//!
//! * [`MockCredStoreClient::empty`] — every `get`/`get_secret` resolves to
//!   `Ok(None)`;
//! * [`MockCredStoreClient::with_secrets`] — a keyed `(reference, value)` store;
//! * [`MockCredStoreClient::returning_raw_value`] — a fixed raw value for any
//!   reference (e.g. non-UTF-8 bytes to drive malformed-value paths);
//! * [`MockCredStoreClient::always_failing`] — every operation fails with
//!   a canonical `Internal` error.
//!
//! Only `get`/`get_secret`/`list` carry behaviour; the write half (`put`/
//! `patch`/`delete`) is a no-op that succeeds (or fails, in the
//! always-failing mode) to match, returning a placeholder validator — this
//! double is read-oriented, for consumers that only resolve credentials.
//! `list` returns every stored reference as one unfiltered, unpaginated item
//! (it does not model the `OData` allowlist or cursor semantics a real server
//! enforces).

use std::collections::HashMap;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_canonical_errors::{CanonicalError, Http, resource_error};
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::reason::failed_precondition::SECRET_EXPIRED;
use crate::{
    CredStoreClientV2, Credential, CredentialListItem, CredentialPatch, CredentialStatus,
    CredentialWrite, Fallback, InheritanceStatus, OwnRow, OwnerId, PutOutcome, PutPrecondition,
    Secret, SecretRef, SecretType, SecretValue, SharingMode, Validator, WritePrecondition,
};

#[resource_error(gts_id!("cf.core.credstore.credential.v1~"))]
struct CredentialScope;

fn backend_failure() -> CanonicalError {
    CanonicalError::internal("backend failure").create()
}

fn not_found() -> CanonicalError {
    CredentialScope::not_found("credential not found")
        .with_resource("credential")
        .create()
}

fn secret_expired() -> CanonicalError {
    CredentialScope::failed_precondition()
        .with_precondition_violation(
            "secret",
            "the credential's secret has expired; renew the credential",
            SECRET_EXPIRED,
        )
        .with_override(Http::status_code(409))
        .create()
}

enum Behavior {
    /// `get`/`get_secret` return the mapped value for a known reference, else
    /// `Ok(None)`.
    Store(HashMap<String, Vec<u8>>),
    /// `get`/`get_secret` return this raw value for *any* reference.
    AnyValue(Vec<u8>),
    /// Every operation fails with a canonical `Internal` error.
    Failing,
    /// `get`/`get_secret` fail with a canonical `NotFound` error — a client
    /// implementation that reports the not-found surface as an error instead
    /// of `Ok(None)`.
    NotFound,
    /// `get` returns the record; `get_secret` fails with
    /// a canonical `FailedPrecondition` (`SECRET_EXPIRED`) for any reference.
    SecretExpired,
}

/// Configurable in-process [`CredStoreClientV2`] test double. See the module
/// docs for the available modes.
pub struct MockCredStoreClient {
    behavior: Behavior,
}

impl MockCredStoreClient {
    /// Empty store — every `get`/`get_secret` resolves to `Ok(None)`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            behavior: Behavior::Store(HashMap::new()),
        }
    }

    /// Store seeded with `(reference, value)` pairs; an unknown reference
    /// resolves to `Ok(None)`. A leading `cred://` scheme prefix on a key is
    /// stripped, so callers using either the bare reference or the consumer's
    /// `cred://<ref>` spelling resolve identically.
    #[must_use]
    pub fn with_secrets(creds: Vec<(String, String)>) -> Self {
        let store = creds
            .into_iter()
            .map(|(k, v)| {
                let key = k.strip_prefix("cred://").unwrap_or(&k).to_owned();
                (key, v.into_bytes())
            })
            .collect();
        Self {
            behavior: Behavior::Store(store),
        }
    }

    /// Resolve *any* reference to this raw value — useful for exercising
    /// non-UTF-8 / malformed-value paths.
    #[must_use]
    pub fn returning_raw_value(value: Vec<u8>) -> Self {
        Self {
            behavior: Behavior::AnyValue(value),
        }
    }

    /// Every operation fails with a canonical `Internal` error — for
    /// error-handling paths.
    #[must_use]
    pub fn always_failing() -> Self {
        Self {
            behavior: Behavior::Failing,
        }
    }

    /// `get`/`get_secret` fail with a canonical `NotFound` error for any
    /// reference — for consumers hardening against clients that report the
    /// not-found surface as an error instead of `Ok(None)`.
    #[must_use]
    pub fn erroring_not_found() -> Self {
        Self {
            behavior: Behavior::NotFound,
        }
    }

    /// `get_secret` fails with a canonical `FailedPrecondition` (`SECRET_EXPIRED`) for any
    /// reference (and `get` returns the record) — for consumers exercising
    /// the expired-secret path.
    #[must_use]
    pub fn with_expired_secret() -> Self {
        Self {
            behavior: Behavior::SecretExpired,
        }
    }

    /// Build a canned [`Credential`] record with placeholder metadata (nil
    /// generation id, `generic` type, own/active, no expiry).
    fn credential(reference: &SecretRef) -> Credential {
        Credential {
            reference: reference.clone(),
            secret_type: SecretType::generic().gts_id().to_owned(),
            sharing: SharingMode::default(),
            inheritance: InheritanceStatus::Own,
            expires_at: None,
            own: Some(OwnRow {
                status: CredentialStatus::Active,
                fallback: Fallback::default(),
                updated_at: OffsetDateTime::UNIX_EPOCH,
                owner_id: OwnerId::nil(),
                validator: Self::validator(),
            }),
        }
    }

    /// Build a canned [`Secret`] wrapping `value` with placeholder metadata.
    fn secret(reference: &SecretRef, value: Vec<u8>) -> Secret {
        Secret {
            reference: reference.clone(),
            secret_type: SecretType::generic().gts_id().to_owned(),
            expires_at: None,
            secret: SecretValue::new(value),
            validator: Self::validator(),
        }
    }

    fn validator() -> Validator {
        Validator {
            id: Uuid::nil(),
            version: 1,
        }
    }

    fn write_result(&self) -> Result<(), CanonicalError> {
        match self.behavior {
            Behavior::Failing => Err(backend_failure()),
            Behavior::Store(_)
            | Behavior::AnyValue(_)
            | Behavior::NotFound
            | Behavior::SecretExpired => Ok(()),
        }
    }
}

#[async_trait]
impl CredStoreClientV2 for MockCredStoreClient {
    async fn get_record(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Credential>, CanonicalError> {
        match &self.behavior {
            Behavior::Store(store) => Ok(store
                .contains_key(key.as_ref())
                .then(|| Self::credential(key))),
            Behavior::AnyValue(_) | Behavior::SecretExpired => Ok(Some(Self::credential(key))),
            Behavior::Failing => Err(backend_failure()),
            Behavior::NotFound => Err(not_found()),
        }
    }

    async fn get_secret(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Secret>, CanonicalError> {
        match &self.behavior {
            Behavior::Store(store) => Ok(store
                .get(key.as_ref())
                .cloned()
                .map(|v| Self::secret(key, v))),
            Behavior::AnyValue(value) => Ok(Some(Self::secret(key, value.clone()))),
            Behavior::Failing => Err(backend_failure()),
            Behavior::NotFound => Err(not_found()),
            Behavior::SecretExpired => Err(secret_expired()),
        }
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _write: CredentialWrite,
        precondition: PutPrecondition,
    ) -> Result<PutOutcome, CanonicalError> {
        self.write_result().map(|()| PutOutcome {
            created: matches!(precondition, PutPrecondition::CreateOnly),
            validator: Self::validator(),
        })
    }

    async fn patch(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _patch: CredentialPatch,
        _precondition: WritePrecondition,
    ) -> Result<Validator, CanonicalError> {
        self.write_result().map(|()| Self::validator())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CanonicalError> {
        self.write_result()
    }

    /// Minimal double: every stored reference is returned as one item
    /// (unfiltered, unpaginated — this test double is read-oriented and does
    /// not model the `OData` allowlist, reduction, or cursor semantics a real
    /// server enforces). `secret` is populated only when `query`'s `$select`
    /// names it, mirroring the real selection switch.
    async fn list(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<CredentialListItem>, CanonicalError> {
        let limit = query.limit.unwrap_or(50);
        let with_secrets = query
            .selected_fields()
            .is_some_and(|fields| fields.iter().any(|f| f.eq_ignore_ascii_case("secret")));
        match &self.behavior {
            Behavior::Failing => Err(backend_failure()),
            Behavior::NotFound | Behavior::AnyValue(_) | Behavior::SecretExpired => {
                Ok(Page::empty(limit))
            }
            Behavior::Store(store) => {
                let mut items: Vec<CredentialListItem> = store
                    .iter()
                    .filter_map(|(k, v)| {
                        let key = SecretRef::new(k.clone()).ok()?;
                        Some(CredentialListItem {
                            credential: Self::credential(&key),
                            secret: with_secrets.then(|| SecretValue::new(v.clone())),
                        })
                    })
                    .collect();
                items.sort_by(|a, b| {
                    a.credential
                        .reference
                        .as_ref()
                        .cmp(b.credential.reference.as_ref())
                });
                Ok(Page::new(
                    items,
                    PageInfo {
                        next_cursor: None,
                        prev_cursor: None,
                        limit,
                    },
                ))
            }
        }
    }
}

// The whole module is already gated on the `test-util` feature in `lib.rs`.
#[cfg(test)]
#[path = "test_util_tests.rs"]
mod test_util_tests;

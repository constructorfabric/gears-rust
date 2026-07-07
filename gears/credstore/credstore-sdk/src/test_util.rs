//! Test-only [`CredStoreClientV1`] doubles, behind the `test-util` feature.
//!
//! A single configurable [`MockCredStoreClient`] covering the shapes consumers
//! exercise in tests, so each gear no longer hand-rolls its own:
//!
//! * [`MockCredStoreClient::empty`] — every `get` resolves to `Ok(None)`;
//! * [`MockCredStoreClient::with_secrets`] — a keyed `(reference, value)` store;
//! * [`MockCredStoreClient::returning_raw_value`] — a fixed raw value for any
//!   reference (e.g. non-UTF-8 bytes to drive malformed-value paths);
//! * [`MockCredStoreClient::always_failing`] — every operation fails with
//!   [`CredStoreError::Internal`].
//!
//! Only `get` carries behaviour; the write half is a no-op that succeeds (or
//! fails, in the always-failing mode) to match.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretType, SecretValue,
    SharingMode, TenantId, WriteOptions, WritePrecondition,
};

enum Behavior {
    /// `get` returns the mapped value for a known reference, else `Ok(None)`.
    Store(HashMap<String, Vec<u8>>),
    /// `get` returns this raw value for *any* reference.
    AnyValue(Vec<u8>),
    /// Every operation fails with [`CredStoreError::Internal`].
    Failing,
    /// `get` fails with [`CredStoreError::NotFound`] — a client implementation
    /// that reports the not-found surface as an error instead of `Ok(None)`.
    NotFound,
}

/// Configurable in-process [`CredStoreClientV1`] test double. See the module
/// docs for the available modes.
pub struct MockCredStoreClient {
    behavior: Behavior,
}

impl MockCredStoreClient {
    /// Empty store — every `get` resolves to `Ok(None)`.
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

    /// Every operation fails with [`CredStoreError::Internal`] — for
    /// error-handling paths.
    #[must_use]
    pub fn always_failing() -> Self {
        Self {
            behavior: Behavior::Failing,
        }
    }

    /// `get` fails with [`CredStoreError::NotFound`] for any reference — for
    /// consumers hardening against clients that report the not-found surface
    /// as an error instead of `Ok(None)`.
    #[must_use]
    pub fn erroring_not_found() -> Self {
        Self {
            behavior: Behavior::NotFound,
        }
    }

    /// Build a canned response wrapping `value` with placeholder metadata
    /// (nil id/tenant, `generic` type, version 1, not inherited, no expiry).
    fn response(value: Vec<u8>) -> GetSecretResponse {
        GetSecretResponse {
            value: SecretValue::new(value),
            id: uuid::Uuid::nil(),
            owner_tenant_id: TenantId::nil(),
            sharing: SharingMode::default(),
            is_inherited: false,
            version: 1,
            secret_type: SecretType::generic().gts_id().to_owned(),
            expires_at: None,
        }
    }

    fn write_result(&self) -> Result<(), CredStoreError> {
        match self.behavior {
            Behavior::Failing => Err(CredStoreError::Internal("backend failure".into())),
            Behavior::Store(_) | Behavior::AnyValue(_) | Behavior::NotFound => Ok(()),
        }
    }
}

#[async_trait]
impl CredStoreClientV1 for MockCredStoreClient {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        match &self.behavior {
            Behavior::Store(store) => Ok(store.get(key.as_ref()).cloned().map(Self::response)),
            Behavior::AnyValue(value) => Ok(Some(Self::response(value.clone()))),
            Behavior::Failing => Err(CredStoreError::Internal("backend failure".into())),
            Behavior::NotFound => Err(CredStoreError::NotFound),
        }
    }

    async fn put_opts(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.write_result()
    }

    async fn create_opts(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.write_result()
    }

    async fn delete_opts(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _precondition: Option<WritePrecondition>,
    ) -> Result<(), CredStoreError> {
        self.write_result()
    }
}

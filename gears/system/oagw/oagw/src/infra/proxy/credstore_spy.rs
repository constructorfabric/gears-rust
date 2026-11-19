//! A [`CredStoreClientV1`] spy for the per-request secret reference tests
//! (ADR-0022): secrets with their own owner tenant, sharing mode,
//! inheritance flag, secret type and visibility, and a record of every `get`
//! with the [`SecurityContext`] it ran under.
//!
//! OAGW-local on purpose: the shared `credstore_sdk::test_util` mock stays
//! as it is.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretType, SecretValue,
    SharingMode, TenantId, WriteOptions, WritePrecondition,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A secret held by [`CredStoreSpy::with_entries`], with the metadata
/// its `get` returns. Built with [`SpySecret::new`] and the setters; the
/// defaults are a nil owner, `tenant` sharing, the generic type and
/// visibility to every caller. `is_inherited` is derived per `get`, as
/// CredStore sets it: the owner is not the caller's tenant. Each entry gets a
/// fresh record id.
#[derive(Debug, Clone)]
pub struct SpySecret {
    reference: String,
    value: Vec<u8>,
    id: Uuid,
    owner_tenant_id: TenantId,
    sharing: SharingMode,
    secret_type: String,
    visible_to_tenants: Option<Vec<Uuid>>,
    visible_to_subject: Option<Uuid>,
}

impl SpySecret {
    /// A secret under `reference` (a leading `cred://` is stripped) holding
    /// `value`.
    #[must_use]
    pub fn new(reference: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        let reference = reference.into();
        Self {
            reference: strip_scheme(&reference).to_owned(),
            value: value.into(),
            id: Uuid::new_v4(),
            owner_tenant_id: TenantId::nil(),
            sharing: SharingMode::default(),
            secret_type: SecretType::generic().gts_id().to_owned(),
            visible_to_tenants: None,
            visible_to_subject: None,
        }
    }

    /// The record id `get` returns.
    #[must_use]
    pub fn id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    /// The tenant that owns the secret.
    #[must_use]
    pub fn owned_by(mut self, tenant: impl Into<Uuid>) -> Self {
        self.owner_tenant_id = TenantId(tenant.into());
        self
    }

    /// The sharing mode `get` returns.
    #[must_use]
    pub fn sharing(mut self, sharing: SharingMode) -> Self {
        self.sharing = sharing;
        self
    }

    /// The full GTS type id `get` returns.
    #[must_use]
    pub fn secret_type(mut self, secret_type: impl Into<String>) -> Self {
        self.secret_type = secret_type.into();
        self
    }

    /// Resolve only for callers whose subject tenant is one of `tenants`,
    /// as hierarchical resolution would. Without it every caller sees it.
    #[must_use]
    pub fn visible_to(mut self, tenants: impl IntoIterator<Item = Uuid>) -> Self {
        self.visible_to_tenants = Some(tenants.into_iter().collect());
        self
    }

    /// A `private` secret: sharing [`SharingMode::Private`], resolving only
    /// for the subject `subject_id`.
    #[must_use]
    pub fn private_to(mut self, subject_id: Uuid) -> Self {
        self.sharing = SharingMode::Private;
        self.visible_to_subject = Some(subject_id);
        self
    }

    fn admits(&self, ctx: &SecurityContext) -> bool {
        self.visible_to_tenants
            .as_ref()
            .is_none_or(|tenants| tenants.contains(&ctx.subject_tenant_id()))
            && self
                .visible_to_subject
                .is_none_or(|subject| subject == ctx.subject_id())
    }

    fn response(&self, ctx: &SecurityContext) -> GetSecretResponse {
        GetSecretResponse {
            value: SecretValue::new(self.value.clone()),
            id: self.id,
            owner_tenant_id: self.owner_tenant_id,
            sharing: self.sharing,
            is_inherited: self.owner_tenant_id.0 != ctx.subject_tenant_id(),
            version: 1,
            secret_type: self.secret_type.clone(),
            expires_at: None,
        }
    }
}

/// One `get` the mock answered: the reference asked for (as the caller
/// passed it) and the context it ran under.
#[derive(Debug, Clone)]
pub struct RecordedGet {
    /// The reference passed to `get`.
    pub reference: String,
    /// The security context passed to `get`.
    pub ctx: SecurityContext,
}

enum Behavior {
    /// `get` returns the first entry for the reference that admits the
    /// caller, else `Ok(None)`.
    Store(Mutex<Vec<SpySecret>>),
    /// Every operation fails with [`CredStoreError::Internal`].
    Failing,
    /// `get` never completes.
    Pending,
    /// Every operation fails with the error the function builds.
    Error(fn() -> CredStoreError),
}

/// Configurable in-process [`CredStoreClientV1`] test double. See the module
/// docs for the available modes.
pub struct CredStoreSpy {
    behavior: Behavior,
    /// How long each `get` waits before it answers.
    get_delay: Option<Duration>,
    gets: Mutex<Vec<RecordedGet>>,
}

fn strip_scheme(reference: &str) -> &str {
    reference.strip_prefix("cred://").unwrap_or(reference)
}

impl CredStoreSpy {
    fn with_behavior(behavior: Behavior) -> Self {
        Self {
            behavior,
            get_delay: None,
            gets: Mutex::new(Vec::new()),
        }
    }

    /// Empty store — every `get` resolves to `Ok(None)`.
    #[must_use]
    pub fn empty() -> Self {
        Self::with_entries(Vec::new())
    }

    /// Store seeded with `entries`. `get` returns the first entry under the
    /// reference that admits the caller ([`SpySecret::visible_to`],
    /// [`SpySecret::private_to`]), so list a tenant's own secret before an
    /// ancestor's shared one of the same name to model hierarchical
    /// resolution.
    #[must_use]
    pub fn with_entries(entries: Vec<SpySecret>) -> Self {
        Self::with_behavior(Behavior::Store(Mutex::new(entries)))
    }

    /// Every operation fails with [`CredStoreError::Internal`] — for
    /// error-handling paths.
    #[must_use]
    pub fn always_failing() -> Self {
        Self::with_behavior(Behavior::Failing)
    }

    /// `get` never completes — for timeout paths. Writes succeed.
    #[must_use]
    pub fn never_answering() -> Self {
        Self::with_behavior(Behavior::Pending)
    }

    /// Every operation fails with the error `error` builds — for the
    /// mapping of each [`CredStoreError`] kind.
    #[must_use]
    pub fn failing_with(error: fn() -> CredStoreError) -> Self {
        Self::with_behavior(Behavior::Error(error))
    }

    /// Each `get` waits `delay` before it answers — for deadline paths.
    #[must_use]
    pub fn answering_after(mut self, delay: Duration) -> Self {
        self.get_delay = Some(delay);
        self
    }

    /// Add `secret` after the existing entries of a store built with
    /// [`Self::with_secrets`], [`Self::with_entries`] or [`Self::empty`].
    /// Returns `false`, adding nothing, when the mock is not a store.
    pub fn add_secret(&self, secret: SpySecret) -> bool {
        self.entries().map(|mut e| e.push(secret)).is_some()
    }

    /// Replace the value of every entry under `reference` (rotation). Returns
    /// how many entries changed (none when the mock is not a store).
    pub fn set_value(&self, reference: &str, value: impl Into<Vec<u8>>) -> usize {
        let reference = strip_scheme(reference);
        let value = value.into();
        let Some(mut entries) = self.entries() else {
            return 0;
        };
        let mut changed = 0;
        for entry in entries.iter_mut().filter(|e| e.reference == reference) {
            entry.value.clone_from(&value);
            changed += 1;
        }
        changed
    }

    /// Every `get` so far, in order, with the context it ran under.
    #[must_use]
    pub fn gets(&self) -> Vec<RecordedGet> {
        self.recorded().clone()
    }

    /// Forget the recorded `get`s.
    pub fn clear_gets(&self) {
        self.recorded().clear();
    }

    fn recorded(&self) -> MutexGuard<'_, Vec<RecordedGet>> {
        self.gets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn entries(&self) -> Option<MutexGuard<'_, Vec<SpySecret>>> {
        match &self.behavior {
            Behavior::Store(entries) => {
                Some(entries.lock().unwrap_or_else(PoisonError::into_inner))
            }
            Behavior::Failing | Behavior::Pending | Behavior::Error(_) => None,
        }
    }

    fn write_result(&self) -> Result<(), CredStoreError> {
        match self.behavior {
            Behavior::Failing => Err(CredStoreError::Internal("backend failure".into())),
            Behavior::Error(error) => Err(error()),
            Behavior::Store(_) | Behavior::Pending => Ok(()),
        }
    }
}

#[async_trait]
impl CredStoreClientV1 for CredStoreSpy {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        self.recorded().push(RecordedGet {
            reference: key.as_ref().to_owned(),
            ctx: ctx.clone(),
        });
        if let Some(delay) = self.get_delay {
            tokio::time::sleep(delay).await;
        }
        match &self.behavior {
            Behavior::Store(entries) => Ok(entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|e| e.reference == key.as_ref() && e.admits(ctx))
                .map(|e| e.response(ctx))),
            Behavior::Failing => Err(CredStoreError::Internal("backend failure".into())),
            Behavior::Error(error) => Err(error()),
            Behavior::Pending => std::future::pending().await,
        }
    }

    async fn put_opts(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
        _precondition: WritePrecondition,
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

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        self.write_result()
    }
}

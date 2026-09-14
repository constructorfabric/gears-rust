//! Test stub for the service-account half of the [`IdpPluginClient`]
//! contract.
//!
//! Pairs with [`FakeServiceAccountOutcome`], which drives the per-call
//! outcome independently for provision / rotate / revoke / list. Tests
//! configure the desired outcome via the `set_*_outcome` helpers, then
//! exercise
//! [`crate::domain::service_account::service::ServiceAccountService`]
//! against the fake to pin contract behaviour without a real provider.
//!
//! The failure arms deliberately carry credential-shaped `detail` text
//! (see [`ADAPTER_DETAIL`]): the service-layer tests assert that this
//! string reaches neither the mapped [`crate::domain::error::DomainError`]
//! nor any log record, which is the whole point of the discard-not-digest
//! posture on this surface.
//!
//! State lives behind `Arc<Mutex<...>>` so the fake is `Send + Sync` and
//! can be shared across tasks, matching `FakeIdpUserProvisioner`.

#![allow(
    dead_code,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::expect_used,
    reason = "test-support fake; canonical mutex-locking pattern with helper getters that not every test exercises today"
)]

use std::sync::Mutex;

use account_management_sdk::{
    IdpListServiceAccountsRequest, IdpPluginClient, IdpProvisionServiceAccountRequest,
    IdpRevokeServiceAccountRequest, IdpRotateServiceAccountSecretRequest,
    IdpServiceAccountCredentials, IdpServiceAccountFailure, IdpServiceAccountSummary,
};
use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Credential-shaped adapter `detail` every failure arm returns. Pure
/// ASCII graphic text, exactly like a real vendor error that quotes the
/// secret it just minted — which is why no filter can distinguish it
/// from operator prose and why the AM boundary discards it wholesale.
pub const ADAPTER_DETAIL: &str = "secret=abc123";

/// The secret the happy path returns, so tests can assert it survives
/// the service layer (the credential must reach the caller) while
/// [`ADAPTER_DETAIL`] does not (adapter error text must not).
pub const FAKE_SECRET: &str = "s3cr3t-value";

/// Configurable outcome for one service-account call.
#[domain_model]
#[derive(Clone)]
pub enum FakeServiceAccountOutcome {
    /// Provision / rotate return synthesized credentials; revoke returns
    /// `Ok(())`; list returns the configured entries.
    Ok,
    /// Returns `Err(IdpServiceAccountFailure::InvalidInput)` with a
    /// credential-shaped detail AND an attributed field.
    InvalidInput,
    /// Returns `Err(IdpServiceAccountFailure::NotFound)`.
    NotFound,
    /// Returns `Err(IdpServiceAccountFailure::CleanFailure)`.
    CleanFailure,
    /// Returns `Err(IdpServiceAccountFailure::Ambiguous)`.
    Ambiguous,
    /// Returns `Err(IdpServiceAccountFailure::UnsupportedOperation)` —
    /// what the SDK default impls produce for an adapter that ships no
    /// service-account support.
    Unsupported,
}

/// In-memory provider implementing the service-account methods of
/// [`IdpPluginClient`]. Per-method outcomes default to
/// [`FakeServiceAccountOutcome::Ok`].
///
/// Every method append-records `(tenant_id, <per-method value>)` plus a
/// snapshot of `req.tenant_context.metadata`, so tests can assert both
/// "no `IdP` call issued" cases and that AM replays the plugin's own
/// per-tenant blob on each call.
#[domain_model]
pub struct FakeServiceAccountProvider {
    create_outcome: Mutex<FakeServiceAccountOutcome>,
    rotate_outcome: Mutex<FakeServiceAccountOutcome>,
    revoke_outcome: Mutex<FakeServiceAccountOutcome>,
    list_outcome: Mutex<FakeServiceAccountOutcome>,
    /// `(tenant_id, name)` per provision call.
    create_calls: Mutex<Vec<(Uuid, String)>>,
    /// `(tenant_id, client_id)` per rotate call.
    rotate_calls: Mutex<Vec<(Uuid, String)>>,
    /// `(tenant_id, client_id)` per revoke call.
    revoke_calls: Mutex<Vec<(Uuid, String)>>,
    /// `tenant_id` per list call.
    list_calls: Mutex<Vec<Uuid>>,
    /// Scopes forwarded on the most recent provision calls, in order.
    create_scopes: Mutex<Vec<Vec<String>>>,
    create_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    rotate_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    revoke_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    list_metadata_snapshots: Mutex<Vec<Option<Value>>>,
    /// Entries returned on the `list` happy path. Defaults to empty.
    list_items: Mutex<Vec<IdpServiceAccountSummary>>,
}

impl FakeServiceAccountProvider {
    pub fn new() -> Self {
        Self {
            create_outcome: Mutex::new(FakeServiceAccountOutcome::Ok),
            rotate_outcome: Mutex::new(FakeServiceAccountOutcome::Ok),
            revoke_outcome: Mutex::new(FakeServiceAccountOutcome::Ok),
            list_outcome: Mutex::new(FakeServiceAccountOutcome::Ok),
            create_calls: Mutex::new(Vec::new()),
            rotate_calls: Mutex::new(Vec::new()),
            revoke_calls: Mutex::new(Vec::new()),
            list_calls: Mutex::new(Vec::new()),
            create_scopes: Mutex::new(Vec::new()),
            create_metadata_snapshots: Mutex::new(Vec::new()),
            rotate_metadata_snapshots: Mutex::new(Vec::new()),
            revoke_metadata_snapshots: Mutex::new(Vec::new()),
            list_metadata_snapshots: Mutex::new(Vec::new()),
            list_items: Mutex::new(Vec::new()),
        }
    }

    pub fn set_create_outcome(&self, oc: FakeServiceAccountOutcome) {
        *self.create_outcome.lock().expect("lock") = oc;
    }

    pub fn set_rotate_outcome(&self, oc: FakeServiceAccountOutcome) {
        *self.rotate_outcome.lock().expect("lock") = oc;
    }

    pub fn set_revoke_outcome(&self, oc: FakeServiceAccountOutcome) {
        *self.revoke_outcome.lock().expect("lock") = oc;
    }

    pub fn set_list_outcome(&self, oc: FakeServiceAccountOutcome) {
        *self.list_outcome.lock().expect("lock") = oc;
    }

    pub fn set_list_items(&self, items: Vec<IdpServiceAccountSummary>) {
        *self.list_items.lock().expect("lock") = items;
    }

    pub fn create_call_count(&self) -> usize {
        self.create_calls.lock().expect("lock").len()
    }

    pub fn rotate_call_count(&self) -> usize {
        self.rotate_calls.lock().expect("lock").len()
    }

    pub fn revoke_call_count(&self) -> usize {
        self.revoke_calls.lock().expect("lock").len()
    }

    pub fn list_call_count(&self) -> usize {
        self.list_calls.lock().expect("lock").len()
    }

    pub fn create_calls_snapshot(&self) -> Vec<(Uuid, String)> {
        self.create_calls.lock().expect("lock").clone()
    }

    pub fn rotate_calls_snapshot(&self) -> Vec<(Uuid, String)> {
        self.rotate_calls.lock().expect("lock").clone()
    }

    pub fn revoke_calls_snapshot(&self) -> Vec<(Uuid, String)> {
        self.revoke_calls.lock().expect("lock").clone()
    }

    pub fn create_scopes_snapshot(&self) -> Vec<Vec<String>> {
        self.create_scopes.lock().expect("lock").clone()
    }

    pub fn create_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.create_metadata_snapshots.lock().expect("lock").clone()
    }

    pub fn rotate_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.rotate_metadata_snapshots.lock().expect("lock").clone()
    }

    pub fn revoke_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.revoke_metadata_snapshots.lock().expect("lock").clone()
    }

    pub fn list_metadata_snapshots(&self) -> Vec<Option<Value>> {
        self.list_metadata_snapshots.lock().expect("lock").clone()
    }
}

impl Default for FakeServiceAccountProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Synthesize credentials for a happy-path provision / rotate. The
/// `client_id` follows the common `svc-<tenant>-<name>` adapter
/// convention only so tests read naturally — nothing in AM parses it.
fn fake_credentials(client_id: String) -> IdpServiceAccountCredentials {
    IdpServiceAccountCredentials::new(
        client_id,
        SecretString::from(FAKE_SECRET.to_owned()),
        "https://idp.example.com/token".to_owned(),
        // Stands in for the IdP-side service-account subject id (the
        // `sub` of issued tokens); fixed so assertions can name it.
        Uuid::from_u128(0x5AB_5AB),
    )
}

/// Map the configured outcome onto the shared failure taxonomy. `Ok` is
/// handled per method (each returns a different success shape).
fn failure_for(oc: &FakeServiceAccountOutcome) -> Option<IdpServiceAccountFailure> {
    match oc {
        FakeServiceAccountOutcome::Ok => None,
        FakeServiceAccountOutcome::InvalidInput => {
            Some(IdpServiceAccountFailure::InvalidInput {
                detail: ADAPTER_DETAIL.to_owned(),
                // An adapter-attributed field name is untrusted text
                // too, so it must not survive the conversion either.
                field: Some(ADAPTER_DETAIL.to_owned()),
            })
        }
        FakeServiceAccountOutcome::NotFound => Some(IdpServiceAccountFailure::NotFound {
            detail: ADAPTER_DETAIL.to_owned(),
        }),
        FakeServiceAccountOutcome::CleanFailure => Some(IdpServiceAccountFailure::CleanFailure {
            detail: ADAPTER_DETAIL.to_owned(),
        }),
        FakeServiceAccountOutcome::Ambiguous => Some(IdpServiceAccountFailure::Ambiguous {
            detail: ADAPTER_DETAIL.to_owned(),
        }),
        FakeServiceAccountOutcome::Unsupported => {
            Some(IdpServiceAccountFailure::UnsupportedOperation {
                detail: ADAPTER_DETAIL.to_owned(),
            })
        }
    }
}

#[async_trait]
impl IdpPluginClient for FakeServiceAccountProvider {
    async fn provision_service_account(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionServiceAccountRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        self.create_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.name.clone()));
        self.create_scopes
            .lock()
            .expect("lock")
            .push(req.scopes.clone());
        self.create_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.create_outcome.lock().expect("lock").clone();
        match failure_for(&oc) {
            Some(failure) => Err(failure),
            None => Ok(fake_credentials(format!(
                "svc-{}-{}",
                req.tenant_context.tenant_id, req.name
            ))),
        }
    }

    async fn rotate_service_account_secret(
        &self,
        _ctx: &SecurityContext,
        req: &IdpRotateServiceAccountSecretRequest,
    ) -> Result<IdpServiceAccountCredentials, IdpServiceAccountFailure> {
        self.rotate_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.client_id.clone()));
        self.rotate_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.rotate_outcome.lock().expect("lock").clone();
        match failure_for(&oc) {
            Some(failure) => Err(failure),
            None => Ok(fake_credentials(req.client_id.clone())),
        }
    }

    async fn revoke_service_account(
        &self,
        _ctx: &SecurityContext,
        req: &IdpRevokeServiceAccountRequest,
    ) -> Result<(), IdpServiceAccountFailure> {
        self.revoke_calls
            .lock()
            .expect("lock")
            .push((req.tenant_context.tenant_id, req.client_id.clone()));
        self.revoke_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.revoke_outcome.lock().expect("lock").clone();
        match failure_for(&oc) {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    async fn list_service_accounts(
        &self,
        _ctx: &SecurityContext,
        req: &IdpListServiceAccountsRequest,
    ) -> Result<Vec<IdpServiceAccountSummary>, IdpServiceAccountFailure> {
        self.list_calls
            .lock()
            .expect("lock")
            .push(req.tenant_context.tenant_id);
        self.list_metadata_snapshots
            .lock()
            .expect("lock")
            .push(req.tenant_context.metadata.clone());
        let oc = self.list_outcome.lock().expect("lock").clone();
        match failure_for(&oc) {
            Some(failure) => Err(failure),
            None => Ok(self.list_items.lock().expect("lock").clone()),
        }
    }
}

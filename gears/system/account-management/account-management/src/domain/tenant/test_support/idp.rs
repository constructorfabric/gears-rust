//! Test stub for the [`IdpPluginClient`] contract. Pairs
//! with the four-outcome enums [`FakeOutcome`] /
//! [`FakeDeprovisionOutcome`] that drive the provision / deprovision
//! branches independently so tests can exercise both compensable and
//! non-compensable paths.

#![allow(
    dead_code,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc
)]

use std::sync::{Arc, Mutex};

use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpPluginClient, IdpProvisionFailure,
    IdpProvisionResult, IdpProvisionTenantRequest,
};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Notify;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Five-outcome stub for the `IdP` provisioner.
///
/// `Hang` exists so the saga's `tokio::time::timeout_at(deadline, ...)`
/// wrapping `provision_tenant` can be exercised: the call never resolves
/// on its own, so the deadline is the only way the future returns.
/// Drives the timeout-without-compensate branch (`service.rs` `Err(_elapsed)`
/// arm) which `Ok` / `CleanFailure` / `Ambiguous` / `Unsupported` cannot
/// reach because they all return synchronously.
#[domain_model]
#[derive(Clone)]
pub enum FakeOutcome {
    Ok,
    CleanFailure,
    Ambiguous,
    Unsupported,
    /// Plugin returned [`IdpProvisionFailure::InvalidInput`] — the
    /// permanent client-error category. AM saga compensates the
    /// `provisioning` row and surfaces 400 `invalid_argument`. Used by
    /// saga tests pinning the "no retry, no reaper, immediate 400"
    /// path that distinguishes this category from `CleanFailure`.
    InvalidInput,
    Hang,
}

/// Stub for `deprovision_tenant` outcomes. Defaults to `Ok`.
#[domain_model]
#[derive(Clone)]
pub enum FakeDeprovisionOutcome {
    Ok,
    Retryable,
    Terminal,
    Unsupported,
    NotFound,
}

#[domain_model]
pub struct FakeIdpProvisioner {
    pub outcome: Mutex<FakeOutcome>,
    pub deprovision_outcome: Mutex<FakeDeprovisionOutcome>,
    /// Opaque plugin-private metadata blob the fake returns from
    /// [`IdpPluginClient::provision_tenant`] on the `FakeOutcome::Ok`
    /// path. `None` (default) models the "plugin owns no per-tenant
    /// state" case; `Some` lets a test pin the exact JSON the
    /// production code will later replay via
    /// [`account_management_sdk::IdpTenantContext::metadata`].
    pub metadata: Mutex<Option<Value>>,
    pub calls: Mutex<Vec<Uuid>>,
    /// Last [`IdpProvisionTenantRequest`] observed by `provision_tenant`,
    /// captured verbatim so tests can assert request-shaping the saga
    /// performs (e.g. `parent_context` replay) rather than only the
    /// `tenant_id`.
    pub last_provision_request: Mutex<Option<IdpProvisionTenantRequest>>,
    pub deprovision_calls: Mutex<Vec<Uuid>>,
    /// Notified once `provision_tenant` is entered (BEFORE the
    /// per-outcome dispatch). Tests using `FakeOutcome::Hang` await
    /// this to deterministically know the saga has reached the
    /// hung future, avoiding empirical yield-loops that depend on
    /// the saga's internal step count.
    pub provision_entered: Arc<Notify>,
}

impl FakeIdpProvisioner {
    pub fn new(outcome: FakeOutcome) -> Self {
        Self {
            outcome: Mutex::new(outcome),
            deprovision_outcome: Mutex::new(FakeDeprovisionOutcome::Ok),
            metadata: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            last_provision_request: Mutex::new(None),
            deprovision_calls: Mutex::new(Vec::new()),
            provision_entered: Arc::new(Notify::new()),
        }
    }

    pub fn set_deprovision_outcome(&self, oc: FakeDeprovisionOutcome) {
        *self.deprovision_outcome.lock().expect("lock") = oc;
    }

    /// Pin the opaque metadata blob returned on the next
    /// `FakeOutcome::Ok` provision call. `None` resets the fake to
    /// the "plugin returns no per-tenant state" default.
    pub fn set_metadata(&self, metadata: Option<Value>) {
        *self.metadata.lock().expect("lock") = metadata;
    }

    /// Mutate the provision outcome between calls. Tests that need to
    /// flip from `FakeOutcome::CleanFailure` to `FakeOutcome::Ok` on
    /// the second saga attempt (retry-then-finalize coverage) call
    /// this between awaits.
    pub fn set_outcome(&self, oc: FakeOutcome) {
        *self.outcome.lock().expect("lock") = oc;
    }

    /// Read the current count of `provision_tenant` calls observed by
    /// this fake. Used by retry-loop tests to assert the saga actually
    /// advanced past `CleanFailure` rather than short-circuiting.
    pub fn provision_call_count(&self) -> usize {
        self.calls.lock().expect("lock").len()
    }

    /// The last provision request the fake captured, cloned out for
    /// assertions. `None` until `provision_tenant` has been called.
    pub fn last_provision_request(&self) -> Option<IdpProvisionTenantRequest> {
        self.last_provision_request.lock().expect("lock").clone()
    }
}

#[async_trait]
impl IdpPluginClient for FakeIdpProvisioner {
    async fn provision_tenant(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        self.calls.lock().expect("lock").push(req.tenant_id);
        *self.last_provision_request.lock().expect("lock") = Some(req.clone());
        // Signal that the saga has reached `provision_tenant`
        // BEFORE the per-outcome dispatch so a test using
        // `FakeOutcome::Hang` can synchronize against entry rather
        // than yield-spin until the saga is parked.
        self.provision_entered.notify_one();
        let oc = self.outcome.lock().expect("lock").clone();
        match oc {
            FakeOutcome::Ok => Ok(IdpProvisionResult::new(
                self.metadata.lock().expect("lock").clone(),
            )),
            FakeOutcome::CleanFailure => Err(IdpProvisionFailure::CleanFailure {
                detail: "fake clean".into(),
            }),
            FakeOutcome::Ambiguous => Err(IdpProvisionFailure::Ambiguous {
                detail: "fake ambiguous".into(),
            }),
            FakeOutcome::Unsupported => Err(IdpProvisionFailure::UnsupportedOperation {
                detail: "fake unsupported".into(),
            }),
            FakeOutcome::InvalidInput => Err(IdpProvisionFailure::InvalidInput {
                detail: "fake invalid input".into(),
                field: Some("provisioning_metadata".into()),
            }),
            FakeOutcome::Hang => {
                std::future::pending::<()>().await;
                unreachable!("FakeOutcome::Hang awaits a never-resolving future")
            }
        }
    }

    async fn deprovision_tenant(
        &self,
        _ctx: &SecurityContext,
        req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), IdpDeprovisionFailure> {
        self.deprovision_calls
            .lock()
            .expect("lock")
            .push(req.tenant_context.tenant_id);
        let oc = self.deprovision_outcome.lock().expect("lock").clone();
        match oc {
            FakeDeprovisionOutcome::Ok => Ok(()),
            FakeDeprovisionOutcome::Retryable => Err(IdpDeprovisionFailure::Retryable {
                detail: "fake retryable".into(),
            }),
            FakeDeprovisionOutcome::Terminal => Err(IdpDeprovisionFailure::Terminal {
                detail: "fake terminal".into(),
            }),
            FakeDeprovisionOutcome::Unsupported => {
                Err(IdpDeprovisionFailure::UnsupportedOperation {
                    detail: "fake unsupported".into(),
                })
            }
            FakeDeprovisionOutcome::NotFound => Err(IdpDeprovisionFailure::NotFound {
                detail: "fake not found".into(),
            }),
        }
    }
}

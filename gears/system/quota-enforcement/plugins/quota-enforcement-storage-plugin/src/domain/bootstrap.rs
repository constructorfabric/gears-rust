//! [`StoragePlugin`]: the foundation slice of the storage plugin.
//!
//! `bootstrap()` realizes the storage half of the gear bootstrap flow
//! (`features/foundation.md`, "Gear Bootstrap and Readiness"): it verifies
//! the installed schema major against the contract major (invariant I12) and
//! seeds the three platform-default configuration rows when missing. Both
//! steps are idempotent and safe under concurrent replicas.
//!
//! The `QuotaEnforcementStoragePluginV1` implementation is wired only once
//! every primitive the trait names exists. Until then this type is reached by
//! the plugin gear and by tests only. The Quota primitives it forwards live in
//! `domain::quotas`.

use std::sync::Arc;

use quota_enforcement_sdk::{BootstrapBundle, ConfigDefaults, PolicyScope, StorageError};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::ports::{
    ConsumptionStore, FoundationStore, LeaseStore, PolicyStore, QuotaStore, SeedReport, StoreError,
};

const LOG_TARGET: &str = "qe.storage";

/// Storage plugin over its foundation and Quota stores.
#[domain_model]
#[derive(Clone)]
pub struct StoragePlugin {
    store: Arc<dyn FoundationStore>,
    pub(super) quotas: Arc<dyn QuotaStore>,
    pub(super) policies: Arc<dyn PolicyStore>,
    pub(super) consumption: Arc<dyn ConsumptionStore>,
    pub(super) leases: Arc<dyn LeaseStore>,
}

impl StoragePlugin {
    /// Bind the plugin to its stores.
    #[must_use]
    pub fn new(
        store: Arc<dyn FoundationStore>,
        quotas: Arc<dyn QuotaStore>,
        policies: Arc<dyn PolicyStore>,
        consumption: Arc<dyn ConsumptionStore>,
        leases: Arc<dyn LeaseStore>,
    ) -> Self {
        Self {
            store,
            quotas,
            policies,
            consumption,
            leases,
        }
    }

    /// Verify the schema major and seed the default configuration rows.
    ///
    /// # Errors
    ///
    /// - [`StorageError::SchemaVersionMismatch`] when the installed major
    ///   differs from `bundle.contract_major`. Nothing is seeded.
    /// - [`StorageError::Unavailable`] when the store rejects a call.
    /// - [`StorageError::Internal`] when a configured default does not fit its
    ///   column type.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1
    // @cpt-dod:cpt-cf-quota-enforcement-dod-storage-plugin:p1
    pub async fn bootstrap(&self, bundle: &BootstrapBundle) -> Result<SeedReport, StorageError> {
        let expected = i32::try_from(bundle.contract_major).map_err(|_| {
            StorageError::Internal(format!(
                "contract major {} does not fit the schema column",
                bundle.contract_major
            ))
        })?;

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema
        let installed = self.ensure_schema_major(expected).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema-if
        if installed != expected {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema-abort
            tracing::error!(
                target: LOG_TARGET,
                installed,
                expected,
                "installed schema major does not match the contract major; refusing to serve"
            );
            return Err(StorageError::SchemaVersionMismatch {
                installed: u32::try_from(installed).unwrap_or(0),
                expected: bundle.contract_major,
            });
            // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema-abort
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-schema-if

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-seed-config
        let report = self
            .seed_configuration_defaults(&bundle.config_defaults)
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-seed-config

        self.seed_global_policy(bundle).await?;
        Ok(report)
    }

    /// Seed the `global` policy when the caller supplied one and the scope is
    /// empty.
    ///
    /// Idempotent by re-reading the scope rather than by catching a unique
    /// violation, so an operator who has since updated or rolled back the
    /// global policy keeps their version: a later bootstrap finds the scope
    /// occupied and writes nothing. Concurrent replicas that both find it
    /// empty are arbitrated by the live-scope unique index, and the loser
    /// treats `PolicyScopeOccupied` as success for the same reason.
    ///
    /// Engine registration happens before this runs, so no active policy can
    /// reference an unregistered engine.
    async fn seed_global_policy(&self, bundle: &BootstrapBundle) -> Result<(), StorageError> {
        let Some(draft) = bundle.global_policy.clone() else {
            return Ok(());
        };
        if self
            .policies
            .read_policy(&PolicyScope::Global)
            .await?
            .is_some()
        {
            return Ok(());
        }
        // Bootstrap has no principal; the nil subject records that absence.
        match self
            .policies
            .create_policy(&SecurityContext::anonymous(), draft, &[])
            .await
        {
            Ok(seeded) => {
                tracing::info!(
                    target: LOG_TARGET,
                    engine_id = %seeded.engine_id,
                    "seeded the global resolution policy"
                );
                Ok(())
            }
            // A concurrent replica won the race; its row is as good as ours.
            Err(StorageError::PolicyScopeOccupied { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The installed contract major, if any.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when the store rejects the read.
    pub async fn installed_major(&self) -> Result<Option<i32>, StorageError> {
        self.store
            .read_installed_major()
            .await
            .map_err(StorageError::from_store)
    }

    /// Read the installed major. On a fresh schema, record `expected` first; a
    /// concurrent peer may win that write, so the value is re-read.
    async fn ensure_schema_major(&self, expected: i32) -> Result<i32, StorageError> {
        if let Some(major) = self
            .store
            .read_installed_major()
            .await
            .map_err(StorageError::from_store)?
        {
            return Ok(major);
        }
        let wrote = self
            .store
            .record_major(expected)
            .await
            .map_err(StorageError::from_store)?;
        if wrote {
            tracing::info!(target: LOG_TARGET, contract_major = expected, "recorded schema major");
        }
        Ok(self
            .store
            .read_installed_major()
            .await
            .map_err(StorageError::from_store)?
            .unwrap_or(expected))
    }

    async fn seed_configuration_defaults(
        &self,
        defaults: &ConfigDefaults,
    ) -> Result<SeedReport, StorageError> {
        let report = self
            .store
            .seed_defaults(defaults)
            .await
            .map_err(StorageError::from_store)?;
        tracing::info!(
            target: LOG_TARGET,
            inserted = report.inserted,
            present = report.present,
            "configuration defaults seeded"
        );
        Ok(report)
    }
}

/// Lift of the store port errors onto the contract error. The four
/// quota-lifecycle variants, `SubjectOutOfScope`, and `InvalidCursor` map one
/// to one; caller input the contract has no variant for (filter bounds), a
/// patch the gear cannot produce, and every inconsistency are `Internal`, as
/// the contract documents.
pub(super) trait FromStore {
    fn from_store(err: StoreError) -> Self;
}

impl FromStore for StorageError {
    fn from_store(err: StoreError) -> Self {
        match err {
            StoreError::Unavailable { .. } => Self::Unavailable(err.to_string()),
            StoreError::QuotaNotFound { id } => Self::QuotaNotFound { id },
            StoreError::QuotaDeactivated { id } => Self::QuotaDeactivated { id },
            StoreError::CapBelowConsumed { new_cap, consumed } => {
                Self::CapBelowConsumed { new_cap, consumed }
            }
            StoreError::ThresholdsRequireBoundedCap => Self::ThresholdsRequireBoundedCap,
            StoreError::SubjectOutOfScope => Self::SubjectOutOfScope,
            StoreError::InvalidCursor => Self::InvalidCursor,
            StoreError::DefaultOutOfRange { .. }
            | StoreError::InvalidPatch { .. }
            | StoreError::InvalidFilter { .. }
            | StoreError::ValueOutOfRange { .. }
            | StoreError::Corrupt { .. } => Self::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;

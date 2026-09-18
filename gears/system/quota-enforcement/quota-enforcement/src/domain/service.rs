//! The gear's domain service: admission plus the dependencies bootstrap binds.

use std::sync::{Arc, OnceLock};

use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
use toolkit_macros::domain_model;

use super::admission::Admission;
use super::attribution::Attribution;
use super::bootstrap::Bound;
use super::catalog::ProjectionContractCatalog;
use super::error::{Dependency, DomainError};
use super::policies::PolicyRuntimeLimits;
use super::ports::coordination::SingletonCoordinator;
use super::quotas::{QuotaLimits, QuotaManagement};
use super::readiness::Readiness;

/// Process-local runtime bounds of the hot path, passed at construction
/// because they are neither PDP state nor bootstrap dependencies.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct OperationsRuntime {
    /// Replay records the cache holds.
    pub cache_entries: usize,
    /// How long a cached record may answer.
    pub cache_ttl: std::time::Duration,
    /// How many preparations one operation may trigger.
    pub preparation_max_attempts: std::num::NonZeroU32,
}

/// Composition root of the domain. Handlers and the in-process client reach
/// every dependency through it.
#[domain_model]
pub struct Service {
    admission: Admission,
    readiness: Arc<Readiness>,
    limits: QuotaLimits,
    policy_limits: PolicyRuntimeLimits,
    bound: OnceLock<Bound>,
    policy_schemas: OnceLock<super::policies::schemas::CatalogPolicySchemas>,
    operations: super::operations::IdempotencyCache,
    evaluation: quota_enforcement_sdk::engine::EvaluationLimits,
    preparation_max_attempts: std::num::NonZeroU32,
}

impl Service {
    /// Assemble the service. Dependencies are bound later by bootstrap.
    #[must_use]
    pub fn new(
        admission: Admission,
        readiness: Arc<Readiness>,
        limits: QuotaLimits,
        policy_limits: PolicyRuntimeLimits,
        operations: OperationsRuntime,
    ) -> Self {
        Self {
            admission,
            readiness,
            limits,
            policy_limits,
            bound: OnceLock::new(),
            policy_schemas: OnceLock::new(),
            operations: super::operations::IdempotencyCache::new(
                operations.cache_entries,
                operations.cache_ttl,
            ),
            evaluation: policy_limits.evaluation,
            preparation_max_attempts: operations.preparation_max_attempts,
        }
    }

    /// The PEP boundary.
    #[must_use]
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Readiness cell shared with the health check.
    #[must_use]
    pub fn readiness(&self) -> &Arc<Readiness> {
        &self.readiness
    }

    /// Publish the bootstrapped dependencies. Set once.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Internal`] when dependencies were bound before.
    pub fn bind(&self, bound: Bound) -> Result<(), DomainError> {
        self.policy_schemas
            .set(super::policies::schemas::CatalogPolicySchemas::new(
                Arc::clone(&bound.catalog),
                self.policy_limits.snapshot,
            ))
            .map_err(|_| DomainError::Internal("dependencies were already bound".into()))?;
        self.bound
            .set(bound)
            .map_err(|_| DomainError::Internal("dependencies were already bound".to_owned()))
    }

    /// The active storage plugin.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn storage(&self) -> Result<Arc<dyn QuotaEnforcementStoragePluginV1>, DomainError> {
        self.bound
            .get()
            .map(|b| b.storage.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Storage,
            })
    }

    /// The sweeper coordinator over the platform `cluster` gear.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn coordinator(&self) -> Result<Arc<dyn SingletonCoordinator>, DomainError> {
        self.bound
            .get()
            .map(|b| b.coordinator.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Cluster,
            })
    }

    /// The published projection contract catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn catalog(&self) -> Result<Arc<ProjectionContractCatalog>, DomainError> {
        self.bound
            .get()
            .map(|b| b.catalog.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Catalog,
            })
    }

    /// The ingress step of every subject-based evaluation operation: shape
    /// check, PDP admission of the attribution tuple, catalogue mapping.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn attribution(&self) -> Result<Attribution<'_>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Catalog,
        })?;
        Ok(Attribution::new(
            &self.admission,
            &bound.catalog,
            self.admission.metrics(),
        ))
    }

    /// The shared operator policy lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap publishes the dependencies.
    pub fn policies(&self) -> Result<super::policies::PolicyManagement<'_>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        let schemas = self.policy_schemas.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Catalog,
        })?;
        Ok(super::policies::PolicyManagement {
            admission: &self.admission,
            storage: bound.storage.as_ref(),
            engines: &bound.engines,
            cache: &bound.artifacts,
            schemas,
            metrics: self.admission.metrics(),
            limits: self.policy_limits.authoring,
        })
    }

    /// The consumption hot path: debit, credit, rollback, preview.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn operations(&self) -> Result<super::operations::Operations<'_>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        Ok(super::operations::Operations {
            admission: &self.admission,
            attribution: Attribution::new(
                &self.admission,
                &bound.catalog,
                self.admission.metrics(),
            ),
            catalog: &bound.catalog,
            classifications: &bound.classifications,
            storage: bound.storage.as_ref(),
            engines: Arc::clone(&bound.engines),
            artifacts: Arc::clone(&bound.artifacts),
            idempotency: &self.operations,
            metrics: self.admission.metrics_handle(),
            evaluation: self.evaluation,
            preparation_max_attempts: self.preparation_max_attempts,
        })
    }

    /// The Quota lifecycle: create, update, deactivate, read.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn quotas(&self) -> Result<QuotaManagement<'_>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        Ok(QuotaManagement::new(
            &self.admission,
            &bound.catalog,
            bound.storage.as_ref(),
            bound.registry.as_ref(),
            bound.metric_registry.as_ref(),
            self.admission.metrics(),
            self.limits,
        ))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

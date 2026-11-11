//! Domain service for the license resolver gear.
//!
//! Plugin discovery is lazy: resolved on the first check after types-registry is
//! ready, then memoized.

use std::sync::Arc;
use std::time::{Duration, Instant};

use license_resolver_sdk::{
    LicenseCheckContext, LicenseCheckRequest, LicenseDecision, LicenseResolverPluginClient,
    LicenseResolverPluginSpecV1, Resource, Subject, field,
};
use serde_json::Map;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use toolkit::telemetry::ThrottledLog;
use toolkit_macros::domain_model;
use tracing::info;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
use uuid::Uuid;

use super::error::DomainError;
use super::ports::{CheckOutcome, LicenseMetrics, ViolationKind};
use super::principal::PrincipalMapper;
use super::validation::ContractValidator;

const UNAVAILABLE_LOG_THROTTLE: Duration = Duration::from_secs(10);

/// Contract-type label used when validation did not complete.
///
/// A caller-supplied contract type only becomes a bounded label once it is known
/// to be registered; labelling with an unvalidated one would let any caller grow
/// the metric's label space without limit.
const UNVALIDATED_CONTRACT: &str = "unvalidated";

const PRINCIPAL_NOT_MAPPED: &str = "principal_not_mapped";

/// License resolver service.
///
/// Maps a principal subject to its Subject contract type, validates each request
/// against its registered licensing contracts, selects a backend plugin by
/// vendor + priority, and delegates the check.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    validator: ContractValidator,
    principals: PrincipalMapper,
    metrics: Arc<dyn LicenseMetrics>,
    selector: GtsPluginSelector,
    unavailable_log_throttle: ThrottledLog,
    mapping_log_throttle: ThrottledLog,
}

impl Service {
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        vendor: String,
        validator: ContractValidator,
        principals: PrincipalMapper,
        metrics: Arc<dyn LicenseMetrics>,
    ) -> Self {
        Self {
            hub,
            vendor,
            validator,
            principals,
            metrics,
            selector: GtsPluginSelector::new(),
            unavailable_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
            mapping_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
        }
    }

    /// Answer whether the resource is licensed to the subject.
    ///
    /// The request is forwarded to the backend unchanged, tenant scope included.
    ///
    /// # Errors
    ///
    /// - [`DomainError::ContractViolation`] — the request does not conform to its
    ///   registered contracts. Raised before any plugin is consulted, so it does
    ///   not depend on a backend being reachable.
    /// - [`DomainError::PluginNotFound`] / [`DomainError::PluginUnavailable`] —
    ///   no backend, or an unreachable one. Never a granted decision.
    /// - Anything the selected backend surfaces, mapped through
    ///   [`DomainError::from_plugin`].
    pub async fn is_licensed(
        &self,
        request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, DomainError> {
        self.check(request, Instant::now()).await
    }

    /// [`is_licensed`](Self::is_licensed) with the resolver latency measured
    /// from `started`, so a principal check includes its mapping.
    #[tracing::instrument(
        skip_all,
        fields(
            tenant_id = %request.context.tenant_id,
            resource_contract = %request.resource.gts_type,
        )
    )]
    async fn check(
        &self,
        request: LicenseCheckRequest,
        started: Instant,
    ) -> Result<LicenseDecision, DomainError> {
        // Validation gates selection: a non-conforming request is refused on its
        // own terms, so the answer cannot depend on a backend being reachable.
        // The label is owned because the delegated call takes `request`.
        let (label, selected) = match self.validator.validate(&request).await {
            Ok(()) => (
                request.resource.gts_type.as_ref().to_owned(),
                self.select_plugin().await,
            ),
            Err(err) => {
                // Only the violations this resolver found are counted as
                // validation failures: a backend's rejection of a conforming
                // request is its own classification, not this gear's validation
                // outcome.
                if let DomainError::ContractViolation { violations } = &err {
                    for violation in violations {
                        self.metrics.record_validation_failure(ViolationKind::from(
                            violation.reason.as_str(),
                        ));
                    }
                }
                (UNVALIDATED_CONTRACT.to_owned(), Err(err))
            }
        };

        // Recorded before delegating: the read-latency boundary is the resolver's
        // own work and excludes backend compute.
        self.metrics
            .record_resolver_latency(started.elapsed().as_secs_f64() * 1_000.0);

        let (instance_id, plugin) = match selected {
            Ok(selected) => selected,
            Err(err) => return Err(self.fail(&label, err)),
        };

        match plugin.is_licensed(request).await {
            Ok(decision) => {
                let outcome = if decision.granted {
                    CheckOutcome::Granted
                } else {
                    CheckOutcome::NotGranted
                };
                self.metrics.record_check(&label, outcome);
                Ok(decision)
            }
            Err(err) => Err(self.fail(&label, DomainError::from_plugin(&instance_id, err))),
        }
    }

    /// Answer whether the resource is licensed to the principal, mapped to its
    /// Subject contract type with `id` and empty `metadata`. A principal that is
    /// not mapped, or has a nil id or tenant, is not granted, without validating
    /// the resource.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the mapping cannot be
    /// determined, otherwise as [`is_licensed`](Self::is_licensed).
    pub async fn is_principal_licensed(
        &self,
        principal_type: Option<&str>,
        principal_id: Uuid,
        resource: Resource,
        context: LicenseCheckContext,
    ) -> Result<LicenseDecision, DomainError> {
        let started = Instant::now();
        let subject_type = if principal_id.is_nil() || context.tenant_id.is_nil() {
            Ok(None)
        } else {
            self.principals.subject_type(principal_type).await
        };
        if !matches!(subject_type, Ok(Some(_))) {
            self.metrics
                .record_resolver_latency(started.elapsed().as_secs_f64() * 1_000.0);
        }
        let Some(gts_type) = subject_type.map_err(|err| self.fail(UNVALIDATED_CONTRACT, err))?
        else {
            self.metrics
                .record_check(UNVALIDATED_CONTRACT, CheckOutcome::PrincipalNotMapped);
            return Ok(LicenseDecision::new(false).with_diagnostic("reason", PRINCIPAL_NOT_MAPPED));
        };
        let subject = Subject {
            gts_type: gts_type.clone(),
            id: Some(principal_id.to_string()),
            metadata: Map::new(),
        };
        let result = self
            .check(
                LicenseCheckRequest::new(subject, resource, context),
                started,
            )
            .await;
        // The caller did not choose this subject type, the configuration did, so
        // the operator needs to see a subject that its own contract rejects.
        if let Err(DomainError::ContractViolation { violations }) = &result
            && violations.iter().any(|v| {
                v.field.starts_with(field::SUBJECT_FIELD)
                    && !matches!(
                        ViolationKind::from(v.reason.as_str()),
                        ViolationKind::Other | ViolationKind::SubjectNotAdmitted
                    )
            })
            && self.mapping_log_throttle.should_log()
        {
            tracing::warn!(
                principal_type = principal_type.unwrap_or_default(),
                subject_type = %gts_type,
                "principal_subject_types maps to a Subject contract type that the mapped subject does not conform to"
            );
        }
        result
    }

    fn fail(&self, label: &str, err: DomainError) -> DomainError {
        self.metrics.record_check(label, CheckOutcome::from(&err));
        err
    }

    async fn select_plugin(
        &self,
    ) -> Result<(Arc<str>, Arc<dyn LicenseResolverPluginClient>), DomainError> {
        let instance_id = self.selector.get_or_init(|| self.resolve_plugin()).await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn LicenseResolverPluginClient>(&scope)
        {
            Ok((instance_id, client))
        } else {
            if self.unavailable_log_throttle.should_log() {
                tracing::warn!(
                    plugin_gts_id = %instance_id,
                    vendor = %self.vendor,
                    "Plugin client not registered yet"
                );
            }
            Err(DomainError::PluginUnavailable {
                gts_id: instance_id.to_string(),
                reason: "client not registered yet".into(),
            })
        }
    }

    #[tracing::instrument(skip_all, fields(vendor = %self.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving license resolver plugin");

        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = LicenseResolverPluginSpecV1::gts_type_id().clone();

        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
            .await
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let gts_id = choose_plugin_instance::<LicenseResolverPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        )?;
        info!(plugin_gts_id = %gts_id, "Selected license resolver plugin instance");

        Ok(gts_id)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

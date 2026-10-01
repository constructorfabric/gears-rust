//! The decision service: one evaluation per request, no stored state.
//!
//! Steps, in order; the first that stops the path decides:
//!
//! 1. an anonymous context (nil subject or subject tenant) is an invalid
//!    request; the subject and its tenant come from the `SecurityContext` only;
//! 2. reachability: the caller's tenant must be the resource tenant or an
//!    ancestor of it (barriers respected), else a denial with
//!    [`reason::TENANT_BOUNDARY`]; the tenant chain is the resource tenant and
//!    its ancestors up to the first self-managed barrier; a hierarchy outage or
//!    timeout is a failure, never a denial;
//! 3. one repository read loads the active bindings along the chain, ordered
//!    nearest tenant first, then bundle id;
//! 4. documents whose `resource_types` and `actions` match the request apply;
//! 5. compiled versions come from a bounded cache keyed by version id;
//! 6. every applicable document is evaluated on a blocking task under the
//!    evaluation timeout; any error fails the request (fail closed). A denial
//!    of an enforcing assignment denies; one of a non-enforcing assignment is a
//!    shadow denial, reported beside either outcome.

use std::sync::Arc;
use std::time::{Duration, Instant};

use admission_control_sdk::EngineRequest;
use gts::GtsId;
use policy_engine_sdk::reason;
use tenant_resolver_sdk::BarrierMode;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::compiled::{CompileCache, CompiledVersion, compile_version};
use crate::domain::eval::{CombinedResult, EvaluatedDocumentKey, combine};
use crate::domain::evaluator::{ApplicableDocument, RequestFacts, evaluate_documents};
use crate::domain::model::VersionId;
use crate::domain::ports::{ActiveBinding, BindingSource, HierarchyPort, PortError};

/// The outcome of a completed evaluation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No enforced denial.
    Permit {
        /// Denials of non-enforcing assignments.
        shadow_denials: Vec<EvaluatedDocumentKey>,
    },
    /// The operation is denied.
    Deny {
        /// [`reason::POLICY_DENIED`] or [`reason::TENANT_BOUNDARY`].
        reason_code: &'static str,
        /// Enforced denials, nearest tenant first; empty for a boundary denial.
        denials: Vec<EvaluatedDocumentKey>,
        /// Denials of non-enforcing assignments.
        shadow_denials: Vec<EvaluatedDocumentKey>,
    },
}

/// Why no verdict could be reached. Never a denial.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionFailure {
    /// The request itself is unusable.
    InvalidRequest(&'static str),
    /// A dependency did not answer in time.
    Timeout(&'static str),
    /// A dependency is unavailable.
    Unavailable(&'static str),
    /// The engine could not complete the evaluation.
    Internal(&'static str),
}

/// Telemetry of the decision path; implementations must be cheap.
pub trait DecisionMetrics: Send + Sync {
    /// One completed request: `outcome` is `permit`, `deny` or `failure`.
    fn evaluation(&self, _outcome: &'static str, _elapsed: Duration) {}
    /// One compiled-version lookup.
    fn compile_cache(&self, _hit: bool) {}
}

/// [`DecisionMetrics`] that records nothing.
#[domain_model]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDecisionMetrics;

impl DecisionMetrics for NoopDecisionMetrics {}

/// Evaluates requests against the active policy along the tenant chain.
#[domain_model]
pub struct DecisionService {
    hierarchy: Arc<dyn HierarchyPort>,
    bindings: Arc<dyn BindingSource>,
    cache: Arc<CompileCache>,
    evaluation_timeout: Duration,
    metrics: Arc<dyn DecisionMetrics>,
}

impl std::fmt::Debug for DecisionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionService")
            .field("evaluation_timeout", &self.evaluation_timeout)
            .finish_non_exhaustive()
    }
}

impl DecisionService {
    /// A service over its ports.
    #[must_use]
    pub fn new(
        hierarchy: Arc<dyn HierarchyPort>,
        bindings: Arc<dyn BindingSource>,
        cache: Arc<CompileCache>,
        evaluation_timeout: Duration,
        metrics: Arc<dyn DecisionMetrics>,
    ) -> Self {
        Self {
            hierarchy,
            bindings,
            cache,
            evaluation_timeout,
            metrics,
        }
    }

    /// Decides one request.
    ///
    /// # Errors
    ///
    /// [`DecisionFailure`] when no verdict could be reached; the caller must
    /// treat it as a refusal.
    pub async fn decide(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<Verdict, DecisionFailure> {
        let started = Instant::now();
        let result = self.run(ctx, request).await;
        let outcome = match &result {
            Ok(Verdict::Permit { .. }) => "permit",
            Ok(Verdict::Deny { .. }) => "deny",
            Err(_) => "failure",
        };
        self.metrics.evaluation(outcome, started.elapsed());
        result
    }

    async fn run(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<Verdict, DecisionFailure> {
        let caller_tenant = ctx.subject_tenant_id();
        if ctx.subject_id().is_nil() || caller_tenant.is_nil() {
            return Err(DecisionFailure::InvalidRequest(
                reason::SECURITY_CONTEXT_REQUIRED,
            ));
        }
        let type_id = GtsId::try_new(&request.resource_type)
            .map_err(|_| DecisionFailure::InvalidRequest("invalid resource type"))?;
        let resource_tenant = request.resource_tenant_id;

        let Some(chain) = self.chain(ctx, caller_tenant, resource_tenant).await? else {
            return Ok(boundary_denial());
        };

        let mut bindings = self
            .bindings
            .active_bindings(&chain)
            .await
            .map_err(|e| port_failure(&e, "policy content"))?;
        bindings.sort_by_key(|b| {
            let rank = chain
                .iter()
                .position(|t| *t == b.assignment.tenant_id)
                .unwrap_or(usize::MAX);
            (rank, b.assignment.bundle_id)
        });

        let facts = RequestFacts {
            action: request.action.clone(),
            resource_type: request.resource_type.clone(),
            resource_id: request.resource_id,
            resource_tenant_id: resource_tenant,
            properties: request.properties.clone(),
            subject_id: ctx.subject_id(),
            subject_tenant_id: caller_tenant,
        };
        let cache = Arc::clone(&self.cache);
        let metrics = Arc::clone(&self.metrics);
        let budget = self.evaluation_timeout;
        let combined = tokio::task::spawn_blocking(move || {
            evaluate_bindings(
                &cache,
                metrics.as_ref(),
                &bindings,
                &facts,
                &type_id,
                budget,
            )
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "policy-engine: evaluation task failed");
            DecisionFailure::Internal("evaluation task failed")
        })??;

        Ok(if combined.is_denied() {
            Verdict::Deny {
                reason_code: reason::POLICY_DENIED,
                denials: combined.denials,
                shadow_denials: combined.shadow_denials,
            }
        } else {
            Verdict::Permit {
                shadow_denials: combined.shadow_denials,
            }
        })
    }

    /// The resource tenant's chain, or `None` when it is not reachable from
    /// the caller's tenant.
    async fn chain(
        &self,
        ctx: &SecurityContext,
        caller_tenant: Uuid,
        resource_tenant: Uuid,
    ) -> Result<Option<Vec<Uuid>>, DecisionFailure> {
        if caller_tenant != resource_tenant {
            match self
                .hierarchy
                .is_reachable(ctx, caller_tenant, resource_tenant, BarrierMode::Respect)
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(PortError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(port_failure(&e, "tenant hierarchy")),
            }
        }
        match self.hierarchy.ancestors(ctx, resource_tenant).await {
            Ok(chain) => Ok(Some(chain)),
            Err(PortError::NotFound(_)) => Ok(None),
            Err(e) => Err(port_failure(&e, "tenant hierarchy")),
        }
    }
}

fn boundary_denial() -> Verdict {
    Verdict::Deny {
        reason_code: reason::TENANT_BOUNDARY,
        denials: Vec::new(),
        shadow_denials: Vec::new(),
    }
}

fn port_failure(err: &PortError, what: &'static str) -> DecisionFailure {
    match err {
        PortError::Timeout => DecisionFailure::Timeout(what),
        PortError::Unavailable(_) | PortError::NotFound(_) => DecisionFailure::Unavailable(what),
    }
}

fn compiled_version(
    cache: &CompileCache,
    metrics: &dyn DecisionMetrics,
    binding: &ActiveBinding,
) -> Result<Arc<CompiledVersion>, DecisionFailure> {
    let id: VersionId = binding.version.id;
    if let Some(hit) = cache.get(id) {
        metrics.compile_cache(true);
        return Ok(hit);
    }
    metrics.compile_cache(false);
    let compiled = compile_version(&binding.version).map_err(|e| {
        tracing::error!(version_id = %id, error = %e, "policy-engine: active version does not compile");
        DecisionFailure::Internal("active policy content does not compile")
    })?;
    let compiled = Arc::new(compiled);
    cache.insert(id, Arc::clone(&compiled));
    Ok(compiled)
}

fn evaluate_bindings(
    cache: &CompileCache,
    metrics: &dyn DecisionMetrics,
    bindings: &[ActiveBinding],
    facts: &RequestFacts,
    type_id: &GtsId,
    budget: Duration,
) -> Result<CombinedResult, DecisionFailure> {
    let mut applicable = Vec::new();
    for binding in bindings {
        let compiled = compiled_version(cache, metrics, binding)?;
        for entry in compiled.applicable(type_id, &facts.action) {
            applicable.push(ApplicableDocument {
                compiled: Arc::clone(&entry.compiled),
                key: EvaluatedDocumentKey {
                    document_id: entry.document_id,
                    document_name: entry.name.clone(),
                    version_id: binding.version.id,
                    bundle_id: binding.version.bundle_id,
                },
                enforce: binding.assignment.enforce,
            });
        }
    }
    let evaluated = evaluate_documents(&applicable, facts, OffsetDateTime::now_utc(), budget)
        .map_err(|e| {
            tracing::error!(error = %e, "policy-engine: evaluation failed");
            DecisionFailure::Internal("policy evaluation failed")
        })?;
    Ok(combine(evaluated))
}

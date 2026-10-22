//! The decision service: one evaluation per request, no stored state.
//!
//! Steps, in order; the first that stops the path decides:
//!
//! 1. an anonymous context (nil subject or subject tenant) is an invalid
//!    request; the subject and its tenant come from the `SecurityContext` only;
//! 2. built-in policies whose `resource_types` and `actions` match the
//!    request are evaluated on a blocking task, in configuration order; the
//!    first denial is a denial with [`reason::BUILTIN_POLICY_DENIED`], whatever
//!    the tenants, and ends the request;
//! 3. the tenant chain is the resource tenant and its ancestors up to the
//!    first self-managed barrier, read once; the caller's tenant must be in
//!    it (the resource tenant or an ancestor, barriers respected), else a
//!    denial with [`reason::TENANT_BOUNDARY`]. Reachability and policy
//!    selection use this one answer, so a barrier raised concurrently can
//!    never pair an old reach with a new chain. A hierarchy outage or timeout
//!    is a failure, never a denial;
//! 4. one repository read loads the active bindings along the chain (no
//!    document content); an assignment applies only while its owner tenant is
//!    in the chain at or above its target, that is while the owner still
//!    reaches the target with barriers respected. Bindings are ordered
//!    nearest tenant first, then bundle id;
//! 5. compiled versions come from a bounded cache keyed by version id; the
//!    content of the versions it misses is read in a second repository read
//!    and each is compiled once, however many requests miss it concurrently;
//! 6. documents whose `resource_types` and `actions` match the request apply;
//! 7. every applicable document is evaluated on a blocking task under what the
//!    built-ins left of the evaluation timeout; any error fails the request
//!    (fail closed). A denial of an enforcing assignment denies; one of a
//!    non-enforcing assignment is a shadow denial, reported beside either
//!    outcome. Findings past the admission gate's bound are dropped, shadow
//!    denials first, and the drop is logged.
//!
//! Built-ins and tenant documents see the same input and evaluation time.
//! Compiling a missed version is not charged to the evaluation time: it
//! happens once per version and process, bounded by the content limits.
//! The outcome is the admission-control engine vocabulary itself: an
//! [`EngineResult`], or an [`EngineFailure`] when no verdict could be reached.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use admission_control_sdk::{
    EngineFailure, EngineRequest, EngineResult, EngineResultBuilder, MAX_ENGINE_FINDINGS,
    PolicyReference,
};
use gts::GtsId;
use policy_engine_sdk::reason;
use tenant_resolver_sdk::TenantId;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::EvaluationBudget;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::builtin::BuiltinPolicySet;
use super::compiled::{CompileCache, CompiledVersion, compile_content};
use crate::domain::evaluator::{
    ApplicableDocument, EvaluatorFailure, RequestFacts, evaluate_documents,
};
use crate::domain::model::{Document, VersionId};
use crate::domain::ports::{ActiveBinding, BindingSource, HierarchyPort, PortError};

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

/// Evaluates requests against the built-in policies, then the active policy
/// along the tenant chain.
#[domain_model]
pub struct DecisionService {
    builtins: Arc<BuiltinPolicySet>,
    hierarchy: Arc<dyn HierarchyPort>,
    bindings: Arc<dyn BindingSource>,
    cache: Arc<CompileCache>,
    evaluation_timeout: Duration,
    metrics: Arc<dyn DecisionMetrics>,
}

impl std::fmt::Debug for DecisionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionService")
            .field("builtins", &self.builtins.len())
            .field("evaluation_timeout", &self.evaluation_timeout)
            .finish_non_exhaustive()
    }
}

impl DecisionService {
    /// A service over its ports.
    #[must_use]
    pub fn new(
        builtins: BuiltinPolicySet,
        hierarchy: Arc<dyn HierarchyPort>,
        bindings: Arc<dyn BindingSource>,
        cache: Arc<CompileCache>,
        evaluation_timeout: Duration,
        metrics: Arc<dyn DecisionMetrics>,
    ) -> Self {
        Self {
            builtins: Arc::new(builtins),
            hierarchy,
            bindings,
            cache,
            evaluation_timeout,
            metrics,
        }
    }

    /// Decides one request. A denial carries
    /// [`reason::BUILTIN_POLICY_DENIED`] or [`reason::TENANT_BOUNDARY`] with no
    /// document, or [`reason::POLICY_DENIED`] with the enforced denials,
    /// nearest tenant first.
    ///
    /// # Errors
    ///
    /// [`EngineFailure`] when no verdict could be reached; the caller must
    /// treat it as a refusal. Its detail is a fixed string, never a
    /// caller-supplied value. Every log line of the decision carries the
    /// request's correlation id.
    #[tracing::instrument(
        name = "policy_engine.decide",
        skip_all,
        fields(correlation_id = %request.correlation_id)
    )]
    pub async fn decide(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        let started = Instant::now();
        let result = self.run(ctx, request).await;
        let outcome = match &result {
            Ok(EngineResult::Permit { .. }) => "permit",
            Ok(_) => "deny",
            Err(_) => "failure",
        };
        self.metrics.evaluation(outcome, started.elapsed());
        result
    }

    async fn run(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        let caller_tenant = ctx.subject_tenant_id();
        if ctx.subject_id().is_nil() || caller_tenant.is_nil() {
            return Err(EngineFailure::invalid_request(
                reason::SECURITY_CONTEXT_REQUIRED,
            ));
        }
        let type_id = GtsId::try_new(&request.resource_type)
            .map_err(|_| EngineFailure::invalid_request("invalid resource type"))?;
        let resource_tenant = request.resource_tenant_id;
        let facts = Arc::new(RequestFacts {
            action: request.action.clone(),
            resource_type: request.resource_type.clone(),
            resource_id: request.resource_id,
            resource_tenant_id: resource_tenant,
            properties: request.properties.clone(),
            subject_id: ctx.subject_id(),
            subject_tenant_id: caller_tenant,
        });
        let evaluated_at = OffsetDateTime::now_utc();

        let mut budget = EvaluationBudget::new(self.evaluation_timeout);
        if self.builtins.applies_to(&type_id, &request.action) {
            let (denial, rest) = self
                .evaluate_builtins(&type_id, &facts, evaluated_at, budget)
                .await?;
            if let Some(policy) = denial {
                // A built-in denial names no document, so this line is the
                // only record of which built-in denied.
                tracing::info!(
                    builtin_policy = %policy,
                    "policy-engine: built-in policy denied"
                );
                return Ok(EngineResult::deny(reason::BUILTIN_POLICY_DENIED));
            }
            budget = rest;
        }

        let Some(chain) = self.chain(ctx, caller_tenant, resource_tenant).await? else {
            return Ok(EngineResult::deny(reason::TENANT_BOUNDARY));
        };
        self.evaluate_chain(&chain, facts, type_id, evaluated_at, budget)
            .await
    }

    /// Evaluates the policy assigned along `chain` under `budget`.
    async fn evaluate_chain(
        &self,
        chain: &[Uuid],
        facts: Arc<RequestFacts>,
        type_id: GtsId,
        evaluated_at: OffsetDateTime,
        budget: EvaluationBudget,
    ) -> Result<EngineResult, EngineFailure> {
        let bindings = self
            .bindings
            .active_bindings(chain)
            .await
            .map_err(|e| port_failure(&e, "policy content"))?;
        let bindings = applying_in_order(chain, bindings);

        let (cached, missed) = self.cached_versions(&bindings);
        let content = if missed.is_empty() {
            HashMap::new()
        } else {
            self.bindings
                .version_documents(&missed)
                .await
                .map_err(|e| port_failure(&e, "policy content"))?
        };

        let cache = Arc::clone(&self.cache);
        let span = tracing::Span::current();
        let result = tokio::task::spawn_blocking(move || {
            let _span = span.entered();
            let compiled = compile_missed(&cache, cached, &missed, content)?;
            evaluate_bindings(&compiled, &bindings, &facts, &type_id, evaluated_at, budget)
        })
        .await
        .map_err(|e| task_failure(&e))??;

        if result.finding_count() > MAX_ENGINE_FINDINGS {
            tracing::warn!(
                findings = result.finding_count(),
                bound = MAX_ENGINE_FINDINGS,
                "policy-engine: findings past the admission bound dropped, shadow denials first"
            );
        }
        Ok(result.finish(reason::POLICY_DENIED))
    }

    /// Evaluates the built-in policies on a blocking task under `budget`:
    /// the first that denies, if any, and what is left of the budget.
    async fn evaluate_builtins(
        &self,
        type_id: &GtsId,
        facts: &Arc<RequestFacts>,
        evaluated_at: OffsetDateTime,
        mut budget: EvaluationBudget,
    ) -> Result<(Option<String>, EvaluationBudget), EngineFailure> {
        let builtins = Arc::clone(&self.builtins);
        let type_id = type_id.clone();
        let facts = Arc::clone(facts);
        let span = tracing::Span::current();
        tokio::task::spawn_blocking(move || {
            let _span = span.entered();
            let denial = builtins
                .first_denial(&type_id, &facts, evaluated_at, &mut budget)
                .map_err(|e| evaluation_failure(&e, "built-in policy evaluation"))?;
            Ok((denial, budget))
        })
        .await
        .map_err(|e| task_failure(&e))?
    }

    /// The resource tenant's chain, or `None` when the caller's tenant is not
    /// in it (not reachable with barriers respected).
    async fn chain(
        &self,
        ctx: &SecurityContext,
        caller_tenant: Uuid,
        resource_tenant: Uuid,
    ) -> Result<Option<Vec<Uuid>>, EngineFailure> {
        let chain = match self
            .hierarchy
            .ancestors(ctx, TenantId(resource_tenant))
            .await
        {
            Ok(chain) => chain,
            Err(PortError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(port_failure(&e, "tenant hierarchy")),
        };
        Ok(chain.contains(&caller_tenant).then_some(chain))
    }

    /// The cached compilations of the bindings' versions, and the versions
    /// the cache misses (each once).
    fn cached_versions(
        &self,
        bindings: &[ActiveBinding],
    ) -> (HashMap<VersionId, Arc<CompiledVersion>>, Vec<VersionId>) {
        let mut cached = HashMap::new();
        let mut missed = Vec::new();
        for binding in bindings {
            let id = binding.version_id;
            if cached.contains_key(&id) || missed.contains(&id) {
                continue;
            }
            let hit = self.cache.get(id);
            self.metrics.compile_cache(hit.is_some());
            if let Some(hit) = hit {
                cached.insert(id, hit);
            } else {
                missed.push(id);
            }
        }
        (cached, missed)
    }
}

/// The bindings that still apply - the owner tenant in the chain at or above
/// the target, i.e. reaching it with barriers respected - nearest target
/// first, then bundle id.
fn applying_in_order(chain: &[Uuid], bindings: Vec<ActiveBinding>) -> Vec<ActiveBinding> {
    let rank = |tenant: Uuid| chain.iter().position(|t| *t == tenant);
    let mut applying: Vec<(usize, ActiveBinding)> = bindings
        .into_iter()
        .filter_map(|b| {
            let target = rank(b.assignment.tenant_id)?;
            match rank(b.assignment.owner_tenant_id) {
                Some(owner) if owner >= target => Some((target, b)),
                _ => {
                    tracing::debug!(
                        assignment_id = %b.assignment.id,
                        "policy-engine: assignment dormant, its owner no longer reaches its target"
                    );
                    None
                }
            }
        })
        .collect();
    applying.sort_by_key(|(target, b)| (*target, b.assignment.bundle_id));
    applying.into_iter().map(|(_, b)| b).collect()
}

/// `cached` completed with the compilation of every `missed` version from
/// its `content`. Blocking: runs on the evaluation task.
fn compile_missed(
    cache: &CompileCache,
    mut cached: HashMap<VersionId, Arc<CompiledVersion>>,
    missed: &[VersionId],
    mut content: HashMap<VersionId, Vec<Document>>,
) -> Result<HashMap<VersionId, Arc<CompiledVersion>>, EngineFailure> {
    for &id in missed {
        let documents = content.remove(&id).unwrap_or_default();
        let compiled = cache
            .get_or_compile(id, || compile_content(&documents))
            .map_err(|e| {
                tracing::error!(version_id = %id, error = %e, "policy-engine: active version does not compile");
                EngineFailure::internal("active policy content does not compile")
            })?;
        cached.insert(id, compiled);
    }
    Ok(cached)
}

/// A dependency failure on the decision path, logged with its cause: the
/// [`EngineFailure`] detail carries only `what` failed.
fn port_failure(err: &PortError, what: &'static str) -> EngineFailure {
    tracing::warn!(dependency = what, error = %err, "policy-engine: dependency failed");
    match err {
        PortError::Timeout => EngineFailure::timeout(format!("{what} timed out")),
        PortError::Unavailable(_) | PortError::NotFound(_) => {
            EngineFailure::unavailable(format!("{what} unavailable"))
        }
    }
}

/// An evaluation that ran out of budget is a timeout; any other evaluation
/// failure is internal. Both are logged with their cause, which the
/// [`EngineFailure`] detail does not carry.
fn evaluation_failure(err: &EvaluatorFailure, what: &'static str) -> EngineFailure {
    tracing::error!(error = %err, "policy-engine: {what} failed");
    match err {
        EvaluatorFailure::Timeout { .. } => EngineFailure::timeout(format!("{what} timed out")),
        _ => EngineFailure::internal(format!("{what} failed")),
    }
}

fn task_failure(err: &tokio::task::JoinError) -> EngineFailure {
    tracing::error!(error = %err, "policy-engine: evaluation task failed");
    EngineFailure::internal("evaluation task failed")
}

fn evaluate_bindings(
    compiled: &HashMap<VersionId, Arc<CompiledVersion>>,
    bindings: &[ActiveBinding],
    facts: &RequestFacts,
    type_id: &GtsId,
    evaluated_at: OffsetDateTime,
    mut budget: EvaluationBudget,
) -> Result<EngineResultBuilder, EngineFailure> {
    let mut applicable = Vec::new();
    for binding in bindings {
        let Some(version) = compiled.get(&binding.version_id) else {
            tracing::error!(version_id = %binding.version_id, "policy-engine: active version not compiled");
            return Err(EngineFailure::internal(
                "active policy content does not compile",
            ));
        };
        for entry in version.applicable(type_id, &facts.action) {
            applicable.push(ApplicableDocument {
                compiled: Arc::clone(&entry.compiled),
                reference: PolicyReference {
                    bundle_id: binding.assignment.bundle_id.0,
                    version_id: binding.version_id.0,
                    document_id: entry.document_id.0,
                    document_name: entry.name.clone(),
                },
                enforce: binding.assignment.enforce,
            });
        }
    }
    evaluate_documents(&applicable, facts, evaluated_at, &mut budget)
        .map_err(|e| evaluation_failure(&e, "policy evaluation"))
}

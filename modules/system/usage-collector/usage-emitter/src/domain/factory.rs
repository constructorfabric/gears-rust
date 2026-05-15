use std::sync::Arc;
use std::time::Instant;

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::models::BarrierMode;
use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer};
use modkit_db::Db;
use modkit_db::outbox::Outbox;
use modkit_security::{SecurityContext, pep_properties};
use tokio::time::timeout;
use tracing::warn;
use usage_collector_sdk::UsageCollectorClientV1;
use usage_collector_sdk::authz;
use usage_collector_sdk::error::UsageRecordError;
use usage_collector_sdk::models::Subject;
use uuid::Uuid;

use crate::config::UsageEmitterConfig;
use crate::domain::emitter::UsageEmitter;
use crate::error::{UsageEmitterError, enforcer_error_to_emitter_error};

/// Build a [`Subject`] from a [`SecurityContext`].
///
/// Free function (not a `From` impl) because Rust orphan rules (E0117) forbid
/// `impl From<&SecurityContext> for Subject` here — both types are foreign to
/// this crate. Colocated with the factory since it is the sole consumer.
fn subject_from_ctx(ctx: &SecurityContext) -> Subject {
    Subject {
        id: ctx.subject_id(),
        r#type: ctx.subject_type().map(str::to_owned),
    }
}

/// Caller intent for the subject identity bound to the next `.authorize()` call.
///
/// Three legal states match the wire/PDP contract:
///
/// - `DefaultFromCtx` — neither `.with_subject(...)` nor `.without_subject()` was
///   called. The factory falls back to `SecurityContext`-derived subject at
///   `.authorize()` time. This is the default for in-process modules whose
///   caller identity *is* the subject.
/// - `Explicit(Some(s))` — caller passed an explicit subject via
///   `.with_subject(s)`. The PDP receives that exact subject; the gateway/
///   forwarder identity is never substituted.
/// - `Explicit(None)` — caller explicitly opted out via `.without_subject()`.
///   No `SUBJECT_ID`/`SUBJECT_TYPE` is sent to the PDP and the resulting
///   `UsageEmitter` has `subject = None`.
///
/// This three-state split is the only way to distinguish "default from
/// context" from "explicit no subject" — `Option<Subject>` collapses both
/// into the same `None`, which silently substitutes the gateway's own
/// `SecurityContext` subject and violates the forwarder-substitution
/// invariant.
#[derive(Clone, Debug, Default)]
pub enum SubjectChoice {
    /// Resolve from `SecurityContext` at `.authorize()` time.
    #[default]
    DefaultFromCtx,
    /// Caller-supplied: `Some(s)` = use exactly `s`; `None` = no subject at all.
    Explicit(Option<Subject>),
}

/// Module-bound producer of [`UsageEmitter`] handles.
///
/// Obtained from [`crate::api::UsageEmitterRuntimeV1::factory`]. Each module stores one
/// factory bound to its `MODULE_NAME` at init and clones it per call to apply tenant /
/// subject overrides via the fluent `.with_*()` chain. The terminal `.authorize()` runs
/// PDP authorization and fetches allowed metrics, returning a post-authorize handle.
///
/// `module` is fixed at construction time and cannot be overridden, preserving the
/// compile-time invariant that a factory cannot emit under the wrong module.
#[derive(Clone)]
pub struct UsageEmitterFactory {
    // shared Arcs cloned from the runtime
    pub(crate) config: Arc<UsageEmitterConfig>,
    pub(crate) db: Db,
    pub(crate) authz: Arc<dyn AuthZResolverClient>,
    pub(crate) collector: Arc<dyn UsageCollectorClientV1>,
    pub(crate) outbox: Arc<Outbox>,
    // immutable scope (set at construction by runtime.factory(name))
    pub(crate) module: String,
    // overridable scope (resolved at .authorize() time)
    pub(crate) tenant: Option<Uuid>,
    pub(crate) subject: SubjectChoice,
}

impl UsageEmitterFactory {
    /// Override the tenant for the next [`Self::authorize`] call.
    ///
    /// Without this, tenant defaults to `ctx.subject_tenant_id()`.
    #[must_use]
    pub fn with_tenant(mut self, t: Uuid) -> Self {
        self.tenant = Some(t);
        self
    }

    /// Bind an explicit caller-supplied [`Subject`] for the next [`Self::authorize`] call.
    ///
    /// The PDP receives this exact subject; gateways / forwarders MUST use this
    /// method (not the ctx-default fallback) so their own service identity is
    /// never substituted for the original caller's.
    #[must_use]
    pub fn with_subject(mut self, subject: Subject) -> Self {
        self.subject = SubjectChoice::Explicit(Some(subject));
        self
    }

    /// Explicitly emit without any subject identity for the next [`Self::authorize`] call.
    ///
    /// `SUBJECT_ID` / `SUBJECT_TYPE` are omitted from the PDP request and the
    /// resulting [`UsageEmitter`] has `subject = None`. Distinct from the
    /// default (neither `with_subject` nor `without_subject` called), which
    /// falls back to the `SecurityContext`-derived subject at authorize time.
    #[must_use]
    pub fn without_subject(mut self) -> Self {
        self.subject = SubjectChoice::Explicit(None);
        self
    }

    /// Run PDP authorization and fetch allowed metrics, returning a time-limited
    /// [`UsageEmitter`] handle bound to the module name, tenant, metered
    /// resource, and subject identity.
    ///
    /// Tenant defaults to `ctx.subject_tenant_id()` when not overridden via
    /// [`Self::with_tenant`]. Subject is resolved per the table in
    /// `RESEARCH-emitter-runtime.md`.
    ///
    /// # Errors
    ///
    /// Returns [`UsageEmitterError`] if PDP denies, the module is not configured, or the
    /// collector call fails.
    // @cpt-algo:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1
    #[tracing::instrument(
        skip_all,
        fields(
            module = %self.module,
            resource_id = %resource_id,
            resource_type = %resource_type,
        ),
    )]
    pub async fn authorize(
        &self,
        ctx: &SecurityContext,
        resource_id: Uuid,
        resource_type: &str,
    ) -> Result<UsageEmitter, UsageEmitterError> {
        // Resolve tenant: explicit override or default to ctx's home tenant.
        let tenant_id = self.tenant.unwrap_or_else(|| ctx.subject_tenant_id());

        // Resolve subject per the three-state intent (see SubjectChoice docs).
        let subject = match &self.subject {
            SubjectChoice::Explicit(s) => s.clone(),
            SubjectChoice::DefaultFromCtx => Some(subject_from_ctx(ctx)),
        };

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-2
        // PDP request shape — both opt-outs are deliberate and load-bearing:
        //
        // * `require_constraints(false)` — usage emission is authorize-once-per-call-site
        //   and re-validates module / tenant / resource / subject / allowed-metrics
        //   in-memory at every `UsageEmitter::enqueue_in` (see `emitter.rs` validators).
        //   The PDP's role here is the allow/deny decision; it does not return additional
        //   constraint expressions for the emitter to apply later. Asking for constraints
        //   we never consume would incur compilation cost on every emit for no policy
        //   benefit. (Contrast: the query path on the gateway *does* enforce returned
        //   constraints as filters — see DESIGN.md §"Authorize each query".)
        //
        // * `BarrierMode::Ignore` — the emitter is invoked by trusted source modules (and
        //   the gateway forwarder) emitting usage *for* a target tenant that may be a
        //   self-managed / barriered sub-tenant of the caller's context. With
        //   `BarrierMode::Respect`, a self-managed child barrier would hide its ancestor
        //   chain from the PDP's tenant traversal and deny all cross-barrier emit — even
        //   though the caller is a platform-level component (gateway / system job)
        //   acting on the platform's behalf rather than a tenant user. The PDP itself
        //   remains the sole authority: the allow/deny still depends on the policy
        //   evaluation of OWNER_TENANT_ID, RESOURCE_ID/TYPE, MODULE, and SUBJECT_ID/TYPE
        //   properties; ignoring the barrier only widens the candidate evaluation set,
        //   it does not bypass the decision. See `tests/emitter_tests.rs::
        //   authorize_sends_barrier_mode_ignore_to_pdp` and
        //   `authorize_allows_subtenant_when_pdp_allows_extra_tenant` for the contract.
        let mut request = AccessRequest::new()
            .require_constraints(false)
            .barrier_mode(BarrierMode::Ignore)
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
            .resource_property(authz::properties::RESOURCE_ID, resource_id)
            .resource_property(authz::properties::RESOURCE_TYPE, resource_type)
            .resource_property(authz::properties::MODULE, self.module.clone());
        if let Some(s) = &subject {
            request = request.resource_property(authz::properties::SUBJECT_ID, s.id.to_string());
            if let Some(ty) = &s.r#type {
                request = request.resource_property(authz::properties::SUBJECT_TYPE, ty.as_str());
            }
        }
        let enforcer = PolicyEnforcer::new(Arc::clone(&self.authz));
        let pdp_call = enforcer.access_scope_with(
            ctx,
            &authz::USAGE_RECORD,
            authz::actions::CREATE,
            None,
            &request,
        );
        let Ok(pdp_result) = timeout(self.config.authorize_call_timeout, pdp_call).await else {
            warn!(
                tenant_id = %tenant_id,
                timeout = ?self.config.authorize_call_timeout,
                "PDP authorization call exceeded authorize_call_timeout",
            );
            return Err(UsageRecordError::deadline_exceeded(
                "PDP authorization call exceeded authorize_call_timeout",
            )
            .create());
        };
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-2

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-3
        if let Err(e) = pdp_result {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-3a
            return Err(enforcer_error_to_emitter_error(e));
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-3a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-3

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-4
        // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-1
        let Ok(module_cfg_result) = timeout(
            self.config.authorize_call_timeout,
            self.collector.get_module_config(&self.module),
        )
        .await
        else {
            warn!(
                tenant_id = %tenant_id,
                timeout = ?self.config.authorize_call_timeout,
                "collector get_module_config call exceeded authorize_call_timeout",
            );
            return Err(UsageRecordError::deadline_exceeded(
                "collector get_module_config call exceeded authorize_call_timeout",
            )
            .create());
        };
        // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-1
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-4

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-5
        let module_cfg = module_cfg_result?;
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-5

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-6
        let authorized = UsageEmitter {
            config: Arc::clone(&self.config),
            db: self.db.clone(),
            outbox: Arc::clone(&self.outbox),
            module: self.module.clone(),
            tenant_id,
            resource_id,
            resource_type: resource_type.to_owned(),
            allowed_metrics: module_cfg.allowed_metrics,
            max_metadata_bytes: module_cfg.max_metadata_bytes,
            subject,
            issued_at: Instant::now(),
        };
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-6

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-7
        Ok(authorized)
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for:p1:inst-authz-7
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "factory_tests.rs"]
mod factory_tests;

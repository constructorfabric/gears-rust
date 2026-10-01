//! The admission service: the `admit` sequence.
//!
//! 1. An anonymous context (nil subject or subject tenant) is `unauthenticated`.
//! 2. `enforcing_gear`, `action` and `resource_type` are validated; an invalid
//!    one is `invalid_argument` (the value is never echoed).
//! 3. The size bounds are checked: too large is a refusal.
//! 4. A correlation identifier is minted.
//! 5. Built-in policies run; the first denial is a refusal.
//! 6. The engine is called under the engine timeout; its result or failure is
//!    mapped. No engine, or any failure, is a refusal (fail closed).
//! 7. Refusal and shadow events are emitted (best effort) and the verdict is
//!    returned.

use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use admission_control_sdk::{
    Admission, AdmissionEnginePluginClientV1, AdmissionError, AdmissionRequest, EngineRequest,
    EngineResult, FailureCondition, PolicyReference, Refusal, RefusalCause, RefusalEvent,
    SizeBound, Verdict, validate_identifier, validate_resource_type,
};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::builtin::{BuiltinOutcome, BuiltinPolicySet};
use crate::infra::metrics::AdmissionControlMetrics;

/// Where refusal events go. Publication is best effort and never blocks or
/// fails an admission.
pub trait EventSink: Send + Sync {
    /// Hands one event over; drops it when it cannot be queued.
    fn emit(&self, event: RefusalEvent);
}

/// The selected engine: its GTS instance id and its plugin client.
#[domain_model]
#[derive(Clone)]
pub struct EngineHandle {
    /// GTS instance identifier of the engine plugin.
    pub id: String,
    /// The plugin client.
    pub plugin: Arc<dyn AdmissionEnginePluginClientV1>,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Settings of the service, from the gear config.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ServiceSettings {
    /// Engine call bound.
    pub engine_timeout: Duration,
    /// Per-policy built-in evaluation bound.
    pub builtin_timeout: Duration,
    /// Largest number of properties per request.
    pub max_properties: usize,
    /// Largest serialized size of a request's properties, in bytes.
    pub max_context_bytes: usize,
}

/// The admission service.
#[domain_model]
pub struct AdmissionService {
    builtins: BuiltinPolicySet,
    settings: ServiceSettings,
    engine: OnceLock<Option<EngineHandle>>,
    events: Arc<dyn EventSink>,
    metrics: Arc<AdmissionControlMetrics>,
}

impl std::fmt::Debug for AdmissionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionService")
            .field("builtins", &self.builtins.len())
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// The decision before events are emitted.
enum Judgment {
    Permit {
        shadow: Vec<PolicyReference>,
    },
    Refuse {
        cause: RefusalCause,
        shadow: Vec<PolicyReference>,
    },
}

impl Judgment {
    fn refuse(cause: RefusalCause) -> Self {
        Self::Refuse {
            cause,
            shadow: Vec::new(),
        }
    }

    fn could_not_run(condition: FailureCondition) -> Self {
        Self::refuse(RefusalCause::CouldNotRun { condition })
    }
}

impl AdmissionService {
    /// A service with no engine installed yet (every call that reaches the
    /// engine step is refused with `NoEngine` until [`Self::install_engine`]).
    #[must_use]
    pub fn new(
        builtins: BuiltinPolicySet,
        settings: ServiceSettings,
        events: Arc<dyn EventSink>,
        metrics: Arc<AdmissionControlMetrics>,
    ) -> Self {
        Self {
            builtins,
            settings,
            engine: OnceLock::new(),
            events,
            metrics,
        }
    }

    /// Installs the engine resolved in the serve phase (`None`: explicitly no
    /// engine). The first installation wins.
    pub fn install_engine(&self, engine: Option<EngineHandle>) {
        if self.engine.set(engine).is_err() {
            tracing::warn!("admission-control: engine already installed");
        }
    }

    /// Admits or refuses one operation.
    ///
    /// # Errors
    ///
    /// `unauthenticated` for an anonymous context; `invalid_argument` for an
    /// invalid identifier. Every decided or failed check is an `Ok` refusal.
    pub async fn admit(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
    ) -> Result<Verdict, AdmissionError> {
        if ctx.is_anonymous() {
            tracing::warn!("admission refused: call without a security context");
            return Err(CanonicalError::unauthenticated()
                .with_reason("MISSING_CONTEXT")
                .create());
        }
        validate_identifier("enforcing_gear", &request.enforcing_gear)
            .and_then(|()| validate_identifier("action", &request.action))
            .and_then(|()| validate_resource_type(&request.resource_type))
            .inspect_err(|_| {
                tracing::warn!(
                    "admission refused: enforcing_gear, action or resource_type is not valid"
                );
            })?;

        let correlation_id = Uuid::new_v4();
        let occurred_at = OffsetDateTime::now_utc();
        let judgment = self.judge(ctx, request, correlation_id, occurred_at).await;
        Ok(self.conclude(ctx, request, correlation_id, occurred_at, judgment))
    }

    async fn judge(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
        correlation_id: Uuid,
        occurred_at: OffsetDateTime,
    ) -> Judgment {
        if let Some(bound) = self.exceeded_bound(request) {
            return Judgment::refuse(RefusalCause::RequestTooLarge { bound });
        }
        let subject = (ctx.subject_id(), ctx.subject_tenant_id());
        match self
            .builtins
            .evaluate(request, subject, occurred_at, self.settings.builtin_timeout)
        {
            BuiltinOutcome::NoProhibition => {}
            BuiltinOutcome::Prohibited { policy_id } => {
                return Judgment::refuse(RefusalCause::BuiltinPolicy { policy_id });
            }
            BuiltinOutcome::Failed { .. } => {
                return Judgment::could_not_run(FailureCondition::BuiltinPolicyFailure);
            }
        }
        self.consult_engine(ctx, request, correlation_id).await
    }

    async fn consult_engine(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
        correlation_id: Uuid,
    ) -> Judgment {
        let Some(engine) = self.engine.get().and_then(Option::as_ref) else {
            return Judgment::could_not_run(FailureCondition::NoEngine);
        };
        let engine_request = EngineRequest::from_admission(request, correlation_id);
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            self.settings.engine_timeout,
            engine.plugin.evaluate(ctx, &engine_request),
        )
        .await;
        self.metrics.engine_latency(started.elapsed());
        match result {
            Err(_elapsed) => Judgment::could_not_run(FailureCondition::EngineTimeout),
            Ok(Ok(EngineResult::Permit { shadow_denials })) => Judgment::Permit {
                shadow: shadow_denials,
            },
            Ok(Ok(EngineResult::Deny {
                reason_code,
                denials,
                shadow_denials,
            })) => Judgment::Refuse {
                cause: RefusalCause::Policy {
                    reason_code,
                    denials,
                },
                shadow: shadow_denials,
            },
            Ok(Err(failure)) => {
                tracing::warn!(
                    engine_id = %engine.id,
                    condition = failure.condition.as_str(),
                    detail = %failure.detail,
                    "admission engine failed"
                );
                Judgment::could_not_run(failure.condition.failure_condition())
            }
        }
    }

    fn exceeded_bound(&self, request: &AdmissionRequest) -> Option<SizeBound> {
        if request.properties.len() > self.settings.max_properties {
            return Some(SizeBound::PropertyCount);
        }
        // A writer that stops at the bound: an oversized context costs at most
        // the bound to reject.
        let mut budget = ByteBudget {
            remaining: self.settings.max_context_bytes,
        };
        match serde_json::to_writer(&mut budget, &request.properties) {
            Ok(()) => None,
            Err(_) => Some(SizeBound::ContextBytes),
        }
    }

    /// Emits the events and builds the verdict.
    fn conclude(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
        correlation_id: Uuid,
        occurred_at: OffsetDateTime,
        judgment: Judgment,
    ) -> Verdict {
        let names = if matches!(
            judgment,
            Judgment::Refuse {
                cause: RefusalCause::RequestTooLarge { .. },
                ..
            }
        ) {
            Vec::new()
        } else {
            request.properties.keys().cloned().collect()
        };
        let base = RefusalEvent {
            correlation_id,
            occurred_at,
            enforcing_gear: request.enforcing_gear.clone(),
            action: request.action.clone(),
            resource_type: request.resource_type.clone(),
            resource_id: request.resource_id,
            resource_tenant_id: request.resource_tenant_id,
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
            enforced: true,
            cause: admission_control_sdk::RefusalEventCause::Policy,
            builtin_policy_id: None,
            condition: None,
            policy: None,
            property_names: names,
        };
        let shadow = match &judgment {
            Judgment::Permit { shadow } | Judgment::Refuse { shadow, .. } => shadow,
        };
        for denial in shadow {
            self.events.emit(RefusalEvent {
                enforced: false,
                policy: Some(denial.clone()),
                ..base.clone()
            });
        }
        match judgment {
            Judgment::Permit { .. } => {
                self.metrics.verdict("admitted");
                Verdict::Admitted(Admission { correlation_id })
            }
            Judgment::Refuse { cause, .. } => {
                self.emit_refusal(&base, &cause);
                self.metrics.verdict(cause_label(&cause));
                Verdict::Refused(Refusal {
                    cause,
                    correlation_id,
                })
            }
        }
    }

    /// One event per (operation, policy) pair.
    fn emit_refusal(&self, base: &RefusalEvent, cause: &RefusalCause) {
        let event = |fields: RefusalEvent| self.events.emit(fields);
        let cause_kind = cause.into();
        match cause {
            RefusalCause::Policy { denials, .. } if !denials.is_empty() => {
                for denial in denials {
                    event(RefusalEvent {
                        cause: cause_kind,
                        policy: Some(denial.clone()),
                        ..base.clone()
                    });
                }
            }
            RefusalCause::BuiltinPolicy { policy_id } => event(RefusalEvent {
                cause: cause_kind,
                builtin_policy_id: Some(policy_id.clone()),
                ..base.clone()
            }),
            RefusalCause::CouldNotRun { condition } => event(RefusalEvent {
                cause: cause_kind,
                condition: Some(*condition),
                ..base.clone()
            }),
            RefusalCause::Policy { .. } | RefusalCause::RequestTooLarge { .. } => {
                event(RefusalEvent {
                    cause: cause_kind,
                    ..base.clone()
                });
            }
        }
    }
}

fn cause_label(cause: &RefusalCause) -> &'static str {
    match cause {
        RefusalCause::BuiltinPolicy { .. } => "builtin_policy",
        RefusalCause::Policy { .. } => "policy",
        RefusalCause::RequestTooLarge { .. } => "request_too_large",
        RefusalCause::CouldNotRun { .. } => "could_not_run",
    }
}

/// A sink that accepts at most `remaining` bytes.
struct ByteBudget {
    remaining: usize,
}

impl io::Write for ByteBudget {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.remaining.checked_sub(buf.len()) {
            Some(remaining) => {
                self.remaining = remaining;
                Ok(buf.len())
            }
            None => Err(io::Error::other("context bytes bound exceeded")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

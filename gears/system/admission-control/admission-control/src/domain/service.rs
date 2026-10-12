//! The admission service: the `admit` sequence.
//!
//! 1. An anonymous context (nil subject or subject tenant) is `unauthenticated`.
//! 2. `enforcing_gear`, `action` and `resource_type` are validated; an invalid
//!    one is `invalid_argument` (the value is never echoed).
//! 3. The size bounds are checked: too large is a refusal.
//! 4. A correlation identifier is minted, and the rest runs in an `admission`
//!    span carrying it with the request's (now validated) identifiers, so every
//!    log line of the decision is tied to it and to the caller's trace.
//! 5. The engine is resolved (on first use; an engine that cannot be resolved
//!    now is `engine_unavailable`) and called, both under the engine timeout;
//!    its result or failure is mapped. No engine, or any failure, is a refusal
//!    (fail closed). A result
//!    past the bounds on what an engine may return (reason code, document
//!    names, number of findings) is a contract defect: could-not-run
//!    `internal`.
//! 6. Refusal and shadow events are emitted (best effort) and the verdict is
//!    returned.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::{
    Admission, AdmissionEnginePluginClientV1, AdmissionError, AdmissionRequest, EngineRequest,
    EngineResult, FailureCondition, PROPERTY_MAX_DEPTH, PolicyReference, Refusal, RefusalCause,
    RefusalEvent, RefusalEventCause, SizeBound, Verdict, validate_identifier,
    validate_resource_type,
};
use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

/// Most denials plus shadow denials one engine result may carry: each becomes
/// an event.
pub const MAX_ENGINE_FINDINGS: usize = 64;

/// Longest engine reason code, in bytes. It is returned to the calling gear.
pub const MAX_REASON_CODE_LEN: usize = 128;

/// Longest policy document name, in bytes. It is copied into events.
pub const MAX_DOCUMENT_NAME_LEN: usize = 256;

/// The broker-envelope fields every event of one operation is published
/// under. They are not repeated in the event's payload.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventEnvelope {
    /// Correlation identifier the gate minted: the events' subject.
    pub correlation_id: Uuid,
    /// Instant the operation was decided.
    pub occurred_at: OffsetDateTime,
    /// Tenant owning the target resource: the events' tenant.
    pub tenant_id: Uuid,
}

/// Where refusal events go. Publication is best effort and never blocks or
/// fails an admission.
pub trait EventSink: Send + Sync {
    /// Hands one event over; drops it when it cannot be queued.
    fn emit(&self, envelope: EventEnvelope, event: RefusalEvent);
}

/// Where the service records its decisions and engine call latency.
pub trait AdmissionMetrics: Send + Sync {
    /// One decision, labelled `admitted` or by refusal cause.
    fn verdict(&self, cause: &'static str);
    /// Wall time of one engine call.
    fn engine_latency(&self, elapsed: Duration);
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

/// The configured engine could not be resolved for this call; the
/// implementation logs why.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineUnavailable;

/// Where the configured engine comes from: resolved on first use, so a call
/// that arrives before the serve phase still reaches it.
#[async_trait]
pub trait EngineSource: Send + Sync {
    /// The engine to consult.
    ///
    /// # Errors
    ///
    /// [`EngineUnavailable`] when the engine cannot be resolved now; the next
    /// call tries again.
    async fn engine(&self) -> Result<EngineHandle, EngineUnavailable>;
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
    /// Largest number of properties per request.
    pub max_properties: usize,
    /// Largest serialized size of a request's properties, in bytes.
    pub max_context_bytes: usize,
}

/// The admission service.
#[domain_model]
pub struct AdmissionService {
    settings: ServiceSettings,
    /// `None`: no engine configured.
    engine: Option<Arc<dyn EngineSource>>,
    events: Arc<dyn EventSink>,
    metrics: Arc<dyn AdmissionMetrics>,
}

impl std::fmt::Debug for AdmissionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionService")
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
    /// A service consulting `engine` (`None`: no engine configured, every
    /// call that reaches the engine step is refused with `NoEngine`).
    #[must_use]
    pub fn new(
        settings: ServiceSettings,
        engine: Option<Arc<dyn EngineSource>>,
        events: Arc<dyn EventSink>,
        metrics: Arc<dyn AdmissionMetrics>,
    ) -> Self {
        Self {
            settings,
            engine,
            events,
            metrics,
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
        let span = tracing::info_span!(
            "admission",
            %correlation_id,
            enforcing_gear = %request.enforcing_gear,
            action = %request.action,
            resource_type = %request.resource_type,
        );
        async {
            let judgment = self.judge(ctx, request, correlation_id).await;
            Ok(self.conclude(ctx, request, correlation_id, occurred_at, judgment))
        }
        .instrument(span)
        .await
    }

    async fn judge(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
        correlation_id: Uuid,
    ) -> Judgment {
        if let Some(bound) = self.exceeded_bound(request) {
            return Judgment::refuse(RefusalCause::RequestTooLarge { bound });
        }
        self.consult_engine(ctx, request, correlation_id).await
    }

    async fn consult_engine(
        &self,
        ctx: &SecurityContext,
        request: &AdmissionRequest,
        correlation_id: Uuid,
    ) -> Judgment {
        // One deadline for resolving the engine (on first use) and calling it.
        let started = tokio::time::Instant::now();
        let deadline = started + self.settings.engine_timeout;
        let engine = match self.resolve_engine(deadline).await {
            Ok(engine) => engine,
            Err(judgment) => return judgment,
        };
        let engine_request = EngineRequest::from_admission(request, correlation_id);
        let result =
            tokio::time::timeout_at(deadline, engine.plugin.evaluate(ctx, &engine_request)).await;
        self.metrics.engine_latency(started.elapsed());
        match result {
            Err(_elapsed) => self.timed_out(&engine.id),
            Ok(Ok(result)) => accept_engine_result(&engine, result),
            Ok(Err(failure)) => {
                tracing::warn!(
                    engine_id = %engine.id,
                    condition = failure.condition.as_str(),
                    detail = %failure.detail,
                    "admission engine failed"
                );
                Judgment::refuse(failure.condition.refusal_cause())
            }
        }
    }

    /// The engine to consult by `deadline`, or the refusal when there is
    /// none: `NoEngine` when none is configured, `EngineUnavailable` when it
    /// cannot be resolved now, `EngineTimeout` past the deadline.
    async fn resolve_engine(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<EngineHandle, Judgment> {
        let Some(source) = &self.engine else {
            // No engine configured: the serve phase warned once already.
            tracing::debug!("admission refused: no engine configured");
            return Err(Judgment::could_not_run(FailureCondition::NoEngine));
        };
        match tokio::time::timeout_at(deadline, source.engine()).await {
            Ok(Ok(engine)) => Ok(engine),
            Ok(Err(EngineUnavailable)) => {
                tracing::debug!("admission refused: engine unavailable");
                Err(Judgment::could_not_run(FailureCondition::EngineUnavailable))
            }
            Err(_elapsed) => Err(self.timed_out("unresolved")),
        }
    }

    fn timed_out(&self, engine_id: &str) -> Judgment {
        tracing::warn!(
            engine_id,
            timeout_ms =
                u64::try_from(self.settings.engine_timeout.as_millis()).unwrap_or(u64::MAX),
            "admission engine timed out"
        );
        Judgment::could_not_run(FailureCondition::EngineTimeout)
    }

    fn exceeded_bound(&self, request: &AdmissionRequest) -> Option<SizeBound> {
        if request.properties.len() > self.settings.max_properties {
            return Some(SizeBound::PropertyCount);
        }
        // Before anything recursive walks the properties (serialisation below,
        // the clone into the engine request, the engine's own processing): a
        // value nested deeply enough overflows the stack instead of being
        // refused.
        if exceeds_depth(&request.properties, PROPERTY_MAX_DEPTH) {
            return Some(SizeBound::ContextDepth);
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
        let envelope = EventEnvelope {
            correlation_id,
            occurred_at,
            tenant_id: request.resource_tenant_id,
        };
        let base = RefusalEvent {
            enforcing_gear: request.enforcing_gear.clone(),
            action: request.action.clone(),
            resource_type: request.resource_type.clone(),
            resource_id: request.resource_id,
            subject_id: ctx.subject_id(),
            subject_tenant_id: ctx.subject_tenant_id(),
            enforced: true,
            cause: RefusalEventCause::Policy,
            condition: None,
            policy: None,
            property_names: names,
        };
        let shadow = match &judgment {
            Judgment::Permit { shadow } | Judgment::Refuse { shadow, .. } => shadow,
        };
        for denial in shadow {
            self.events.emit(
                envelope,
                RefusalEvent {
                    enforced: false,
                    policy: Some(denial.clone()),
                    ..base.clone()
                },
            );
        }
        match judgment {
            Judgment::Permit { .. } => {
                self.metrics.verdict("admitted");
                Verdict::Admitted(Admission { correlation_id })
            }
            Judgment::Refuse { cause, .. } => {
                self.emit_refusal(envelope, &base, &cause);
                self.metrics
                    .verdict(RefusalEventCause::from(&cause).as_str());
                Verdict::Refused(Refusal {
                    cause,
                    correlation_id,
                })
            }
        }
    }

    /// One event per (operation, policy) pair.
    fn emit_refusal(&self, envelope: EventEnvelope, base: &RefusalEvent, cause: &RefusalCause) {
        let event = |fields: RefusalEvent| self.events.emit(envelope, fields);
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
            RefusalCause::CouldNotRun { condition } => event(RefusalEvent {
                cause: cause_kind,
                condition: Some(*condition),
                ..base.clone()
            }),
            // A size refusal, a policy refusal naming no document, and any
            // cause added later: one event carrying the cause alone.
            _ => event(RefusalEvent {
                cause: cause_kind,
                ..base.clone()
            }),
        }
    }
}

/// The judgment for an engine's answer. An answer this gate does not know,
/// or one past the bounds, breaks the gate–engine contract and is
/// could-not-run `internal`.
fn accept_engine_result(engine: &EngineHandle, result: EngineResult) -> Judgment {
    let (denial, shadow) = match result {
        EngineResult::Permit { shadow_denials } => (None, shadow_denials),
        EngineResult::Deny {
            reason_code,
            denials,
            shadow_denials,
        } => (Some((reason_code, denials)), shadow_denials),
        _ => {
            tracing::warn!(
                engine_id = %engine.id,
                "admission engine answered with a result this gate does not know"
            );
            return Judgment::could_not_run(FailureCondition::Internal);
        }
    };
    let (reason_code, denials) = match &denial {
        Some((reason_code, denials)) => (Some(reason_code.as_str()), &denials[..]),
        None => (None, &[][..]),
    };
    if let Some(violation) = out_of_bounds(reason_code, denials, &shadow) {
        // The offending value is not logged: it is engine output that failed
        // the bounds meant to keep it out of records.
        tracing::warn!(
            engine_id = %engine.id,
            violation,
            "admission engine result out of bounds"
        );
        return Judgment::could_not_run(FailureCondition::Internal);
    }
    match denial {
        None => Judgment::Permit { shadow },
        Some((reason_code, denials)) => Judgment::Refuse {
            cause: RefusalCause::Policy {
                reason_code,
                denials,
            },
            shadow,
        },
    }
}

/// Which bound an engine result violates, if any: at most
/// [`MAX_ENGINE_FINDINGS`] findings, a reason code of 1 to
/// [`MAX_REASON_CODE_LEN`] printable ASCII bytes, and document names of 1 to
/// [`MAX_DOCUMENT_NAME_LEN`] bytes without control characters.
fn out_of_bounds(
    reason_code: Option<&str>,
    denials: &[PolicyReference],
    shadow: &[PolicyReference],
) -> Option<&'static str> {
    if denials.len().saturating_add(shadow.len()) > MAX_ENGINE_FINDINGS {
        return Some("too_many_findings");
    }
    if reason_code.is_some_and(|code| {
        code.is_empty()
            || code.len() > MAX_REASON_CODE_LEN
            || !code.bytes().all(|b| b.is_ascii_graphic())
    }) {
        return Some("reason_code");
    }
    let bad_name = |reference: &PolicyReference| {
        let name = &reference.document_name;
        name.is_empty() || name.len() > MAX_DOCUMENT_NAME_LEN || name.chars().any(char::is_control)
    };
    if denials.iter().chain(shadow).any(bad_name) {
        return Some("document_name");
    }
    None
}

/// Whether any value in `properties` is nested deeper than `max` (a property's
/// own value is at depth 1). Iterative, so it cannot itself overflow the stack.
fn exceeds_depth(properties: &serde_json::Map<String, serde_json::Value>, max: usize) -> bool {
    let mut pending: Vec<(&serde_json::Value, usize)> =
        properties.values().map(|value| (value, 1)).collect();
    while let Some((value, depth)) = pending.pop() {
        if depth > max {
            return true;
        }
        match value {
            serde_json::Value::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            serde_json::Value::Object(map) => {
                pending.extend(map.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    false
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

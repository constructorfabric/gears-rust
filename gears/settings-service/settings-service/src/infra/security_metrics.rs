// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-telemetry:p1
//! The enforcement points', the Secret Manager's and the audit sink's
//! security metrics, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};

use crate::domain::metric_labels::{AuditKind, AuthzReason, Operation, SecretUseResult};
use crate::domain::ports::SecurityMetrics;

/// `settings_authz_denial_total` by `operation` and `reason`,
/// `settings_secret_use_total` by `result`, and `settings_audit_events_total`
/// by `kind`.
///
/// The labels are finite sets the DESIGN names; no label carries a key, a
/// tenant, an actor, a handle or a value, and nothing the caller supplied
/// reaches a label at all — the port takes `&'static str`.
pub struct OtelSecurityMetrics {
    denials: Counter<u64>,
    secret_uses: Counter<u64>,
    audit_events: Counter<u64>,
}

impl OtelSecurityMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        Self::on(&opentelemetry::global::meter("settings-service"))
    }

    /// On `meter`: the gear's in production, a test's own provider in tests.
    #[must_use]
    pub fn on(meter: &Meter) -> Self {
        Self {
            denials: meter
                .u64_counter("settings_authz_denial_total")
                .with_description("Authorization refusals at an enforcement point")
                .build(),
            secret_uses: meter
                .u64_counter("settings_secret_use_total")
                .with_description("Machine secret-use resolutions, by result")
                .build(),
            audit_events: meter
                .u64_counter("settings_audit_events_total")
                .with_description("Audit records appended, by kind")
                .build(),
        }
    }
}

impl Default for OtelSecurityMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityMetrics for OtelSecurityMetrics {
    fn authz_denial(&self, operation: Operation, reason: AuthzReason) {
        self.denials.add(
            1,
            &[
                KeyValue::new("operation", operation.as_str()),
                KeyValue::new("reason", reason.as_str()),
            ],
        );
    }

    fn secret_use(&self, result: SecretUseResult) {
        self.secret_uses
            .add(1, &[KeyValue::new("result", result.as_str())]);
    }

    fn audit_event(&self, kind: AuditKind) {
        self.audit_events
            .add(1, &[KeyValue::new("kind", kind.as_str())]);
    }
}

#[cfg(test)]
#[path = "security_metrics_tests.rs"]
mod security_metrics_tests;

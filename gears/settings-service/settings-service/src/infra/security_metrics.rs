// @cpt-dod:cpt-cf-settings-service-dod-gear-foundation-telemetry:p1
//! The enforcement points', the Secret Manager's and the audit sink's
//! security metrics, on the process's meter.

use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

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
        let meter = opentelemetry::global::meter("settings-service");
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
    fn authz_denial(&self, operation: &'static str, reason: &'static str) {
        self.denials.add(
            1,
            &[
                KeyValue::new("operation", operation),
                KeyValue::new("reason", reason),
            ],
        );
    }

    fn secret_use(&self, result: &'static str) {
        self.secret_uses.add(1, &[KeyValue::new("result", result)]);
    }

    fn audit_event(&self, kind: &'static str) {
        self.audit_events.add(1, &[KeyValue::new("kind", kind)]);
    }
}

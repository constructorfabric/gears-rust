use crate::domain::admission::{AdmissionEvents, SensitiveDataEvent};

/// [`AdmissionEvents`] written to the structured log, until the Audit gear
/// ships. An event names the sensitive-data kind and never the value, so the
/// data protection officer can count blocks and redactions per kind.
///
/// @cpt-dod:cpt-cf-construct-dod-admission-events:p1
#[derive(Debug, Default)]
pub struct LogAdmissionEvents;

fn log(action: &'static str, event: &SensitiveDataEvent) {
    // @cpt-begin:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-log
    tracing::warn!(
        target: "construct::audit",
        event = action,
        tenant_id = %event.tenant_id,
        subject_id = %event.subject_id,
        origin = event.origin.kind(),
        actor = %event.origin.actor(),
        kind = event.kind.code(),
        "sensitive data enforced"
    );
    // @cpt-end:cpt-cf-construct-algo-admission-enforce:p1:inst-enforce-log
}

impl AdmissionEvents for LogAdmissionEvents {
    fn blocked(&self, event: SensitiveDataEvent) {
        log("value_blocked", &event);
    }

    fn redacted(&self, event: SensitiveDataEvent) {
        log("value_redacted", &event);
    }
}

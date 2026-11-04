//! Structured audit records on the `event_broker.events` target.
//!
//! Each record is a `tracing::info!` event on that dedicated target, with a
//! stable `kind` and the acting principal's `actor_*` fields, the same shape
//! the other gears' audit streams use (`am.events`,
//! `keycloak_idp_plugin.events`). A subscriber routes the target to the audit
//! sink; nothing here depends on which one.

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::producer_reset::ProducerResetScope;

/// The `actor_*` fields of every record: `(subject_id, subject_type,
/// subject_tenant_id)`. `subject_type` is the context's verbatim string, or
/// `"anonymous"` when the context records none.
fn actor_fields(actor: &SecurityContext) -> (Uuid, &str, Uuid) {
    (
        actor.subject_id(),
        actor.subject_type().unwrap_or("anonymous"),
        actor.subject_tenant_id(),
    )
}

/// `kind = "producer.chain_reset"` - fired once per successful
/// `POST /v1/producers/{id}:reset` (or the in-process
/// `EventBrokerApi::reset_producer_chain`), after the chain rows are gone.
///
/// `scope` is `"all"` with `topic = "*"` for a reset of every topic, or
/// `"topic"` with the topic id the request named.
pub fn emit_producer_chain_reset(
    actor: &SecurityContext,
    producer_id: Uuid,
    scope: &ProducerResetScope,
) {
    let (actor_subject_id, actor_subject_type, actor_tenant_id) = actor_fields(actor);
    let (scope, topic) = match scope {
        ProducerResetScope::All => ("all", "*"),
        ProducerResetScope::Topic { topic } => ("topic", topic.as_str()),
    };
    tracing::info!(
        target: "event_broker.events",
        kind = "producer.chain_reset",
        %actor_subject_id,
        actor_subject_type,
        %actor_tenant_id,
        %producer_id,
        scope,
        topic,
        "producer chain reset"
    );
}

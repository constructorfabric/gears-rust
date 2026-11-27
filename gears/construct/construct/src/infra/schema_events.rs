use crate::domain::schema_service::{NodesForgotten, SchemaChanged, SchemaEvents};

/// Writes the changes of schemas to the audit log, as the other events of Construct go.
pub struct LogSchemaEvents;

impl SchemaEvents for LogSchemaEvents {
    fn changed(&self, event: SchemaChanged) {
        tracing::info!(
            target: "construct::audit",
            event = "profile_schema_changed",
            tenant_id = %event.tenant_id,
            subject_id = %event.subject_id,
            caller = %event.caller,
            changes = event.changes,
            removed_kinds = ?event.removed_kinds,
            version = event.version,
            "construct: a profile schema was changed"
        );
    }

    fn nodes_forgotten(&self, event: NodesForgotten) {
        tracing::info!(
            target: "construct::audit",
            event = "profile_schema_nodes_forgotten",
            tenant_id = %event.tenant_id,
            subject_id = %event.subject_id,
            nodes = event.nodes,
            "nodes of a kind the person's schema no longer has were forgotten"
        );
    }
}

//! Permission labels and reason identities. These schemas grant no authority.
pub mod permissions;
use bss_orders_lifecycle_sdk::catalog::{OPERATIONS, Reason};
pub use permissions::{ORDER, PERMISSIONS, Permission};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use toolkit_gts::gts_id;

fn entities() -> Vec<Value> {
    let mut schemas: BTreeSet<&str> = PERMISSIONS.iter().map(|p| p.resource).collect();
    schemas.insert(gts_id!("cf.bss.orders.err.v1~"));
    schemas.extend(Reason::ALL.iter().map(|r| r.mapping().gts_id));
    let mut values: Vec<Value> = schemas.into_iter().map(|id| json!({
        "$id": format!("gts://{id}"), "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "title": id,
    })).collect();
    values.push(json!({
        "$id": format!("gts://{}", gts_id!("cf.bss.orders.category.v1~")),
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object", "required": ["id"],
        "properties": {"id": {"type": "string"}},
    }));
    // Registration is not admission: change stays refused by capture in this phase.
    for id in [
        gts_id!("cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1"),
        gts_id!("cf.bss.orders.category.v1~cf.bss.orders.change.v1"),
    ] {
        values.push(json!({"id": id}));
    }
    values.extend(PERMISSIONS.iter().map(|p| {
        json!({
            "id": p.id, "resource_type": p.resource, "action": p.action,
            "display_name": format!("Orders {}", p.action),
        })
    }));
    // The event contract (§4.7): topic, abstract family base and eleven final types. The order
    // subject type above (`ORDER`) is registered in the same batch, before them.
    values.extend(crate::infra::events::schema::documents());
    values
}

/// Every entity Orders registers at init, exposed for the registry-commit test.
#[cfg(test)]
pub(crate) fn registration_entities() -> Vec<Value> {
    entities()
}

/// Validate static operation/reason/permission declarations before publishing the SDK provider.
///
/// # Errors
/// Returns an error on duplicate identifiers or an unregistered operation action.
pub fn validate_catalog() -> anyhow::Result<()> {
    let operations: BTreeSet<_> = OPERATIONS.iter().map(|o| (o.method, o.path)).collect();
    anyhow::ensure!(
        operations.len() == OPERATIONS.len(),
        "duplicate Orders operation"
    );
    let reasons: BTreeSet<_> = Reason::ALL.iter().map(|r| r.mapping().code).collect();
    anyhow::ensure!(
        reasons.len() == Reason::ALL.len(),
        "duplicate Orders reason"
    );
    let permissions: BTreeSet<_> = PERMISSIONS
        .iter()
        .map(|p| (p.logical_resource, p.logical_action))
        .collect();
    anyhow::ensure!(
        permissions.len() == PERMISSIONS.len(),
        "duplicate Orders permission"
    );
    for op in OPERATIONS {
        let actions: &[&str] = if op.action == "field_class:write|edit" {
            &["write", "edit"]
        } else {
            &[op.action]
        };
        for action in actions {
            anyhow::ensure!(
                permissions.contains(&(op.resource, *action)),
                "Orders operation has no permission"
            );
        }
    }
    anyhow::ensure!(
        crate::infra::storage::TABLE_PREFIX == "bss_orders__",
        "Orders table namespace mismatch"
    );
    Ok(())
}

pub(crate) async fn register(
    registry: &dyn types_registry_sdk::TypesRegistryClient,
) -> anyhow::Result<()> {
    let entities = entities();
    let expected: BTreeSet<String> = entities
        .iter()
        .filter_map(|v| v.get("$id").or_else(|| v.get("id")).and_then(Value::as_str))
        .map(|s| s.trim_start_matches("gts://").to_owned())
        .collect();
    let results = registry.register(entities).await?;
    let mut actual = BTreeSet::new();
    for result in results {
        match result {
            types_registry_sdk::RegisterResult::Ok { gts_id } => {
                anyhow::ensure!(
                    actual.insert(gts_id),
                    "duplicate Orders schema registration result"
                );
            }
            types_registry_sdk::RegisterResult::Err { .. } => {
                anyhow::bail!("Orders schema registration refused")
            }
        }
    }
    anyhow::ensure!(
        actual == expected,
        "incomplete Orders schema registration response"
    );
    Ok(())
}

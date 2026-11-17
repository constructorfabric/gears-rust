//! GTS publication of the gear (DESIGN §4 Authorization): the two abstract
//! PDP resource types and the eleven permission instances. Everything here
//! is submitted to the `toolkit-gts` inventory and registered in Types
//! Registry at server start.

use toolkit_gts::{AuthzPermissionV1, gts_instance, gts_type_schema};

use crate::domain::access_guard::actions;

/// PDP resource type of providers. Abstract: it names the properties the
/// PDP may constrain and has no instances.
#[derive(Debug)]
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.genai.model_registry.provider.v1~"),
    description = "Model Registry provider resource type for authorization: owner_tenant_id, id, gts_type",
    properties = "id,owner_tenant_id,gts_type",
    gts_abstract = true,
    base = true
)]
pub struct ProviderResourceV1 {
    pub id: gts::GtsInstanceId,
    pub owner_tenant_id: uuid::Uuid,
    pub gts_type: String,
}

/// PDP resource type of models. Abstract.
#[derive(Debug)]
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.genai.model_registry.model.v1~"),
    description = "Model Registry model resource type for authorization: owner_tenant_id, id, provider_id, canonical_id, gts_type",
    properties = "id,owner_tenant_id,provider_id,canonical_id,gts_type",
    gts_abstract = true,
    base = true
)]
pub struct ModelResourceV1 {
    pub id: gts::GtsInstanceId,
    pub owner_tenant_id: uuid::Uuid,
    pub provider_id: uuid::Uuid,
    pub canonical_id: String,
    pub gts_type: String,
}

const PROVIDER: &str = model_registry_sdk::gts::PROVIDER_RESOURCE_TYPE;
const MODEL: &str = model_registry_sdk::gts::MODEL_RESOURCE_TYPE;

macro_rules! permission {
    ($id:literal, $resource:expr, $action:expr, $name:literal) => {
        gts_instance! {
            AuthzPermissionV1 {
                id: gts_id!($id),
                resource_type: $resource.to_owned(),
                action: $action.to_owned(),
                display_name: $name.to_owned(),
            }
        }
    };
}

permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.provider_get.v1",
    PROVIDER,
    actions::GET,
    "Get provider"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.provider_list.v1",
    PROVIDER,
    actions::LIST,
    "List providers"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.provider_create.v1",
    PROVIDER,
    actions::CREATE,
    "Create provider"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.provider_update.v1",
    PROVIDER,
    actions::UPDATE,
    "Update provider"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.provider_delete.v1",
    PROVIDER,
    actions::DELETE,
    "Delete provider"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_get.v1",
    MODEL,
    actions::GET,
    "Get model"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_list.v1",
    MODEL,
    actions::LIST,
    "List models"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_create.v1",
    MODEL,
    actions::CREATE,
    "Create model"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_update.v1",
    MODEL,
    actions::UPDATE,
    "Update model"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_delete.v1",
    MODEL,
    actions::DELETE,
    "Delete model"
);
permission!(
    "cf.toolkit.authz.permission.v1~cf.model_registry._.model_evaluate.v1",
    MODEL,
    actions::EVALUATE,
    "Evaluate model"
);

#[cfg(test)]
mod tests {
    use super::*;
    use gts::GtsSchema;
    use toolkit_gts::{InventoryInstance, InventoryTypeSchema};

    #[test]
    fn resource_type_ids_equal_the_sdk_constants() {
        assert_eq!(ProviderResourceV1::TYPE_ID, PROVIDER);
        assert_eq!(ModelResourceV1::TYPE_ID, MODEL);
        for id in [PROVIDER, MODEL] {
            let entry = toolkit_gts::inventory::iter::<InventoryTypeSchema>
                .into_iter()
                .find(|e| e.type_id == id)
                .unwrap_or_else(|| panic!("{id} is in the inventory"));
            let schema: serde_json::Value = serde_json::from_str(&(entry.schema_fn)()).unwrap();
            assert_eq!(schema["x-gts-abstract"], true, "{schema}");
        }
    }

    #[test]
    fn eleven_permissions_cover_the_design_table() {
        let mut found: Vec<(String, String)> = toolkit_gts::inventory::iter::<InventoryInstance>
            .into_iter()
            .filter(|e| e.instance_id.contains("cf.model_registry._."))
            .map(|e| {
                let payload = (e.payload_fn)();
                (
                    payload["resource_type"].as_str().unwrap().to_owned(),
                    payload["action"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        found.sort();
        let mut expected: Vec<(String, String)> = [
            (PROVIDER, "get"),
            (PROVIDER, "list"),
            (PROVIDER, "create"),
            (PROVIDER, "update"),
            (PROVIDER, "delete"),
            (MODEL, "get"),
            (MODEL, "list"),
            (MODEL, "create"),
            (MODEL, "update"),
            (MODEL, "delete"),
            (MODEL, "evaluate"),
        ]
        .into_iter()
        .map(|(r, a)| (r.to_owned(), a.to_owned()))
        .collect();
        expected.sort();
        assert_eq!(found, expected);
    }
}

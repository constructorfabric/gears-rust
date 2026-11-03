//! LLM Gateway authorization permission catalog: one instance per
//! `(resource_type, action)` the PEP gate enforces.

use toolkit_gts::{AuthzPermissionV1, gts_instance};

use crate::domain::authz::{LLM_GATEWAY_RESOURCE_TYPE, actions};

gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.llmgw.gateway.create_response.v1"),
        resource_type: LLM_GATEWAY_RESOURCE_TYPE.to_owned(),
        action: actions::CREATE_RESPONSE.to_owned(),
        display_name: "Create response".to_owned(),
    }
}
gts_instance! {
    AuthzPermissionV1 {
        id: gts_id!("cf.toolkit.authz.permission.v1~cf.llmgw.gateway.create_embedding.v1"),
        resource_type: LLM_GATEWAY_RESOURCE_TYPE.to_owned(),
        action: actions::CREATE_EMBEDDING.to_owned(),
        display_name: "Create embedding".to_owned(),
    }
}

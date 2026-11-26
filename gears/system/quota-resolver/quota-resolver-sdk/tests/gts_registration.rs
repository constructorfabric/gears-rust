#![allow(clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use quota_resolver_sdk::gts::{
    QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID,
};
use serde_json::Value;
use toolkit_gts::all_inventory_type_schemas;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::service::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::RegisterResult;

fn service() -> Arc<TypesRegistryService> {
    let repo = Arc::new(InMemoryGtsRepository::new(
        TypesRegistryConfig::default().to_gts_config(),
    ));
    Arc::new(TypesRegistryService::new(
        repo,
        TypesRegistryConfig::default(),
    ))
}

fn example(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/schemas/examples")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read example"))
        .expect("example is JSON")
}

#[test]
fn the_bases_and_the_documented_examples_survive_the_startup_commit() {
    let service = service();
    let mut entities = all_inventory_type_schemas().expect("inventory schemas");
    for name in [
        "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~.schema.json",
        "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~.schema.json",
        "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~.schema.json",
    ] {
        entities.push(example(name));
    }

    RegisterResult::ensure_all_ok(&service.register(entities)).expect("every entity registers");
    service
        .switch_to_ready()
        .expect("the ready commit must succeed, or the server cannot boot");

    for type_id in [
        QUOTA_SUBJECT_TYPE_ID,
        QUOTA_TENANT_SUBJECT_TYPE_ID,
        QUOTA_METRIC_TYPE_ID,
        "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~",
        "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~",
        "gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~",
    ] {
        assert!(service.get(type_id).is_ok(), "{type_id} is registered");
    }
}

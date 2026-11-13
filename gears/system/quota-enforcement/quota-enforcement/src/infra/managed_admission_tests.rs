#![allow(clippy::expect_used)]
//! The QE publications as managed registry entities (T38b QE slice): the
//! manifest the runtime publishes for quota-enforcement and for its storage
//! plugin admit through the real local client and database/outbox admission,
//! and owner contracts derived from the QE bases admit on top of them.

use serde_json::Value;
use toolkit::config::ConfigProvider;
use toolkit::gts_publication::GtsPublicationCapability;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsIdPattern, GtsTypeId, LifecycleStatus, ListEntitiesRequest,
    PlatformTypesRegistryApiExt, Projection, ReadOptions,
};

use crate::QuotaEnforcementGear;
use crate::test_support::{
    LLM_USER_PROJECTION, ManagedRegistry, llm_gateway_documents, metric_base_documents,
};
use quota_enforcement_storage_plugin::StoragePluginGear;

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

fn config() -> Config {
    Config(serde_json::json!({
        "quota-enforcement-storage-plugin": { "config": { "vendor": "acme" } }
    }))
}

fn ids(manifest: &toolkit::gts_publication::GtsManifest) -> Vec<String> {
    manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_ref().to_owned())
        .collect()
}

/// The QE manifest, the storage plugin instance and the `llm_gateway`
/// contracts over a stand-in metric base all admit; the registry then serves
/// a concrete subject projection with its materialized scope trait.
#[tokio::test]
async fn the_qe_publications_and_owner_contracts_admit() {
    let h = ManagedRegistry::empty().await;
    let qe = QuotaEnforcementGear::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("QE manifest");
    let plugin = StoragePluginGear::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("plugin manifest");

    // As the runtime publishes them: keyed as the manifest keys them.
    h.admit_keyed("quota-enforcement", qe.documents()).await;
    h.admit_keyed("quota-enforcement-storage-plugin", plugin.documents())
        .await;
    h.admit("metric-owner", metric_base_documents()).await;
    h.admit("llm-gateway", llm_gateway_documents()).await;

    let api = h.platform();
    let ctx = PlatformSecurityContext::outbound_marker();
    let mut active: Vec<String> = api
        .list_type_schemas(
            &ctx,
            ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.*").expect("pattern")),
        )
        .await
        .expect("types")
        .into_iter()
        .filter(|t| t.lifecycle_status == LifecycleStatus::Active)
        .map(|t| t.type_id.as_ref().to_owned())
        .chain(
            api.list_instances(
                &ctx,
                ListEntitiesRequest::matching(GtsIdPattern::try_new("gts.*").expect("pattern")),
            )
            .await
            .expect("instances")
            .into_iter()
            .filter(|i| i.lifecycle_status == LifecycleStatus::Active)
            .map(|i| i.id.as_ref().to_owned()),
        )
        .collect();
    active.sort();
    for id in ids(&qe).iter().chain(ids(&plugin).iter()) {
        assert!(active.contains(id), "{id} is admitted and active");
    }

    let user = api
        .get_type_schema(
            &ctx,
            &GtsTypeId::new(LLM_USER_PROJECTION),
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::EffectiveTraits,
            ]))),
        )
        .await
        .expect("the user projection");
    assert_eq!(
        user.effective_traits.expect("traits")["scope"],
        quota_enforcement_sdk::SCOPE_USER
    );
}

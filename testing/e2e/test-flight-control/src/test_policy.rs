use std::sync::{Arc, LazyLock};

use anyhow::{Result, ensure};
use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverError, AuthZResolverPluginClient, AuthZResolverPluginSpecV1, Constraint,
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InPredicate, Predicate,
};
use gts::{GtsId, GtsIdPattern, GtsTypeId};
use serde::{Deserialize, Deserializer};
use toolkit::Gear;
use toolkit::bootstrap::AppConfig;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use uuid::Uuid;

const INSTANCE_SEGMENT: &str = "cf.e2e.eb_test_policy.plugin.v1";
const SHIPPED_POLICY: &str = toolkit_gts::gts_id!(
    "cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.builtin.tr_authz_resolver.plugin.v1"
);
const VENDOR: &str = "e2e";
const PRIORITY: i16 = 100;
const CONFIG_ERROR: &str = "invalid eb-test-policy configuration";
const MAX_GRANTS: usize = 256;
const MAX_TYPES_PER_GRANT: usize = 256;
const OWNER_TENANT_ID: &str = "owner_tenant_id";
const EVENT_TYPE: &str = "event_type";
const EVENT_SUBJECT_TYPE: &str = "event_subject_type";
static TOPIC_FAMILY: LazyLock<Option<GtsIdPattern>> =
    LazyLock::new(|| GtsIdPattern::try_new(toolkit_gts::gts_id!("cf.core.events.topic.v1~")).ok());

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TestPolicyConfig {
    grants: Vec<TestGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestGrant {
    subject_id: Uuid,
    action: String,
    #[serde(deserialize_with = "deserialize_resource")]
    resource: GtsIdPattern,
    event_types: Option<Vec<GtsTypeId>>,
    subject_types: Option<Vec<GtsTypeId>>,
}

fn deserialize_resource<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<GtsIdPattern, D::Error> {
    let resource = String::deserialize(deserializer)?;
    GtsIdPattern::try_new(&resource).map_err(|_| serde::de::Error::custom(CONFIG_ERROR))
}

impl TestPolicyConfig {
    fn validate(&self) -> Result<()> {
        ensure!(self.grants.len() <= MAX_GRANTS, CONFIG_ERROR);
        for grant in &self.grants {
            let event_types = grant.event_types.as_ref().map_or(0, Vec::len);
            let subject_types = grant.subject_types.as_ref().map_or(0, Vec::len);
            ensure!(
                event_types.saturating_add(subject_types) <= MAX_TYPES_PER_GRANT,
                CONFIG_ERROR
            );
        }
        Ok(())
    }
}

/// Loads the test policy's grant configuration and checks its bounds.
///
/// # Errors
///
/// Returns an error if the configuration cannot be decoded, if it contains more than 256 grants,
/// or if any grant lists more than 256 event and subject types combined.
pub fn validate_config(config: &AppConfig) -> Result<()> {
    let policy: TestPolicyConfig = toolkit::gear_config_or_default(config, TestPolicy::MODULE_NAME)
        .map_err(|_| anyhow::anyhow!(CONFIG_ERROR))?;
    policy.validate()
}

#[derive(Default)]
#[toolkit::gear(
    name = "eb-test-policy",
    deps = [tr_authz_plugin],
    gts_publication(entities = Self::plugin_instance),
)]
struct TestPolicy;

impl TestPolicy {
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> Result<Vec<(types_registry_sdk::GtsId, serde_json::Value)>> {
        let cfg: TestPolicyConfig = inputs
            .config_or_default()
            .map_err(|_| anyhow::anyhow!(CONFIG_ERROR))?;
        cfg.validate()?;
        let (id, document) = PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            VENDOR,
            PRIORITY,
        )?;
        Ok(vec![(
            types_registry_sdk::GtsId::try_new(id.as_ref())?,
            document,
        )])
    }
}

#[async_trait]
impl Gear for TestPolicy {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        let config: TestPolicyConfig = ctx
            .config_or_default()
            .map_err(|_| anyhow::anyhow!(CONFIG_ERROR))?;
        config.validate()?;
        let hub = ctx.client_hub();
        let hierarchy = hub
            .try_get_scoped::<dyn AuthZResolverPluginClient>(&ClientScope::gts_id(SHIPPED_POLICY))
            .ok_or_else(|| anyhow::anyhow!("eb-test-policy requires the bound tr-authz plugin"))?;
        let api: Arc<dyn AuthZResolverPluginClient> =
            Arc::new(NarrowingPolicy { hierarchy, config });
        bind_plugin_instances::<AuthZResolverPluginSpecV1, _>(
            &hub,
            ctx.gts_publication_manifest()?.documents(),
            &api,
        )
    }
}

struct NarrowingPolicy {
    hierarchy: Arc<dyn AuthZResolverPluginClient>,
    config: TestPolicyConfig,
}

#[async_trait]
impl AuthZResolverPluginClient for NarrowingPolicy {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let hierarchy = self.hierarchy.evaluate(request.clone()).await?;
        Ok(intersect(&request, hierarchy, &self.config))
    }
}

fn intersect(
    request: &EvaluationRequest,
    mut hierarchy: EvaluationResponse,
    config: &TestPolicyConfig,
) -> EvaluationResponse {
    if !hierarchy.decision {
        return hierarchy;
    }
    let Ok(resource) = GtsId::try_new(&request.resource.resource_type) else {
        return deny();
    };
    let topic_consume = request.action.name == "consume"
        && TOPIC_FAMILY
            .as_ref()
            .is_some_and(|topic| resource.matches_pattern(topic));
    let matching = config.grants.iter().filter(|grant| {
        grant.subject_id == request.subject.id
            && grant.action == request.action.name
            && resource.matches_pattern(&grant.resource)
    });
    let alternatives: Vec<_> = matching
        .filter_map(|grant| grant.predicates(request, topic_consume))
        .collect();
    if alternatives.is_empty() {
        return deny();
    }
    if hierarchy.context.constraints.is_empty() && decision_only_create(request) {
        return hierarchy;
    }
    if hierarchy.context.constraints.is_empty()
        || !hierarchy.context.constraints.iter().all(has_tenant_fence)
    {
        return deny();
    }

    // Each alternative must retain its tenant fence and its own grant's type pair.
    hierarchy.context.constraints = hierarchy
        .context
        .constraints
        .into_iter()
        .flat_map(|branch| {
            alternatives.iter().map(move |requested| {
                let mut predicates = branch.predicates.clone();
                predicates.extend_from_slice(requested);
                Constraint { predicates }
            })
        })
        .collect();
    hierarchy
}

impl TestGrant {
    fn predicates(
        &self,
        request: &EvaluationRequest,
        topic_consume: bool,
    ) -> Option<Vec<Predicate>> {
        let mut predicates = Vec::new();
        for (property, types) in [
            (EVENT_TYPE, self.event_types.as_deref()),
            (EVENT_SUBJECT_TYPE, self.subject_types.as_deref()),
        ] {
            let Some(types) = types else {
                continue;
            };
            if types.is_empty()
                || !topic_consume
                || !request
                    .context
                    .supported_properties
                    .iter()
                    .any(|supported| supported == property)
            {
                return None;
            }
            predicates.push(Predicate::In(InPredicate::new(
                property,
                types.iter().map(AsRef::<str>::as_ref),
            )));
        }
        Some(predicates)
    }
}

fn decision_only_create(request: &EvaluationRequest) -> bool {
    request.action.name == "create"
        && request.resource.id.is_none()
        && !request.context.require_constraints
        && request.context.supported_properties.is_empty()
        && !request.resource.properties.contains_key("group_ids")
        && !request
            .resource
            .properties
            .contains_key("ancestor_group_ids")
}

fn has_tenant_fence(branch: &Constraint) -> bool {
    let is_tenant = |value: &serde_json::Value| {
        value
            .as_str()
            .is_some_and(|value| Uuid::parse_str(value).is_ok())
    };
    branch.predicates.iter().any(|predicate| match predicate {
        Predicate::Eq(eq) => eq.property == OWNER_TENANT_ID && is_tenant(&eq.value),
        Predicate::In(set) => {
            set.property == OWNER_TENANT_ID
                && !set.values.is_empty()
                && set.values.iter().all(is_tenant)
        }
        _ => false,
    })
}

fn deny() -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext::default(),
    }
}

#[cfg(test)]
#[path = "test_policy_tests.rs"]
mod tests;

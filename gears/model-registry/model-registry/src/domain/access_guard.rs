//! PDP decisions (DESIGN §4 Authorization).
//!
//! Every call asks the PDP with `owner_tenant_id = T0`, `require_constraints
//! = false`. Tenant constraints are a gate on T0; the remaining filters on
//! `id`, `provider_id`, `canonical_id`, and `gts_type` are row constraints
//! that lists apply in SQL and point operations check in memory. An
//! `InTenantSubtree` tenant constraint and constraint kinds that cannot be
//! evaluated deny (ADR-0001).

use authz_resolver_sdk::{AccessRequest, PolicyEnforcer, ResourceType};
use model_registry_sdk::gts::{MODEL_RESOURCE_TYPE, PROVIDER_RESOURCE_TYPE};
use model_registry_sdk::{ModelV1, ProviderV1};
use toolkit_macros::domain_model;
use toolkit_security::pep_properties::{OWNER_TENANT_ID, RESOURCE_ID};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, SecurityContext};
use uuid::Uuid;

use super::error::DomainError;

pub const PROP_PROVIDER_ID: &str = "provider_id";
pub const PROP_CANONICAL_ID: &str = "canonical_id";
pub const PROP_GTS_TYPE: &str = "gts_type";

pub static PROVIDER_RESOURCE: ResourceType = ResourceType::from_static(
    PROVIDER_RESOURCE_TYPE,
    &[OWNER_TENANT_ID, RESOURCE_ID, PROP_GTS_TYPE],
);

pub static MODEL_RESOURCE: ResourceType = ResourceType::from_static(
    MODEL_RESOURCE_TYPE,
    &[
        OWNER_TENANT_ID,
        RESOURCE_ID,
        PROP_PROVIDER_ID,
        PROP_CANONICAL_ID,
        PROP_GTS_TYPE,
    ],
);

pub mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
    pub const EVALUATE: &str = "evaluate";
}

const ROW_PROPERTIES: [&str; 4] = [
    RESOURCE_ID,
    PROP_PROVIDER_ID,
    PROP_CANONICAL_ID,
    PROP_GTS_TYPE,
];

/// A row the PDP constraints are checked against in memory.
pub trait RowProperties {
    fn property(&self, name: &str) -> Option<String>;
}

impl RowProperties for ModelV1 {
    fn property(&self, name: &str) -> Option<String> {
        match name {
            OWNER_TENANT_ID => Some(self.tenant_id.to_string()),
            RESOURCE_ID => Some(self.id.to_string()),
            PROP_PROVIDER_ID => Some(self.provider_id.to_string()),
            PROP_CANONICAL_ID => Some(self.canonical_id.clone()),
            PROP_GTS_TYPE => Some(self.info.gts_type.to_string()),
            _ => None,
        }
    }
}

impl RowProperties for ProviderV1 {
    fn property(&self, name: &str) -> Option<String> {
        match name {
            OWNER_TENANT_ID => Some(self.tenant_id.to_string()),
            RESOURCE_ID => Some(self.id.to_string()),
            PROP_GTS_TYPE => Some(self.gts_type.to_string()),
            _ => None,
        }
    }
}

/// One row filter kept from a PDP constraint (`property IN values`).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RowFilter {
    property: String,
    values: Vec<ScopeValue>,
}

/// The row constraints of a permitted decision: an OR of AND-groups, or
/// unconstrained.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    groups: Option<Vec<Vec<RowFilter>>>,
}

impl Decision {
    #[must_use]
    pub fn unconstrained() -> Self {
        Self { groups: None }
    }

    #[must_use]
    pub fn is_unconstrained(&self) -> bool {
        self.groups.is_none()
    }

    /// Whether the row satisfies the constraints.
    #[must_use]
    pub fn matches(&self, row: &dyn RowProperties) -> bool {
        let Some(groups) = &self.groups else {
            return true;
        };
        groups.iter().any(|group| {
            group.iter().all(|f| {
                row.property(&f.property)
                    .is_some_and(|value| f.values.iter().any(|v| v.to_string() == value))
            })
        })
    }

    /// The SQL scope of a list: the chain scope `AND`ed with each kept group.
    #[must_use]
    pub fn scope(&self, chain: &[Uuid]) -> AccessScope {
        let tenant_filter = || ScopeFilter::in_uuids(OWNER_TENANT_ID, chain.to_vec());
        match &self.groups {
            None => AccessScope::for_tenants(chain.to_vec()),
            Some(groups) => AccessScope::from_constraints(
                groups
                    .iter()
                    .map(|group| {
                        let mut filters = vec![tenant_filter()];
                        filters.extend(
                            group
                                .iter()
                                .map(|f| ScopeFilter::r#in(f.property.clone(), f.values.clone())),
                        );
                        ScopeConstraint::new(filters)
                    })
                    .collect(),
            ),
        }
    }
}

#[domain_model]
pub struct AccessGuard {
    enforcer: PolicyEnforcer,
    max_values: usize,
}

impl AccessGuard {
    #[must_use]
    pub fn new(enforcer: PolicyEnforcer, max_values: usize) -> Self {
        Self {
            enforcer,
            max_values,
        }
    }

    /// Takes one PDP decision. `props` are the resource properties beyond
    /// `owner_tenant_id`.
    pub async fn decide(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &str,
        resource_id: Option<Uuid>,
        props: &[(&str, serde_json::Value)],
        operation: &'static str,
    ) -> Result<Decision, DomainError> {
        let t0 = ctx.subject_tenant_id();
        let mut request = AccessRequest::new()
            .resource_property(OWNER_TENANT_ID, t0)
            .require_constraints(false);
        for (name, value) in props {
            request = request.resource_property(*name, value.clone());
        }
        let scope = self
            .enforcer
            .access_scope_with(ctx, resource, action, resource_id, &request)
            .await
            .map_err(|e| DomainError::from_enforcer(e, operation))?;
        self.reduce(&scope, t0, operation)
    }

    /// The decision rules 1 to 4: tenant gate, row constraints, deny on
    /// anything else.
    fn reduce(
        &self,
        scope: &AccessScope,
        t0: Uuid,
        operation: &'static str,
    ) -> Result<Decision, DomainError> {
        if scope.is_unconstrained() {
            return Ok(Decision::unconstrained());
        }
        let mut groups: Vec<Vec<RowFilter>> = Vec::new();
        for constraint in scope.constraints() {
            let mut keep = true;
            let mut group = Vec::new();
            for filter in constraint.filters() {
                let property = filter.property();
                if property == OWNER_TENANT_ID {
                    // `InTenantSubtree` and unknown kinds admit nothing.
                    let admits = match filter {
                        ScopeFilter::Eq(_) | ScopeFilter::In(_) => {
                            filter.values().iter().any(|v| v.as_uuid() == Some(t0))
                        }
                        _ => false,
                    };
                    if !admits {
                        keep = false;
                        break;
                    }
                } else if ROW_PROPERTIES.contains(&property) {
                    let values: Vec<ScopeValue> = match filter {
                        ScopeFilter::Eq(_) | ScopeFilter::In(_) => {
                            filter.values().iter().cloned().collect()
                        }
                        _ => {
                            keep = false;
                            break;
                        }
                    };
                    if values.is_empty() || values.len() > self.max_values {
                        keep = false;
                        break;
                    }
                    group.push(RowFilter {
                        property: property.to_owned(),
                        values,
                    });
                } else {
                    keep = false;
                    break;
                }
            }
            if keep {
                if group.is_empty() {
                    return Ok(Decision::unconstrained());
                }
                groups.push(group);
            }
        }
        if groups.is_empty() {
            tracing::debug!(
                operation,
                "no pdp constraint alternative admits the acting tenant"
            );
            return Err(DomainError::not_permitted(operation));
        }
        Ok(Decision {
            groups: Some(groups),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz_resolver_sdk::constraints::{
        Constraint, EqPredicate, InPredicate, InTenantSubtreePredicate, Predicate,
    };
    use authz_resolver_sdk::models::Capability;
    use std::sync::Arc;
    use toolkit_gts::gts_id;

    use crate::testing::{StaticAuthz, permit_with, security_context};

    /// Advertises the hierarchy capability, which the gear does not, so a
    /// subtree predicate reaches the gate under test.
    fn guard(authz: StaticAuthz, max_values: usize) -> AccessGuard {
        AccessGuard::new(
            PolicyEnforcer::new(Arc::new(authz))
                .with_capabilities(vec![Capability::TenantHierarchy]),
            max_values,
        )
    }

    async fn decide_with(authz: StaticAuthz, chain: &[Uuid]) -> Result<Decision, DomainError> {
        let ctx = security_context(chain[0]);
        guard(authz, 3)
            .decide(&ctx, &MODEL_RESOURCE, actions::GET, None, &[], "op")
            .await
    }

    fn constraint(predicates: Vec<Predicate>) -> Constraint {
        Constraint { predicates }
    }

    #[tokio::test]
    async fn own_tenant_eq_without_row_filters_is_unconstrained() {
        let t0 = Uuid::new_v4();
        let d = decide_with(StaticAuthz::permit_own_tenant(), &[t0])
            .await
            .unwrap();
        assert!(d.is_unconstrained());
        assert_eq!(d.scope(&[t0]), AccessScope::for_tenants(vec![t0]));
    }

    #[tokio::test]
    async fn no_constraints_is_unconstrained_and_denial_is_not_permitted() {
        let t0 = Uuid::new_v4();
        assert!(
            decide_with(StaticAuthz::permit_unconstrained(), &[t0])
                .await
                .unwrap()
                .is_unconstrained()
        );
        let err = decide_with(StaticAuthz::deny_all(), &[t0])
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotPermitted { .. }));
        let err = decide_with(StaticAuthz::failing(), &[t0])
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::DependencyUnavailable { .. }));
    }

    #[tokio::test]
    async fn tenant_gate_rejects_other_tenants() {
        let t0 = Uuid::new_v4();
        let other = Uuid::new_v4();
        let authz = StaticAuthz::with(move |_| {
            Ok(permit_with(vec![constraint(vec![Predicate::In(
                InPredicate::new(OWNER_TENANT_ID, [other]),
            )])]))
        });
        let err = decide_with(authz, &[t0]).await.unwrap_err();
        assert!(matches!(err, DomainError::NotPermitted { .. }));
    }

    #[tokio::test]
    async fn tenant_subtree_constraint_fails_closed() {
        let t0 = Uuid::new_v4();
        let parent = Uuid::new_v4();
        // Rooted at an ancestor, at T0 itself, or elsewhere: none admits.
        for root in [parent, t0, Uuid::new_v4()] {
            let authz = StaticAuthz::with(move |_| {
                Ok(permit_with(vec![constraint(vec![
                    Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        OWNER_TENANT_ID,
                        root,
                    )),
                ])]))
            });
            let err = decide_with(authz, &[t0, parent]).await.unwrap_err();
            assert!(matches!(err, DomainError::NotPermitted { .. }), "{root}");
        }
        // Another alternative of the same decision still admits T0.
        let authz = StaticAuthz::with(move |req| {
            Ok(permit_with(vec![
                constraint(vec![Predicate::InTenantSubtree(
                    InTenantSubtreePredicate::new(OWNER_TENANT_ID, parent),
                )]),
                constraint(vec![Predicate::Eq(EqPredicate::new(
                    OWNER_TENANT_ID,
                    crate::testing::subject_tenant(req),
                ))]),
            ]))
        });
        assert!(
            decide_with(authz, &[t0, parent])
                .await
                .unwrap()
                .is_unconstrained()
        );
    }

    #[tokio::test]
    async fn row_constraints_are_kept_and_checked() {
        struct Row(Uuid, &'static str);
        impl RowProperties for Row {
            fn property(&self, name: &str) -> Option<String> {
                match name {
                    PROP_PROVIDER_ID => Some(self.0.to_string()),
                    PROP_GTS_TYPE => Some(self.1.to_owned()),
                    _ => None,
                }
            }
        }
        let t0 = Uuid::new_v4();
        let allowed = Uuid::new_v4();
        let authz = StaticAuthz::with(move |req| {
            Ok(permit_with(vec![
                constraint(vec![
                    Predicate::Eq(EqPredicate::new(
                        OWNER_TENANT_ID,
                        crate::testing::subject_tenant(req),
                    )),
                    Predicate::In(InPredicate::new(PROP_PROVIDER_ID, [allowed])),
                    Predicate::Eq(EqPredicate::new(
                        PROP_GTS_TYPE,
                        gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~"),
                    )),
                ]),
                constraint(vec![
                    Predicate::Eq(EqPredicate::new(OWNER_TENANT_ID, Uuid::new_v4())),
                    Predicate::Eq(EqPredicate::new(PROP_CANONICAL_ID, "x::y")),
                ]),
            ]))
        });
        let d = decide_with(authz, &[t0]).await.unwrap();
        assert!(!d.is_unconstrained());
        let scope = d.scope(&[t0]);
        assert_eq!(
            scope.constraints().len(),
            1,
            "the foreign-tenant alternative is dropped"
        );
        assert_eq!(scope.constraints()[0].filters().len(), 3);
        assert_eq!(
            scope.constraints()[0].filters()[0].property(),
            OWNER_TENANT_ID
        );

        assert!(d.matches(&Row(
            allowed,
            gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")
        )));
        assert!(!d.matches(&Row(
            Uuid::new_v4(),
            gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")
        )));
        assert!(!d.matches(&Row(
            allowed,
            gts_id!("cf.genai.model.provider.v1~cf.genai._.azure.v1~")
        )));
    }

    #[tokio::test]
    async fn unsupported_kinds_properties_and_long_lists_deny() {
        let t0 = Uuid::new_v4();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let authz = StaticAuthz::with(move |req| {
            Ok(permit_with(vec![constraint(vec![
                Predicate::Eq(EqPredicate::new(
                    OWNER_TENANT_ID,
                    crate::testing::subject_tenant(req),
                )),
                Predicate::In(InPredicate::new(RESOURCE_ID, ids.clone())),
            ])]))
        });
        let err = decide_with(authz, &[t0]).await.unwrap_err();
        assert!(
            matches!(err, DomainError::NotPermitted { .. }),
            "value list over the cap"
        );

        let authz = StaticAuthz::with(move |req| {
            Ok(permit_with(vec![constraint(vec![
                Predicate::Eq(EqPredicate::new(
                    OWNER_TENANT_ID,
                    crate::testing::subject_tenant(req),
                )),
                Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                    PROP_PROVIDER_ID,
                    Uuid::new_v4(),
                )),
            ])]))
        });
        let err = decide_with(authz, &[t0]).await.unwrap_err();
        assert!(
            matches!(err, DomainError::NotPermitted { .. }),
            "subtree on a row property"
        );
    }

    #[tokio::test]
    async fn properties_and_resource_id_reach_the_pdp() {
        let t0 = Uuid::new_v4();
        let (authz, handle) = crate::testing::shared(StaticAuthz::permit_unconstrained());
        let guard = AccessGuard::new(PolicyEnforcer::new(authz), 10);
        let id = Uuid::new_v4();
        let ctx = security_context(t0);
        guard
            .decide(
                &ctx,
                &MODEL_RESOURCE,
                actions::EVALUATE,
                Some(id),
                &[(PROP_CANONICAL_ID, serde_json::json!("openai::gpt-4o"))],
                "resolve_model",
            )
            .await
            .unwrap();
        let req = &handle.requests()[0];
        assert_eq!(req.action.name, "evaluate");
        assert_eq!(req.resource.resource_type, MODEL_RESOURCE_TYPE);
        assert_eq!(req.resource.id, Some(id));
        assert_eq!(
            req.resource.properties[OWNER_TENANT_ID],
            serde_json::json!(t0.to_string())
        );
        assert_eq!(
            req.resource.properties[PROP_CANONICAL_ID],
            serde_json::json!("openai::gpt-4o")
        );
        assert!(!req.context.require_constraints);
    }
}

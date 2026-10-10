//! Test support: the real `types-registry` startup commit over Orders' registration, and
//! valid / maximal event instances. Not runtime code.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use bss_orders_lifecycle_sdk::catalog::OrderState;
use tenant_resolver_sdk::{TenantInfo, TenantResolverClient, TenantResolverError, TenantStatus};
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::local_client::TypesRegistryLocalClient;
use types_registry::domain::service::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

use super::OrderSummary;
use super::payload::{
    Assertion, CompensationEvidence, FailureReason, LineRef, LineSubscription, MAX_CATEGORY_CHARS,
    MAX_EXTERNAL_REFERENCE_CHARS, MAX_LINES, MAX_REASON_CHARS, MAX_REFERENCE_CHARS,
    OperatorAttestation, OrderAcceptanceRecorded, OrderAmended, OrderApproved, OrderCancelled,
    OrderCompleted, OrderExpired, OrderFulfillmentFailed, OrderHeld, OrderRejected, OrderResumed,
    OrderSubmitted, RequirementSource, SubmitAcceptance, UnknownToken,
};

/// The registry after the process-wide startup commit: every linked inventory type (including
/// Event Broker's topic/event bases) plus exactly what Orders registers at init.
pub fn committed_registry() -> Arc<TypesRegistryService> {
    let service = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(
            TypesRegistryConfig::default().to_gts_config(),
        )),
        TypesRegistryConfig::default(),
    ));
    let mut entities = toolkit_gts::all_inventory_type_schemas().unwrap();
    entities.extend(toolkit_gts::all_inventory_instances().unwrap());
    entities.extend(crate::gts::registration_entities());
    let results = service.register(entities);
    let refused: Vec<_> = results
        .iter()
        .filter(|r| matches!(r, RegisterResult::Err { .. }))
        .collect();
    assert!(refused.is_empty(), "registry refused: {refused:#?}");
    service
        .switch_to_ready()
        .expect("the Orders contract survives the startup commit");
    service
}

/// The registry as a `TypesRegistryClient` (what Event Broker loads from).
pub fn registry_client() -> Arc<dyn TypesRegistryClient> {
    Arc::new(TypesRegistryLocalClient::new(committed_registry()))
}

pub fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
pub fn at() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap()
}
pub const NEW_SALE: &str = "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1";

pub fn summary(order: Uuid, version: i32, state: OrderState) -> OrderSummary {
    OrderSummary {
        order_id: order,
        order_version: version,
        occurred_at: at(),
        correlation_id: u(77),
        category: NEW_SALE.to_owned(),
        state,
        resource_tenant_id: u(10),
        seller_tenant_id: u(20),
        payer_tenant_id: u(30),
        contract_id: None,
        external_reference: None,
    }
}

/// Worst-case JSON text: 4-byte UTF-8 scalars, or 2-byte escapes where ASCII is required.
fn wide(chars: usize) -> String {
    "\u{1F600}".repeat(chars)
}
fn escaped_ascii(chars: usize) -> String {
    "\"".repeat(chars)
}
fn ids(start: u128, n: usize) -> Vec<Uuid> {
    (0..n as u128).map(|i| u(start + i)).collect()
}

/// A summary with every bounded member at its maximum JSON size.
pub fn max_summary(order: Uuid, version: i32, state: OrderState) -> OrderSummary {
    let mut category = NEW_SALE.to_owned();
    category.push_str(&"~".repeat(MAX_CATEGORY_CHARS - category.len()));
    OrderSummary {
        category,
        contract_id: Some(u(40)),
        external_reference: Some(wide(MAX_EXTERNAL_REFERENCE_CHARS)),
        ..summary(order, version, state)
    }
}

pub fn ordinary_evidence(n: usize) -> CompensationEvidence {
    CompensationEvidence {
        drafts_voided: ids(1_000, n),
        activated_rolled_back: ids(5_000, n),
        activation_dispatched: true,
        at_sale_facts_emitted: Assertion::Known(false),
        no_active_subscription_remains: Assertion::Known(true),
        operator_attestation: None,
    }
}
pub fn forced_evidence(n: usize) -> CompensationEvidence {
    CompensationEvidence {
        at_sale_facts_emitted: Assertion::Unknown(UnknownToken::Unknown),
        no_active_subscription_remains: Assertion::Unknown(UnknownToken::Unknown),
        operator_attestation: Some(OperatorAttestation {
            requested_by: u(91),
            request_audit_id: u(92),
            requested_at: at(),
            approved_by: u(93),
        }),
        ..ordinary_evidence(n)
    }
}

/// One valid instance of each event at its largest admitted size, as `(name, summary, data)`
/// builders for both the unit and the real-producer capacity tests.
pub struct MaxEvents {
    pub submitted: (OrderSummary, OrderSubmitted),
    pub approved: (OrderSummary, OrderApproved),
    pub rejected: (OrderSummary, OrderRejected),
    pub amended: (OrderSummary, OrderAmended),
    pub held: (OrderSummary, OrderHeld),
    pub resumed: (OrderSummary, OrderResumed),
    pub cancelled: (OrderSummary, OrderCancelled),
    pub expired: (OrderSummary, OrderExpired),
    pub completed: (OrderSummary, OrderCompleted),
    pub failed: (OrderSummary, OrderFulfillmentFailed),
    pub forced: (OrderSummary, OrderFulfillmentFailed),
    pub acceptance: (OrderSummary, OrderAcceptanceRecorded),
}
pub fn max_events(order: Uuid) -> MaxEvents {
    let reason = || wide(MAX_REASON_CHARS);
    let reference = || escaped_ascii(MAX_REFERENCE_CHARS);
    MaxEvents {
        submitted: (
            max_summary(order, i32::MAX, OrderState::Submitted),
            OrderSubmitted {
                lines: ids(10_000, MAX_LINES)
                    .into_iter()
                    .map(|line_id| LineRef { line_id })
                    .collect(),
                acceptance: Some(SubmitAcceptance {
                    accepted_version: i32::MAX,
                    accepted_at: at(),
                }),
            },
        ),
        approved: (
            max_summary(order, i32::MAX, OrderState::Approved),
            OrderApproved {
                deciding_authority: reference(),
                approved_version: i32::MAX,
            },
        ),
        rejected: (
            max_summary(order, i32::MAX, OrderState::Rejected),
            OrderRejected {
                deciding_authority: reference(),
                denial_reason: reason(),
            },
        ),
        amended: (
            max_summary(order, i32::MAX, OrderState::Submitted),
            OrderAmended {
                supersedes_version: i32::MAX - 1,
            },
        ),
        held: (
            max_summary(order, i32::MAX, OrderState::OnHold),
            OrderHeld {
                previous_state: OrderState::PendingApproval,
                hold_reason: Some(reason()),
            },
        ),
        resumed: (
            max_summary(order, i32::MAX, OrderState::PendingApproval),
            OrderResumed {
                restored_state: OrderState::PendingApproval,
            },
        ),
        cancelled: (
            max_summary(order, i32::MAX, OrderState::Cancelled),
            OrderCancelled {
                cancelling_actor: reference(),
                cancel_reason: reason(),
                compensation_evidence: Some(ordinary_evidence(MAX_LINES)),
            },
        ),
        expired: (
            max_summary(order, i32::MAX, OrderState::Expired),
            OrderExpired {
                expired_state: OrderState::PendingApproval,
                ttl: format!("P{}D", "9".repeat(60)),
                ttl_policy_id: u(181),
                ttl_policy_revision: i64::MAX,
                platform_policy_revision: i64::MAX,
            },
        ),
        completed: (
            max_summary(order, i32::MAX, OrderState::Completed),
            OrderCompleted {
                lines: ids(10_000, MAX_LINES)
                    .into_iter()
                    .zip(ids(20_000, MAX_LINES))
                    .map(|(line_id, subscription_id)| LineSubscription {
                        line_id,
                        subscription_id,
                    })
                    .collect(),
            },
        ),
        failed: (
            max_summary(order, i32::MAX, OrderState::FulfillmentFailed),
            OrderFulfillmentFailed {
                failure_reason: FailureReason::OverlapPresenceUnevaluable,
                forced_reason: None,
                compensation_evidence: ordinary_evidence(MAX_LINES),
            },
        ),
        forced: (
            max_summary(order, i32::MAX, OrderState::FulfillmentFailed),
            OrderFulfillmentFailed {
                failure_reason: FailureReason::OperatorForcedUnreconciled,
                forced_reason: Some(reason()),
                compensation_evidence: forced_evidence(MAX_LINES),
            },
        ),
        acceptance: (
            max_summary(order, i32::MAX, OrderState::InFulfillment),
            OrderAcceptanceRecorded {
                accepted_version: i32::MAX,
                accepted_at: at(),
                recording_actor: reference(),
                requirement_source: RequirementSource::PlatformDefault,
            },
        ),
    }
}

/// Apply `f` to every maximal event as `(name, summary, encoded data)`.
pub fn encoded_max_events(order: Uuid) -> Vec<(&'static str, Vec<u8>)> {
    fn encode<K: super::EventKind>(summary: OrderSummary, detail: K) -> (&'static str, Vec<u8>) {
        let event = super::OrderEvent::rooted(super::RootTenant(u(1)), summary, detail);
        event
            .validate()
            .unwrap_or_else(|e| panic!("{}: {e}", K::NAME));
        (K::NAME, serde_json::to_vec(&event).unwrap())
    }
    let m = max_events(order);
    vec![
        encode(m.submitted.0, m.submitted.1),
        encode(m.approved.0, m.approved.1),
        encode(m.rejected.0, m.rejected.1),
        encode(m.amended.0, m.amended.1),
        encode(m.held.0, m.held.1),
        encode(m.resumed.0, m.resumed.1),
        encode(m.cancelled.0, m.cancelled.1),
        encode(m.expired.0, m.expired.1),
        encode(m.completed.0, m.completed.1),
        encode(m.failed.0, m.failed.1),
        encode(m.forced.0, m.forced.1),
        encode(m.acceptance.0, m.acceptance.1),
    ]
}

/// The platform tenant resolver's root answer (input double; `None` is an outage).
pub struct Root(pub Option<TenantInfo>);
pub fn root(id: u128, parent: Option<u128>) -> Arc<dyn TenantResolverClient> {
    Arc::new(Root(Some(TenantInfo {
        id: tenant_resolver_sdk::TenantId(u(id)),
        name: "root".into(),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: parent.map(|p| tenant_resolver_sdk::TenantId(u(p))),
        self_managed: false,
    })))
}
#[async_trait::async_trait]
impl TenantResolverClient for Root {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Internal("unused".into()))
    }
    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.0
            .clone()
            .ok_or_else(|| TenantResolverError::ServiceUnavailable("down".into()))
    }
    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[tenant_resolver_sdk::TenantId],
        _: &tenant_resolver_sdk::GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Err(TenantResolverError::Internal("unused".into()))
    }
    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetAncestorsOptions,
    ) -> Result<tenant_resolver_sdk::GetAncestorsResponse, TenantResolverError> {
        Err(TenantResolverError::Internal("unused".into()))
    }
    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::GetDescendantsOptions,
    ) -> Result<tenant_resolver_sdk::GetDescendantsResponse, TenantResolverError> {
        Err(TenantResolverError::Internal("unused".into()))
    }
    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: tenant_resolver_sdk::TenantId,
        _: tenant_resolver_sdk::TenantId,
        _: &tenant_resolver_sdk::IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Err(TenantResolverError::Internal("unused".into()))
    }
}

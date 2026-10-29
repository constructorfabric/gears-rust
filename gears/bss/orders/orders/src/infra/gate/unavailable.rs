//! The fail-closed production adapters of the nine undelivered ports: each answers
//! `PortAnswer::Unavailable(Unavailable::Missing)` and calls nothing. They log nothing per call;
//! readiness reports the missing providers (S3-14).
use super::budget::RunBudget;
use super::ports::{
    AcceptancePolicyReads, AssessmentReads, BillingTermsReads, ContractReads, EvaluationReads,
    IdentityReads, IndicativeTaxReads, OccupancyReads, OverlapKeyReads,
};
use crate::domain::gate::answers::{
    AcceptancePolicy, AcceptancePolicyQuery, AssessmentAnswer, AssessmentQuery, BillingTermsQuery,
    ContractFacts, ContractQuery, IndicativeTax, OccupancyObservation, OccupancyQuery,
    OrderEvaluation, OrderEvaluationRequest, OverlapKey, OverlapKeyLine, PayerProfile,
    PayerProfileQuery, ResolvedTerms, TaxQuery,
};
use crate::domain::gate::ports::{PortAnswer, Unavailable};
use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// [`IdentityReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdentityUnavailable;

#[async_trait]
impl IdentityReads for IdentityUnavailable {
    async fn payer_profile(
        &self,
        _ctx: &SecurityContext,
        _query: &PayerProfileQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<PayerProfile> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`ContractReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContractsUnavailable;

#[async_trait]
impl ContractReads for ContractsUnavailable {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _queries: &[ContractQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<BTreeMap<Uuid, ContractFacts>> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`OverlapKeyReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlapKeyUnavailable;

#[async_trait]
impl OverlapKeyReads for OverlapKeyUnavailable {
    async fn keys(
        &self,
        _ctx: &SecurityContext,
        _payer_tenant_id: Uuid,
        _resource_tenant_id: Option<Uuid>,
        _lines: &[OverlapKeyLine],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<OverlapKey>> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`OccupancyReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OccupancyUnavailable;

#[async_trait]
impl OccupancyReads for OccupancyUnavailable {
    async fn get_overlap_occupancy(
        &self,
        _ctx: &SecurityContext,
        _queries: &[OccupancyQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`BillingTermsReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BillingTermsUnavailable;

#[async_trait]
impl BillingTermsReads for BillingTermsUnavailable {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _queries: &[BillingTermsQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`AcceptancePolicyReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptancePolicyUnavailable;

#[async_trait]
impl AcceptancePolicyReads for AcceptancePolicyUnavailable {
    async fn policy(
        &self,
        _ctx: &SecurityContext,
        _query: &AcceptancePolicyQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`AssessmentReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssessmentUnavailable;

#[async_trait]
impl AssessmentReads for AssessmentUnavailable {
    async fn assess(
        &self,
        _ctx: &SecurityContext,
        _query: &AssessmentQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`EvaluationReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvaluationUnavailable;

#[async_trait]
impl EvaluationReads for EvaluationUnavailable {
    async fn evaluate(
        &self,
        _ctx: &SecurityContext,
        _request: &OrderEvaluationRequest,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<OrderEvaluation> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

/// [`IndicativeTaxReads`] with no provider: always `Missing`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndicativeTaxUnavailable;

#[async_trait]
impl IndicativeTaxReads for IndicativeTaxUnavailable {
    async fn estimate(
        &self,
        _ctx: &SecurityContext,
        _query: &TaxQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<IndicativeTax> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

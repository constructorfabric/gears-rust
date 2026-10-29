//! The nine gate ports whose providers are not delivered (D-227 item 1), typed by the Task 6a
//! request and answer types (`domain::gate::answers`). Each method reads as the given principal
//! under the run's budget and answers `PortAnswer::Unavailable` instead of failing; the
//! production adapters are the fail-closed `*Unavailable` structs (`unavailable.rs`) until each
//! provider lands.
use super::budget::RunBudget;
use crate::domain::gate::answers::{
    AcceptancePolicy, AcceptancePolicyQuery, AssessmentAnswer, AssessmentQuery, BillingTermsQuery,
    ContractFacts, ContractQuery, IndicativeTax, OccupancyObservation, OccupancyQuery,
    OrderEvaluation, OrderEvaluationRequest, OverlapKey, OverlapKeyLine, PayerProfile,
    PayerProfileQuery, ResolvedTerms, TaxQuery,
};
use crate::domain::gate::ports::PortAnswer;
use async_trait::async_trait;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Account management: the payer's commercial profile (D-199, D-215; S4-02).
#[async_trait]
pub trait IdentityReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn payer_profile(
        &self,
        ctx: &SecurityContext,
        query: &PayerProfileQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<PayerProfile>;
}

/// Contracts: party eligibility of the referenced contracts, by contract id (S4-02).
///
/// D-215/D-228: the orchestrator must not call it before the payer relationship is confirmed
/// (the identity answer has `in_relationship_with_seller` and `in_relationship_with_resource`
/// both true). With the relationship unconfirmed the assessment ignores this port's answer, its
/// rows carry `payer-relationship-unconfirmed` and its engine input is `Resolved` by the
/// composite gate's withholding (D-228 item 6).
#[async_trait]
pub trait ContractReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    ///
    /// The caller must not call it for an empty query set (an order naming no contract): it passes
    /// `PortAnswer::Answered` of an empty map instead.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        queries: &[ContractQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<BTreeMap<Uuid, ContractFacts>>;
}

/// Subscriptions: the catalog subscription product key of each line (#5131 SUB-G1; S5-06a).
///
/// D-215/D-228: the orchestrator must not call it before the payer relationship is confirmed
/// (the identity answer has `in_relationship_with_seller` and `in_relationship_with_resource`
/// both true). With the relationship unconfirmed the assessment ignores this port's answer, its
/// rows carry `payer-relationship-unconfirmed` and its engine input is `Resolved` by the
/// composite gate's withholding (D-228 item 6).
#[async_trait]
pub trait OverlapKeyReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    ///
    /// The caller must not call it for an empty line set: it passes `PortAnswer::Answered` of an
    /// empty list instead.
    async fn keys(
        &self,
        ctx: &SecurityContext,
        payer_tenant_id: Uuid,
        resource_tenant_id: Option<Uuid>,
        lines: &[OverlapKeyLine],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<OverlapKey>>;
}

/// Subscriptions: overlap presence per scope (#5131 `getOverlapOccupancy`; S5-06a).
///
/// D-215/D-228: the orchestrator must not call it before the payer relationship is confirmed
/// (the identity answer has `in_relationship_with_seller` and `in_relationship_with_resource`
/// both true). With the relationship unconfirmed the assessment ignores this port's answer, its
/// rows carry `payer-relationship-unconfirmed` and its engine input is `Resolved` by the
/// composite gate's withholding (D-228 item 6).
#[async_trait]
pub trait OccupancyReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    ///
    /// The caller must not call it for an empty query set: it passes `PortAnswer::Answered` of an
    /// empty list instead.
    async fn get_overlap_occupancy(
        &self,
        ctx: &SecurityContext,
        queries: &[OccupancyQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>>;
}

/// Subscriptions: pre-subscription billing terms per line (D-193; S3-04).
#[async_trait]
pub trait BillingTermsReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        queries: &[BillingTermsQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>>;
}

/// Pricing: the seller's acceptance policy; `None` is an answered missing policy (D-191; S3-05).
#[async_trait]
pub trait AcceptancePolicyReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn policy(
        &self,
        ctx: &SecurityContext,
        query: &AcceptancePolicyQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>>;
}

/// Pricing: the purchase assessment of predicates 2, 7 and 8 (D-189; S3-03).
#[async_trait]
pub trait AssessmentReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn assess(
        &self,
        ctx: &SecurityContext,
        query: &AssessmentQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer>;
}

/// Rating: the order evaluation (#5390/#5175; S3-07).
#[async_trait]
pub trait EvaluationReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn evaluate(
        &self,
        ctx: &SecurityContext,
        request: &OrderEvaluationRequest,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<OrderEvaluation>;
}

/// Unowned: the indicative tax read, Preview only (S3-08).
#[async_trait]
pub trait IndicativeTaxReads: Send + Sync {
    /// The port's answer, read as `ctx` under `run` and `cancel`.
    async fn estimate(
        &self,
        ctx: &SecurityContext,
        query: &TaxQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<IndicativeTax>;
}

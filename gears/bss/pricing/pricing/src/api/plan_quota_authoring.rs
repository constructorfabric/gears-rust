//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-authoring:p1
//! The local client of `PricingQuotaAuthoringClientV1` (D-540): the quota rules read, their
//! replacement and the revision's checks, in-process, over the same functions as the endpoints.
//!
//! Each method authenticates and authorizes the caller as its endpoint does (`read` or `author`
//! on `plan`, before anything is read), then calls the endpoint's own function, so both surfaces
//! answer a request alike: the same body, the same refusals and the same version. A replacement
//! is sent as the JSON body its `PUT` would carry, so its request digest, its stored answer and
//! the parse of its rules are the `PUT`'s. The key space is shared: the key is bound to the
//! `If-Match` version and the rules as they parse, so a key used on either surface replays the
//! same rules on the other, whatever the REST body's spelling.
use crate::api::rest::authoring::{
    AuthoringState,
    dto::{PricingPlanCheckDto, PricingPlanChecksDto, PricingPlanQuotaRulesDto},
    plan_quota::{self, Replace},
    plans,
    support::{authz_failure, require_authenticated},
};
use crate::api::rest::closed_sets::PricingQuotaAvailabilityState;
use crate::api::rest::preconditions;
use crate::authz::{self, OwnerTenant, actions, resource_types};
use crate::domain::plan_quota::digest_from_hex;
pub use crate::infra::plan_quota::rule_json;
use authz_resolver_sdk::PolicyEnforcer;
use bss_pricing_sdk::plan_quota::{
    PlanCheckBlockingPriceV1, PlanCheckSubjectV1, PlanCheckV1, PlanQuotaAvailability,
    PlanQuotaAvailabilityState, PlanQuotaRuleV1, PlanQuotaRulesV1, PlanRevisionChecksV1,
    PricingQuotaAuthoringClientV1, QuotaDefinitionRef, SavedPlanQuotaRuleV1,
};
use serde_json::json;
use std::num::NonZeroU64;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The local client, registered in the `ClientHub` at init.
pub struct PricingQuotaAuthoringProvider {
    state: Arc<AuthoringState>,
    enforcer: Arc<PolicyEnforcer>,
}

impl PricingQuotaAuthoringProvider {
    /// The client over the gear's runtime and its policy enforcer.
    #[must_use]
    pub fn new(state: Arc<AuthoringState>, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { state, enforcer }
    }

    /// The caller authenticated as an endpoint's (401, and 403 `SYSTEM_ACTOR_RESERVED` for
    /// pricing's own system actor), then its grant of `action` on `plan`: an author's scope names
    /// the caller's tenant as the owner, as the `PUT` does.
    async fn scope(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
    ) -> Result<AccessScope, CanonicalError> {
        let ctx = require_authenticated(Some(axum::Extension(ctx.clone())))?;
        let owner = (action == actions::AUTHOR).then(|| OwnerTenant(ctx.subject_tenant_id()));
        authz::access_scope(
            &self.enforcer,
            &ctx,
            &resource_types::PLAN,
            action,
            owner,
            None,
        )
        .await
        .map_err(authz_failure)
    }
}

#[async_trait::async_trait]
impl PricingQuotaAuthoringClientV1 for PricingQuotaAuthoringProvider {
    async fn get_quota_rules(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
    ) -> Result<PlanQuotaRulesV1, CanonicalError> {
        let scope = self.scope(ctx, actions::READ).await?;
        let (body, _) = plan_quota::rules(&self.state, scope, ctx, revision_id).await?;
        rules_v1(body)
    }

    async fn replace_quota_rules(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
        expected_version: u64,
        operation_id: &str,
        rules: Vec<PlanQuotaRuleV1>,
    ) -> Result<PlanQuotaRulesV1, CanonicalError> {
        let scope = self.scope(ctx, actions::AUTHOR).await?;
        preconditions::validate_idempotency_key(operation_id)?;
        let request = Replace {
            id: revision_id,
            version: expected_version,
            key: operation_id.to_owned(),
            body: json!({ "quota_rules": rules.iter().map(rule_json).collect::<Vec<_>>() }),
        };
        // An in-process call carries no request's correlation id: each call gets its own.
        let answer = plan_quota::replace(&self.state, scope, ctx, Uuid::now_v7(), request).await?;
        if !answer.status.is_success() {
            return Err(unreadable("a quota rules answer that is not a success"));
        }
        let body: PricingPlanQuotaRulesDto = serde_json::from_value(answer.body)
            .map_err(|_| unreadable("a stored quota rules answer"))?;
        rules_v1(body)
    }

    async fn check_plan_revision(
        &self,
        ctx: &SecurityContext,
        revision_id: Uuid,
    ) -> Result<PlanRevisionChecksV1, CanonicalError> {
        let scope = self.scope(ctx, actions::READ).await?;
        checks_v1(plans::checks_body(&self.state, scope, ctx, revision_id).await?)
    }
}

/// 500: an answer Pricing wrote that does not read back.
fn unreadable(what: &str) -> CanonicalError {
    tracing::error!(what, "pricing: a quota authoring answer does not read back");
    CanonicalError::internal(format!("{what} does not read back")).create()
}

fn version(text: &str, what: &str) -> Result<NonZeroU64, CanonicalError> {
    text.parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(|| unreadable(what))
}

fn digest(text: &str, what: &str) -> Result<bss_pricing_sdk::Digest, CanonicalError> {
    digest_from_hex(text).ok_or_else(|| unreadable(what))
}

fn stored_version(version: i64) -> Result<u64, CanonicalError> {
    u64::try_from(version).map_err(|_| unreadable("a revision version"))
}

/// The quota rules answer of the endpoints as the SDK shapes it.
/// # Errors
/// 500 for an answer whose version or digest does not read.
pub fn rules_v1(body: PricingPlanQuotaRulesDto) -> Result<PlanQuotaRulesV1, CanonicalError> {
    let quota_rules = body
        .quota_rules
        .into_iter()
        .map(|rule| {
            Ok(SavedPlanQuotaRuleV1 {
                rule: PlanQuotaRuleV1 {
                    rule_id: rule.rule_id,
                    definition_ref: QuotaDefinitionRef {
                        id: rule.definition_ref.id,
                        version: version(&rule.definition_ref.version, "a definition version")?,
                        digest: digest(&rule.definition_ref.digest, "a definition digest")?,
                    },
                    item_id: rule.item_id,
                    dimension_value: rule.dimension_value,
                    logical_binding_key: rule.logical_binding_key,
                    subject_selector: rule.subject_selector.into(),
                    scope_selection: rule.scope_selection,
                    amount: rule.amount,
                    unit_ref: rule.unit_ref,
                    basis: rule.basis.into(),
                },
                availability: PlanQuotaAvailability {
                    state: match rule.availability.state {
                        PricingQuotaAvailabilityState::Enabled => {
                            PlanQuotaAvailabilityState::Enabled
                        }
                        PricingQuotaAvailabilityState::Disabled => {
                            PlanQuotaAvailabilityState::Disabled
                        }
                        PricingQuotaAvailabilityState::Withdrawn => {
                            PlanQuotaAvailabilityState::Withdrawn
                        }
                    },
                    revision: version(&rule.availability.revision, "an availability revision")?,
                    observed_at: rule.availability.observed_at,
                    reason_code: rule.availability.reason_code,
                },
            })
        })
        .collect::<Result<Vec<_>, CanonicalError>>()?;
    Ok(PlanQuotaRulesV1 {
        revision_id: body.revision_id,
        revision_version: stored_version(body.revision_version)?,
        quota_rules,
        quota_rules_digest: digest(&body.quota_rules_digest, "a quota rules digest")?,
    })
}

/// The checks answer of the endpoints as the SDK shapes it.
/// # Errors
/// 500 for an answer whose sale date, version or digest does not read.
pub fn checks_v1(body: PricingPlanChecksDto) -> Result<PlanRevisionChecksV1, CanonicalError> {
    let sale_date = time::Date::parse(
        &body.sale_date,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| unreadable("a sale date"))?;
    Ok(PlanRevisionChecksV1 {
        checks: body.checks.into_iter().map(check_v1).collect(),
        ready: body.ready,
        sale_date,
        quorum_required: body.quorum_required,
        revision_version: stored_version(body.revision_version)?,
        quota_rules_digest: digest(&body.quota_rules_digest, "a quota rules digest")?,
    })
}

fn check_v1(row: PricingPlanCheckDto) -> PlanCheckV1 {
    PlanCheckV1 {
        code: row.code,
        ok: row.ok,
        label: row.label,
        detail: row.detail,
        info: row.info,
        blocked_by: row.blocked_by,
        subjects: row
            .subjects
            .into_iter()
            .map(|s| PlanCheckSubjectV1 {
                item_id: s.item_id,
                sku_id: s.sku_id,
                price_book_entry_id: s.price_book_entry_id,
                quota_rule_id: s.quota_rule_id,
                field: s.field,
            })
            .collect(),
        blocked_by_prices: row
            .blocked_by_prices
            .into_iter()
            .map(|p| PlanCheckBlockingPriceV1 {
                unit_id: p.unit_id,
                price_id: p.price_id,
                price_book_entry_id: p.price_book_entry_id,
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "plan_quota_authoring_tests.rs"]
mod plan_quota_authoring_tests;

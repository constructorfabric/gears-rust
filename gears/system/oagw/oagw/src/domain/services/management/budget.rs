use crate::domain::error::DomainError;
use crate::domain::model::BudgetMode;
use crate::domain::repo::Tags;

use super::ControlPlaneServiceImpl;
use super::merge::{rate_per_second, window_to_secs};

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Validate budget configuration field constraints per ADR 0004 schema.
pub(in crate::domain::services) fn validate_budget_config(
    budget: &crate::domain::model::BudgetConfig,
) -> Result<(), DomainError> {
    match budget.mode {
        BudgetMode::Allocated | BudgetMode::Shared => {
            let total = budget.total.ok_or_else(|| {
                DomainError::validation(
                    "budget.total is required when budget.mode is 'allocated' or 'shared'",
                )
            })?;
            if total < 1 {
                return Err(DomainError::validation("budget.total must be at least 1"));
            }
        }
        BudgetMode::Unlimited => {}
    }

    if let Some(ratio) = budget.overcommit_ratio
        && !(1.0..=2.0).contains(&ratio)
    {
        return Err(DomainError::validation(
            "budget.overcommit_ratio must be between 1.0 and 2.0",
        ));
    }

    Ok(())
}

impl ControlPlaneServiceImpl {
    /// Validate that this child's rate limit allocation fits within the
    /// ancestor's budget. Only runs when the closest ancestor with matching
    /// alias has `budget.mode == Allocated`.
    pub(super) async fn validate_budget_allocation(
        &self,
        ctx: &SecurityContext,
        requesting_tenant: Uuid,
        ancestor: Option<&crate::domain::model::Upstream>,
        alias: &str,
        child_rate_limit: Option<&crate::domain::model::RateLimitConfig>,
    ) -> Result<(), DomainError> {
        let Some(ancestor) = ancestor else {
            return Ok(()); // No ancestor with this alias — nothing to validate.
        };

        let ancestor_budget = match ancestor
            .rate_limit
            .as_ref()
            .and_then(|rl| rl.budget.as_ref())
        {
            Some(b) if b.mode == BudgetMode::Allocated => b,
            _ => return Ok(()), // Not allocated mode — no budget validation.
        };

        // Under allocated budget, children must specify an explicit rate_limit
        // so their allocation is accounted for at write time.
        if child_rate_limit.is_none() {
            return Err(DomainError::validation(
                "rate_limit is required when ancestor has budget.mode = 'allocated'",
            ));
        }

        let budget_total = match ancestor_budget.total {
            Some(t) => t,
            None => return Ok(()), // No total specified — nothing to enforce.
        };
        let ratio = ancestor_budget.overcommit_ratio.unwrap_or(1.0);

        // Get the ancestor's descendant tree first, then query only upstreams
        // belonging to those tenants — avoids loading upstreams from unrelated trees.
        let descendant_resp = self
            .tenant_resolver
            .get_descendants(
                ctx,
                tenant_resolver_sdk::TenantId(ancestor.tenant_id),
                &tenant_resolver_sdk::GetDescendantsOptions::default(),
            )
            .await?;
        let tree_ids: std::collections::HashSet<Uuid> =
            descendant_resp.descendants.iter().map(|t| t.id.0).collect();

        let siblings = self
            .upstreams
            .list_by_alias_for_tenants(alias, &tree_ids, Tags::Skip)
            .await?;

        let ancestor_rl = ancestor.rate_limit.as_ref().unwrap(); // safe: we checked above

        let mut total_rps: f64 = 0.0;
        for sibling in &siblings {
            // Skip the ancestor itself and the requesting tenant (we'll add child_rate_limit separately).
            if sibling.tenant_id == ancestor.tenant_id || sibling.tenant_id == requesting_tenant {
                continue;
            }
            if let Some(ref rl) = sibling.rate_limit {
                total_rps += rate_per_second(rl);
            }
        }

        // Add this child's proposed rate.
        if let Some(child_rl) = child_rate_limit {
            total_rps += rate_per_second(child_rl);
        }

        let budget_rps = f64::from(budget_total) / window_to_secs(ancestor_rl.sustained.window);
        let allowed_rps = budget_rps * ratio;

        if total_rps > allowed_rps {
            return Err(DomainError::validation(format!(
                "budget allocation exceeded: children total {total_rps:.2} req/s \
                 exceeds ancestor budget {budget_rps:.2} req/s × {ratio} overcommit ratio \
                 (allowed: {allowed_rps:.2} req/s)"
            )));
        }

        Ok(())
    }

    /// Validate that a parent's proposed budget still accommodates existing
    /// descendant allocations. Called when `update_upstream` changes budget
    /// parameters (total, overcommit_ratio, or mode) to prevent retroactively
    /// exceeding the ceiling.
    pub(super) async fn validate_descendants_within_budget(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        proposed_rl: &crate::domain::model::RateLimitConfig,
    ) -> Result<(), DomainError> {
        let budget = match proposed_rl.budget.as_ref() {
            Some(b) if b.mode == BudgetMode::Allocated => b,
            _ => return Ok(()), // Not allocated mode — nothing to enforce.
        };

        let budget_total = match budget.total {
            Some(t) => t,
            None => return Ok(()), // No total specified — nothing to enforce.
        };
        let ratio = budget.overcommit_ratio.unwrap_or(1.0);

        let tenant_id = ctx.subject_tenant_id();
        let descendant_resp = self
            .tenant_resolver
            .get_descendants(
                ctx,
                tenant_resolver_sdk::TenantId(tenant_id),
                &tenant_resolver_sdk::GetDescendantsOptions::default(),
            )
            .await?;
        let tree_ids: std::collections::HashSet<Uuid> =
            descendant_resp.descendants.iter().map(|t| t.id.0).collect();

        if tree_ids.is_empty() {
            return Ok(()); // No descendants — nothing to check.
        }

        let descendants = self
            .upstreams
            .list_by_alias_for_tenants(alias, &tree_ids, Tags::Skip)
            .await?;

        let mut total_rps: f64 = 0.0;
        for descendant in &descendants {
            if let Some(ref rl) = descendant.rate_limit {
                total_rps += rate_per_second(rl);
            }
        }

        let budget_rps = f64::from(budget_total) / window_to_secs(proposed_rl.sustained.window);
        let allowed_rps = budget_rps * ratio;

        if total_rps > allowed_rps {
            return Err(DomainError::validation(format!(
                "cannot lower budget: existing descendant allocations total {total_rps:.2} req/s \
                 which exceeds proposed budget {budget_rps:.2} req/s × {ratio} overcommit ratio \
                 (allowed: {allowed_rps:.2} req/s)"
            )));
        }

        Ok(())
    }
}

use std::collections::{HashMap, HashSet};

use crate::domain::error::DomainError;
use crate::domain::model::{BudgetMode, Upstream};
use crate::domain::repo::Tags;

use super::ControlPlaneServiceImpl;
use super::merge::{
    effective_rate_limit, is_visible_to_descendant, rate_per_second, shared_upstream_view,
    window_to_secs,
};

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

/// Reject a write after which every request on `proposed` would cost more
/// than the whole shared pool it draws on, and be answered 429 forever.
///
/// The cost and the pool are those of the effective limit the proxy merges
/// over `ancestors` (closest first, as [`ancestor_upstreams`] gives them)
/// and `proposed`: the pool is charged the upstream's effective cost, a
/// route's own cost goes to the route's bucket only. Two chains are checked:
/// the enabled ancestors, which the proxy merges today, and all of them, as
/// in bind validation, so that enabling a disabled one does not break the
/// descendant. A disabled ancestor's stricter limit can lower the cost, so
/// passing one chain does not imply passing the other. In each chain, a
/// shortfall the ancestors' own effective limit already has does not come
/// from this write and does not block it.
///
/// [`ancestor_upstreams`]: super::ancestor::ancestor_upstreams
pub(in crate::domain::services) fn validate_shared_pool_cost(
    ancestors: &[Upstream],
    proposed: &Upstream,
) -> Result<(), DomainError> {
    check_shared_pool_cost(ancestors.iter().filter(|u| u.enabled), proposed)?;
    check_shared_pool_cost(ancestors.iter(), proposed)
}

/// [`validate_shared_pool_cost`] for one chain of `ancestors`, closest first.
fn check_shared_pool_cost<'a>(
    ancestors: impl DoubleEndedIterator<Item = &'a Upstream>,
    proposed: &Upstream,
) -> Result<(), DomainError> {
    // (cost, pool) when the cost exceeds the pool's total.
    let overdraw = |chain: &[Upstream]| {
        effective_rate_limit(chain).and_then(|rl| {
            rl.shared_pool
                .filter(|pool| rl.cost > pool.total)
                .map(|pool| (rl.cost, pool))
        })
    };
    // Same visibility filter as the proxy's upstream selection.
    let mut chain: Vec<Upstream> = ancestors
        .rev()
        .filter(|u| is_visible_to_descendant(u))
        .cloned()
        .map(shared_upstream_view)
        .collect();
    let inherited = overdraw(&chain);
    chain.push(proposed.clone());
    let Some((cost, pool)) = overdraw(&chain) else {
        return Ok(());
    };
    if inherited.is_some_and(|(c, p)| (c, p.total) == (cost, pool.total)) {
        return Ok(());
    }
    let total = pool.total;
    Err(DomainError::validation(if pool.owner_id == proposed.id {
        format!("rate_limit.cost ({cost}) must not exceed rate_limit.budget.total ({total})")
    } else {
        format!(
            "rate_limit.cost ({cost}) must not exceed budget.total ({total}) of the shared \
             budget an ancestor's upstream defines for this alias"
        )
    }))
}

/// The upstreams in `upstreams` that allocate from `owner`'s budget: those
/// of tenants below `owner` whose closest ancestor with the alias is `owner`
/// itself. An upstream below another tenant with the alias allocates from
/// that tenant's limit instead. `tree` is `owner`'s descendants (each with
/// its parent); `bound` adds tenants that have the alias but no upstream in
/// `upstreams` yet (the tenant being created).
fn direct_allocations<'a>(
    owner: Uuid,
    tree: &[tenant_resolver_sdk::TenantRef],
    upstreams: &'a [Upstream],
    bound: Option<Uuid>,
) -> impl Iterator<Item = &'a Upstream> {
    let parents: HashMap<Uuid, Option<Uuid>> = tree
        .iter()
        .map(|t| (t.id.0, t.parent_id.map(|p| p.0)))
        .collect();
    let with_alias: HashSet<Uuid> = upstreams.iter().map(|u| u.tenant_id).chain(bound).collect();
    upstreams.iter().filter(move |u| {
        // Bounded by the tree size, so a malformed (cyclic) answer ends.
        let mut tenant = parents.get(&u.tenant_id).copied().flatten();
        for _ in 0..=parents.len() {
            match tenant {
                Some(t) if t == owner => return true,
                Some(t) if with_alias.contains(&t) => return false,
                Some(t) => tenant = parents.get(&t).copied().flatten(),
                None => return false,
            }
        }
        false
    })
}

impl ControlPlaneServiceImpl {
    /// Validate that this child's rate limit allocation fits within the
    /// ancestor's budget. Only runs when the closest ancestor with matching
    /// alias has `budget.mode == Allocated`. Only the ancestor's direct
    /// allocations count (see [`direct_allocations`]).
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
        let tree_ids: HashSet<Uuid> = descendant_resp.descendants.iter().map(|t| t.id.0).collect();

        let siblings = self
            .upstreams
            .list_by_alias_for_tenants(alias, &tree_ids, Tags::Skip)
            .await?;

        let ancestor_rl = ancestor.rate_limit.as_ref().unwrap(); // safe: we checked above

        let mut total_rps: f64 = 0.0;
        for sibling in direct_allocations(
            ancestor.tenant_id,
            &descendant_resp.descendants,
            &siblings,
            Some(requesting_tenant),
        ) {
            // The requesting tenant's stored allocation is replaced by the
            // proposed one, added below.
            if sibling.tenant_id == requesting_tenant {
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

    /// Validate that a parent's proposed budget still accommodates its
    /// existing direct allocations (see [`direct_allocations`]). Called when
    /// `update_upstream` changes the budget (total, overcommit_ratio, or
    /// mode) or the window it is counted in, to prevent retroactively
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
        let tree_ids: HashSet<Uuid> = descendant_resp.descendants.iter().map(|t| t.id.0).collect();

        if tree_ids.is_empty() {
            return Ok(()); // No descendants — nothing to check.
        }

        let descendants = self
            .upstreams
            .list_by_alias_for_tenants(alias, &tree_ids, Tags::Skip)
            .await?;

        let mut total_rps: f64 = 0.0;
        for descendant in
            direct_allocations(tenant_id, &descendant_resp.descendants, &descendants, None)
        {
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

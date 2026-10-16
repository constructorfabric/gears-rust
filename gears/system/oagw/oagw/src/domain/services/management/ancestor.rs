use std::collections::HashSet;

use crate::domain::error::DomainError;
use crate::domain::model::Upstream;
use crate::domain::repo::{Tags, UpstreamRepository};

use uuid::Uuid;

/// Find the closest ancestor upstream with `alias` in one repository call
/// (`cpt-cf-oagw-algo-tenant-closest-ancestor`).
///
/// `tenant_chain` is `[self, parent, ..., root]`; only `tenant_chain[1..]` is
/// searched. The closest match is returned regardless of its enabled flag or
/// sharing modes. Upstream create and update run this once and pass the
/// result to both bind validation and budget allocation validation.
pub(in crate::domain::services) async fn closest_ancestor_upstream(
    upstreams: &dyn UpstreamRepository,
    tenant_chain: &[Uuid],
    alias: &str,
) -> Result<Option<Upstream>, DomainError> {
    let ancestors = tenant_chain.get(1..).unwrap_or_default();
    if ancestors.is_empty() {
        return Ok(None);
    }
    let tenant_ids: HashSet<Uuid> = ancestors.iter().copied().collect();
    let found = upstreams
        .list_by_alias_for_tenants(alias, &tenant_ids, Tags::Skip)
        .await?;
    Ok(found
        .into_iter()
        .min_by_key(|u| ancestors.iter().position(|t| *t == u.tenant_id)))
}

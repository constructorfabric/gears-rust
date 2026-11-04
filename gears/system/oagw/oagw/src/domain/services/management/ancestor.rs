use std::collections::HashSet;

use crate::domain::error::DomainError;
use crate::domain::model::Upstream;
use crate::domain::repo::{Tags, UpstreamRepository};

use uuid::Uuid;

/// Every ancestor upstream with `alias`, closest first, in one repository
/// call (`cpt-cf-oagw-algo-tenant-closest-ancestor`).
///
/// `tenant_chain` is `[self, parent, ..., root]`; only `tenant_chain[1..]` is
/// searched, regardless of enabled flags or sharing modes. Upstream create and
/// update run this once: the closest goes to both bind validation and budget
/// allocation validation, the whole list to the enforced-auth check.
pub(in crate::domain::services) async fn ancestor_upstreams(
    upstreams: &dyn UpstreamRepository,
    tenant_chain: &[Uuid],
    alias: &str,
) -> Result<Vec<Upstream>, DomainError> {
    let ancestors = tenant_chain.get(1..).unwrap_or_default();
    if ancestors.is_empty() {
        return Ok(Vec::new());
    }
    let tenant_ids: HashSet<Uuid> = ancestors.iter().copied().collect();
    let mut found = upstreams
        .list_by_alias_for_tenants(alias, &tenant_ids, Tags::Skip)
        .await?;
    found.sort_by_key(|u| ancestors.iter().position(|t| *t == u.tenant_id));
    Ok(found)
}

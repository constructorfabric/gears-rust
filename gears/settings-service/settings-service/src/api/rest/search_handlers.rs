// Created: 2026-09-17 by Virtuozzo International GmbH
//! The search handler: browse's gates, then the corpus decision, then the
//! service — and the `hidden` exclusion on the way out.

use std::sync::Arc;

use axum::extract::Query;
use axum::{Extension, Json};
use serde::Deserialize;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::search_dto::{SearchHitDto, render_hit};
use crate::api::rest::setting_handlers::{
    conn_error, gate_target, may_read_pii, parse_tenant, unsupported,
};
use crate::domain::category::visibility;
use crate::domain::resolution::ScopeTarget;
use crate::domain::search::service::SearchService;
use crate::domain::search::{Corpus, Needle, SearchRequest, cursor_binding};
use crate::gear::ConcreteResolver;
use crate::infra::storage::search_repo::SearchRepo;

/// The service over the concrete repository binding.
pub type ConcreteSearchService = SearchService<SearchRepo>;

const READ: &str = "read";

/// The plain query parameters; `limit` and `cursor` arrive through the `OData`
/// extractor, which is also what refuses the options this resource does not
/// take.
#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    /// Free text, at least two characters.
    #[serde(default)]
    pub q: Option<String>,
    /// The target tenant; absent, the caller's own.
    #[serde(default)]
    pub tenant: Option<String>,
}

/// `GET /settings-service/v1/search?q={query}&tenant={tenant_id}` with
/// `limit` and `cursor`.
///
/// # Errors
/// 400 when `q` is absent, shorter than two characters or longer than two
/// hundred, on a malformed `tenant`, on `$filter`, `$orderby` or `$select`,
/// or on a cursor minted for another search; 403 when the caller may not read
/// values or the target is outside its subtree or standalone; 503 when a
/// dependency cannot answer.
// @cpt-dod:cpt-cf-settings-service-dod-search-discoverability-surface:p2
pub async fn search_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(search): Extension<Arc<ConcreteSearchService>>,
    Extension(resolver): Extension<Arc<ConcreteResolver>>,
    Extension(db): Extension<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Query(params): Query<SearchParams>,
    OData(query): OData,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-1
    let needle = Needle::parse(params.q.as_deref().unwrap_or_default())?;
    let requested = parse_tenant(params.tenant.as_deref())?;
    if query.filter.is_some() || !query.order.0.is_empty() || query.select.is_some() {
        return Err(unsupported(
            "search takes `q`, `tenant`, `limit` and `cursor`; results are ordered by key",
        )
        .into());
    }
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-1

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-2
    // One decision on the value resource for the whole request; its
    // constraints are the secure scope of the declarations query, so a setting
    // the caller may not read is absent from the results and from the count.
    let scope: AccessScope =
        authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-2

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-3
    let target = gate_target(&resolver, &ctx, requested).await?;
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-3
    let root = resolver.root_tenant().await?;
    let target_tenant = target.tenant_id(root);

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-4
    // The override corpus: the target and its descendants, standalone ones
    // excluded by the hierarchy — exactly the rows the caller could read.
    let mut tenant_ids = resolver.hierarchy().descendants(target_tenant).await?;
    tenant_ids.push(target_tenant);
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-4

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-5
    // Decided before any query, not after the page comes back: whether `pii`
    // content is in the corpus at all. Browse asks only when a page carries
    // pii; search has to ask every time, since the answer shapes the query.
    let pii = may_read_pii(&enforcer, &ctx).await;
    let corpus = Corpus::for_caller(pii);
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-5

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-6
    let mut query: ODataQuery = query;
    query.filter_hash = Some(cursor_binding(&needle, target_tenant, corpus));
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-6

    let conn = db.conn().map_err(|e| conn_error(&e))?;
    let domain_visibility = visibility::domain_visibility(&scope);
    let page = search
        .search(
            &conn,
            &SearchRequest {
                scope: &scope,
                visibility: &domain_visibility,
                needle: &needle,
                corpus,
                tenant_ids: &tenant_ids,
                query: &query,
            },
        )
        .await?;

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-10
    // A setting hidden from the caller leaves silently, hits and all — never
    // marked, exactly as browse drops it.
    let mut ids: Vec<Uuid> = page.hits.iter().map(|h| h.declaration.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let caller = ScopeTarget::Tenant(ctx.subject_tenant_id()).normalize(root);
    let access = resolver.effective_access_for(&conn, &ids, caller).await?;
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-10

    // @cpt-begin:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-11
    let items: Vec<SearchHitDto> = page
        .hits
        .iter()
        .filter(|h| !access.get(&h.declaration.id).is_some_and(|a| a.is_hidden()))
        .map(|h| render_hit(h, root, pii))
        .collect();
    Ok(Json(toolkit_odata::Page {
        items,
        page_info: page.page_info,
    }))
    // @cpt-end:cpt-cf-settings-service-flow-search-discoverability-search:p2:inst-sd-search-11
}

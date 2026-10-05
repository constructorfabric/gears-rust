//! REST arm of pricing's `ProductCatalogClientV1`.
//!
//! `#[toolkit::provides]` constructs this with `new(cfg)` where `cfg` is
//! `tuning.apply_to(endpoint).with_internal_token_provider(...)`. The client
//! calls the existing browse door (`GET /bss-products/v1/browse?kind=sku`)
//! rather than a generated contract surface beside the trait.

use async_trait::async_trait;
use bss_pricing_sdk::product_catalog::{
    CatalogSku, CatalogSkuPage, CatalogTaxCategory, ProductCatalogClientV1, catalog_unreachable,
};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use toolkit::contract_support::runtime::config::ClientConfig;
use toolkit::contract_support::runtime::http::parse_retry_after;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Browse door the REST arm calls.
const BROWSE_PATH: &str = "/bss-products/v1/browse";

/// Same ceiling the browse door serves.
const BROWSE_MAX: u32 = 200;

/// REST [`ProductCatalogClientV1`] over `GET /bss-products/v1/browse`.
pub struct ProductCatalogRestClient {
    http: HttpClient,
    config: ClientConfig,
}

impl ProductCatalogRestClient {
    /// Build the transport the provides-macro REST arm expects.
    ///
    /// # Errors
    ///
    /// [`toolkit_http::HttpError`] when the default HTTP client cannot be built.
    pub fn new(config: ClientConfig) -> Result<Self, toolkit_http::HttpError> {
        let http = toolkit::contract_support::runtime::client::build_default_http_client(
            "ProductCatalogRestClient",
            &config,
        )?;
        Ok(Self { http, config })
    }
}

#[derive(Debug, Deserialize)]
struct BrowseView {
    rows: Vec<BrowseRowView>,
    page_info: BrowsePageInfo,
}

#[derive(Debug, Deserialize)]
struct BrowsePageInfo {
    next_cursor: Option<String>,
}

/// Retained wire parser, including legacy projection fields.
#[allow(dead_code, reason = "Legacy fields stay in the kept REST contract")]
#[derive(Debug, Deserialize)]
struct BrowseRowView {
    entity_kind: String,
    entity_id: Uuid,
    entity_code: Option<String>,
    name: String,
    lifecycle_state: String,
    deprecated: bool,
    composition_pending: bool,
    sellable: Option<bool>,
    deprecation_provenance: Option<String>,
    replaced_by_sku_id: Option<Uuid>,
    #[serde(default)]
    region_scope: String,
    #[serde(default)]
    brand_scope: String,
    sku_type: Option<String>,
    plan_tier_label: Option<String>,
    metering_unit: Option<String>,
    usage_type_ref: Option<String>,
    display_attributes: Option<String>,
    category_paths: Option<String>,
    #[serde(default)]
    published_version: i64,
}

fn map_rows(rows: Vec<BrowseRowView>, _tenant_id: Uuid) -> Result<Vec<CatalogSku>, CanonicalError> {
    rows.into_iter()
        .filter(|r| r.entity_kind == "sku")
        .map(|r| {
            let missing = |field| {
                CanonicalError::internal(format!("products catalog mapping: missing {field}"))
                    .create()
            };
            Ok(CatalogSku {
                sku_id: r.entity_id,
                sku_code: r.entity_code.ok_or_else(|| missing("sku_code"))?,
                name: r.name,
                metering_unit: r.metering_unit,
                status: r.lifecycle_state,
                plan_tier: r.plan_tier_label,
                sku_type: r.sku_type.ok_or_else(|| missing("sku_type"))?,
                sellable: r.sellable.ok_or_else(|| missing("sellable"))?,
                usage_type_ref: r.usage_type_ref,
                deprecated: r.deprecated,
            })
        })
        .collect()
}

fn encode_query(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
    }
    out
}

fn ids_filter(ids: &[Uuid]) -> String {
    ids.iter()
        .map(|id| format!("entity_id eq {id}"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn search_filter(q: Option<&str>) -> Option<String> {
    let prefix = q.filter(|s| !s.is_empty())?;
    let escaped = prefix.replace('\'', "''");
    Some(format!("startswith(name,'{escaped}')"))
}

fn browse_url(base: &str, filter: Option<&str>, limit: u32, cursor: Option<&str>) -> String {
    let mut url = format!(
        "{}{BROWSE_PATH}?kind=sku&limit={}",
        base.trim_end_matches('/'),
        limit.min(BROWSE_MAX)
    );
    if let Some(filter) = filter.filter(|s| !s.is_empty()) {
        url.push_str("&$filter=");
        url.push_str(&encode_query(filter));
    }
    if let Some(cursor) = cursor.filter(|s| !s.is_empty()) {
        url.push_str("&cursor=");
        url.push_str(&encode_query(cursor));
    }
    url
}

/// Surface browse's 503 with **its** `Retry-After`. Do not invent a delay. A 4xx is the door's
/// refusal of this call (a caller without `sku` read, a `$filter` or a cursor it refused): its
/// Problem passes through with its class, never as a retryable outage (RS-06). A 4xx that carries
/// no Problem is an answer the client cannot use, a 500; any other status is a catalog that did
/// not answer.
pub(crate) fn catalog_error_from_http(
    status: u16,
    retry_after: Option<std::time::Duration>,
    body: &[u8],
) -> CanonicalError {
    if status == 503 {
        let mut builder = CanonicalError::service_unavailable();
        if let Some(delay) = retry_after {
            builder = builder.with_retry_after_seconds(delay.as_secs());
        }
        if let Ok(problem) = serde_json::from_slice::<Problem>(body)
            && !problem.detail.is_empty()
        {
            builder = builder.with_detail(problem.detail);
        }
        return builder.create();
    }
    if (400..500).contains(&status) {
        return serde_json::from_slice::<Problem>(body)
            .ok()
            .and_then(|problem| CanonicalError::try_from(problem).ok())
            .unwrap_or_else(|| {
                tracing::error!(
                    status,
                    "bss-products: the browse door answered a 4xx without a problem"
                );
                CanonicalError::internal(format!(
                    "products catalog browse: HTTP {status} without a problem"
                ))
                .create()
            });
    }
    tracing::warn!(status, "bss-products: the browse door did not answer");
    catalog_unreachable(format!("the product catalog answered HTTP {status}"))
}

impl ProductCatalogRestClient {
    async fn browse(
        &self,
        ctx: &SecurityContext,
        filter: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<BrowseView, CanonicalError> {
        let url = browse_url(&self.config.base_url, filter, limit, cursor);
        self.read_url(ctx, &url).await
    }

    async fn read_url<T: serde::de::DeserializeOwned>(
        &self,
        ctx: &SecurityContext,
        url: &str,
    ) -> Result<T, CanonicalError> {
        let mut request = self.http.get(url);
        if let Some(token) = ctx.bearer_token() {
            request = request.bearer_auth(token.expose_secret());
        }
        let response = request
            .send()
            .await
            .map_err(|e| catalog_unreachable(e.to_string()))?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        let bytes = response
            .bytes()
            .await
            .map_err(|e| catalog_unreachable(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(catalog_error_from_http(status, retry_after, &bytes));
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| CanonicalError::internal(format!("products catalog browse: {e}")).create())
    }
}

#[async_trait]
impl ProductCatalogClientV1 for ProductCatalogRestClient {
    async fn get_skus(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
    ) -> Result<Vec<CatalogSku>, CanonicalError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let filter = ids_filter(ids);
        let wanted: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
        let mut cursor = None;
        let mut items = Vec::new();
        loop {
            let page = self
                .browse(ctx, Some(&filter), BROWSE_MAX, cursor.as_deref())
                .await?;
            items.extend(
                map_rows(page.rows, ctx.subject_tenant_id())?
                    .into_iter()
                    .filter(|s| wanted.contains(&s.sku_id)),
            );
            cursor = page.page_info.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(items)
    }

    async fn search_skus(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        limit: u64,
        cursor: Option<&str>,
    ) -> Result<CatalogSkuPage, CanonicalError> {
        // The door serves 1 to 200 (PS-49: the contract's `limit` is `u64`).
        let limit = u32::try_from(limit.min(u64::from(BROWSE_MAX))).unwrap_or(BROWSE_MAX);
        let filter = search_filter(q);
        let page = self.browse(ctx, filter.as_deref(), limit, cursor).await?;
        Ok(CatalogSkuPage {
            items: map_rows(page.rows, ctx.subject_tenant_id())?,
            next_cursor: page.page_info.next_cursor,
        })
    }

    async fn list_tax_categories(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<CatalogTaxCategory>, CanonicalError> {
        #[derive(Deserialize)]
        struct TaxRow {
            code: String,
            display_name: String,
        }
        #[derive(Deserialize)]
        struct TaxPage {
            rows: Vec<TaxRow>,
        }
        let url = format!(
            "{}{BROWSE_PATH}?kind=tax_category",
            self.config.base_url.trim_end_matches('/')
        );
        let page: TaxPage = self.read_url(ctx, &url).await?;
        Ok(page
            .rows
            .into_iter()
            .map(|r| CatalogTaxCategory {
                code: r.code,
                display_name: r.display_name,
            })
            .collect())
    }
}

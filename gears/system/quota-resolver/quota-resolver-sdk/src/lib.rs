//! Quota Resolver SDK: the public and plugin contracts of the `quota_resolver`
//! gear, their data types, and the quota GTS types.

toolkit_gts::declare_gts_inventory!();

pub mod api;
pub mod gts;
pub mod models;
pub mod plugin_api;
pub mod rest;

pub use api::{
    PlatformQuotaResolverApi, QuotaResolverApi, platform_quota_resolver_api_ir,
    quota_resolver_api_ir,
};
pub use gts::{
    QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID, QuotaMetricV1,
    QuotaResolverPluginSpecV1, QuotaSubjectV1,
};
pub use plugin_api::QuotaResolverPluginClient;
pub use rest::{
    PlatformQuotaResolverApiRest, QuotaResolverApiRest,
    platform_quota_resolver_api_rest_http_binding, quota_resolver_api_rest_http_binding,
};
#[cfg(feature = "rest-client")]
pub use rest::{
    PlatformQuotaResolverApiRestClient, PlatformQuotaResolverApiRestResolvingClient,
    QuotaResolverApiRestClient, QuotaResolverApiRestResolvingClient,
};

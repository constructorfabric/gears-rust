//! SKU registry SDK, the usage-type catalog port, the SKU usage port pricing fills, the derived
//! usage declaration with its one evaluator (P-D-230), and the commercial quota contract
//! (P-D-267, P-D-272).
#![forbid(unsafe_code)]
pub mod api;
pub mod derived;
pub mod models;
pub mod quota;
pub mod sku_usage;
pub mod usage_types;

pub use api::ProductsClient;
pub use models::{
    BillingTiming, Category, Lifecycle, Sku, SkuChangedPayload, SkuContent, SkuType, SkuVersion,
};
pub use quota::catalog::ProductsQuotaCatalogClientV1;
pub use quota::discovery::ProductsQuotaDiscoveryPluginClientV1;

pub mod references;
pub use models::{ReferenceKind, ReferenceState, ReservationReceipt};
pub use references::{
    PRICING_SYSTEM_ACTOR, PRICING_SYSTEM_SUBJECT_TYPE, PricingReferenceRegistry,
    ReferenceRegistryV1, is_pricing_system_actor,
};

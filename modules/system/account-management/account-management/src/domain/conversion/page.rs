//! Offset-pagination envelope used by the conversion listing surfaces.
//!
//! Decoupled from `account_management_sdk` because the conversion
//! flows are not yet hoisted into the SDK (see the doc-comment on
//! `account_management_sdk::AccountManagementClient` — the conversion
//! DTOs `ConversionRequest` / `ListConversionsQuery` / etc. still live
//! impl-side). The legacy `TenantPage<T>` SDK type was retired when
//! `list_children` migrated to cursor pagination
//! (`modkit_odata::Page<T>`); `OffsetPage<T>` preserves the offset +
//! `total` semantics conversion's `list_*` endpoints expect until the
//! conversion surface is itself hoisted.
//!
//! `total` is best-effort — the underlying `list` + `count` repo
//! calls run as two independent statements (READ COMMITTED on
//! Postgres, autocommit on `SQLite`) so a row committed between them
//! can let `items.len()` differ from `total` by one. Consumers
//! deriving `has_more` from `(total - skip) > top` should treat the
//! number as advisory rather than authoritative.

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};

#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OffsetPage<T> {
    pub items: Vec<T>,
    pub top: u32,
    pub skip: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

impl<T> OffsetPage<T> {
    #[must_use]
    pub const fn new(items: Vec<T>, top: u32, skip: u32, total: Option<u64>) -> Self {
        Self {
            items,
            top,
            skip,
            total,
        }
    }
}

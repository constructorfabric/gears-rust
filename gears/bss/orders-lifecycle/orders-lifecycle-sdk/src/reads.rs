//! Read contracts (08 §3.3; S1-02 catalog `get`, `list`, `list_lines`; `models.json`
//! `ListOrders`, `OrderPage`, `LineList`, `LinePage`). Early S6-01 draft subset with S6-04
//! access logging.
//!
//! Every collection is paged (08 §2.2): `page_size` defaults to 50 and is bounded 1–200; the
//! continuation `cursor` is an opaque versioned token that is position only, never authority
//! (D-139). Filters bind the existing design filters under the S1-02 query names; clients cannot
//! select a sort. A read carries no expected version or idempotency key; the only call metadata
//! is the supplied delegation proof reference, forwarded unvalidated to the PDP and recorded as
//! *supplied* on the access log (08 §4.4, D-202).
use crate::authoring::{DraftLine, OrderHeader};
use crate::catalog::OrderState;
use crate::models::{DelegationProofRef, DraftRevision, InvalidBoundaryValue, OrderVersion};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Default page size (08 §4.5).
pub const PAGE_DEFAULT: u16 = 50;
/// Smallest accepted page size.
pub const PAGE_MIN: u16 = 1;
/// Largest accepted page size; a larger request is refused `page-size-exceeded`, never truncated.
pub const PAGE_MAX: u16 = 200;
/// Largest accepted cursor token length in bytes (structural bound before decoding).
pub const CURSOR_MAX_LEN: usize = 4_096;

/// A validated page size in `1..=200`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct PageSize(u16);
impl PageSize {
    /// The design default of 50.
    pub const DEFAULT: Self = Self(PAGE_DEFAULT);
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}
impl Default for PageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl TryFrom<u64> for PageSize {
    type Error = InvalidBoundaryValue;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let value = u16::try_from(value).map_err(|_| InvalidBoundaryValue)?;
        if (PAGE_MIN..=PAGE_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidBoundaryValue)
        }
    }
}
impl From<PageSize> for u64 {
    fn from(value: PageSize) -> Self {
        Self::from(value.0)
    }
}

/// An opaque continuation token. Structure, version, precision and request binding are
/// validated by the server before the access decision; any failure is `cursor-invalid`.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cursor(String);
impl Cursor {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cursor([opaque])")
    }
}
impl TryFrom<String> for Cursor {
    type Error = InvalidBoundaryValue;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > CURSOR_MAX_LEN
            || !value.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(InvalidBoundaryValue);
        }
        Ok(Self(value))
    }
}
impl From<Cursor> for String {
    fn from(value: Cursor) -> Self {
        value.0
    }
}

/// Trusted read call metadata: no version, no idempotency key.
#[derive(Debug, Clone, Default)]
pub struct ReadMeta {
    pub correlation_id: Option<Uuid>,
    /// Supplied delegation proof reference, forwarded to the PDP decision of the call and
    /// recorded as supplied on any access-log row (08 §4.4).
    pub delegation_proof_ref: Option<DelegationProofRef>,
}

/// Order-list filters under the S1-02 query names (CONTRACTS.md): `state` exact,
/// `created_from` inclusive, `created_to` exclusive, `state_entered_before` inclusive
/// (continuously in the current state since that instant or earlier), `contract_id` exact.
/// Timestamps are RFC 3339 instants; stored microseconds are compared exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<OrderState>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_from: Option<OffsetDateTime>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_to: Option<OffsetDateTime>,
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub state_entered_before: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<Uuid>,
}

/// The order-list request (`models.json` `ListOrders`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListOrders {
    pub filters: OrderFilters,
    /// Absent means the default of 50.
    pub page_size: Option<PageSize>,
    pub cursor: Option<Cursor>,
}

/// The per-order line-list request (`models.json` `LineList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineList {
    pub page_size: Option<PageSize>,
    pub cursor: Option<Cursor>,
}

/// One order-list element: the aggregate header at its current commercial version, with the
/// draft revision while the order is a draft. The composed detail is the point read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderSummary {
    pub order: OrderHeader,
    pub current_version: OrderVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<DraftRevision>,
}

/// One authorized keyset page of orders in `(created_at, order_id)` order (`models.json`
/// `OrderPage`). `next_cursor` is present only when another authorized row followed the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderPage {
    pub orders: Vec<OrderSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
}

/// One authorized keyset page of an order's current lines in `(created_at, line_id)` order of
/// the line identity (`models.json` `LinePage`). While the order is a draft the members are the
/// working set as authored; `current_version` and `draft_revision` come from the same
/// consistent snapshot as the lines, and the response `ETag` carries `current_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinePage {
    pub lines: Vec<DraftLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
    pub current_version: OrderVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<DraftRevision>,
}

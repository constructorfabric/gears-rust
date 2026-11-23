//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! Reading one complete capability snapshot of a target (P-D-268), outside any transaction: every
//! page of one immutable snapshot, verified whole, or no answer at all. An incomplete read is
//! never taken as capabilities removed.
//!
//! The adapter's own error text is logged under `bss_products.quota.discovery` and never answered:
//! it may carry anything the adapter saw. No log line carries a cursor or a record.
use std::collections::HashSet;
use std::time::Duration;

use bss_products_sdk::ProductsQuotaDiscoveryPluginClientV1;
use bss_products_sdk::quota::digest::snapshot_digest;
use bss_products_sdk::quota::discovery::{QuotaDiscoveryPage, QuotaDiscoveryRequest};
use serde_json::Value;
use time::{OffsetDateTime, UtcOffset};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The log target of every discovery failure the intake meets.
const LOG_TARGET: &str = "bss_products.quota.discovery";
/// The most digits a canonical revision has.
const MAX_REVISION_DIGITS: usize = 39;
/// The most bytes one discovered record may take, serialized as JSON (P-D-268): checked as each
/// page arrives, so no adapter makes the intake buffer, hash or store a record of unbounded size.
pub const MAX_RECORD_BYTES: usize = 64 * 1024;

/// How discovery reads a target: the page size asked for, the time one page may take, the most
/// records one snapshot may hold, and the time the whole read may take (P-D-268).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaSettings {
    pub page_size: u32,
    pub timeout: Duration,
    pub max_items: u32,
    pub deadline: Duration,
}

/// One complete snapshot whose pages, count and digest agree.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSnapshot {
    pub snapshot_id: Uuid,
    /// A canonical decimal of at most 39 digits.
    pub source_revision: String,
    /// The adapter's observation time, in UTC.
    pub observed_at: OffsetDateTime,
    pub item_count: u64,
    /// The adapter's digest, verified against the records.
    pub content_digest: String,
    /// Sorted by `id`, as the digest sorts them.
    pub items: Vec<Value>,
}

/// Why a snapshot could not be read whole.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// The pages do not make one complete snapshot (503 `QUOTA_DISCOVERY_INCOMPLETE`).
    Incomplete(String),
    /// The adapter failed or did not answer in time (503 `QUOTA_DISCOVERY_UNAVAILABLE`).
    Unavailable(String),
}

/// Every page of the target's current snapshot, verified whole (P-D-268).
///
/// # Errors
/// `Incomplete` (503 `QUOTA_DISCOVERY_INCOMPLETE`) for mixed pages, a duplicate id, a broken cursor
/// chain, an empty page that is not the last, a count or digest mismatch, a non-canonical revision,
/// more than `max_items` records, a record over [`MAX_RECORD_BYTES`] or holding U+0000, or pages
/// that together outlast `deadline`;
/// `Unavailable` (503 `QUOTA_DISCOVERY_UNAVAILABLE`) for an adapter error or a page later than
/// `timeout`.
// @cpt-algo:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1
pub async fn read_snapshot(
    client: &dyn ProductsQuotaDiscoveryPluginClientV1,
    ctx: &SecurityContext,
    target_key: &str,
    settings: QuotaSettings,
) -> Result<VerifiedSnapshot, SnapshotError> {
    // One discovery has an overall deadline: every page may answer within its own timeout and
    // the whole read still be cut, so an adapter that answers slowly cannot hold the intake.
    let pages = tokio::time::timeout(
        settings.deadline,
        read_pages(client, ctx, target_key, settings),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            target: LOG_TARGET,
            target_key,
            deadline_ms = settings.deadline.as_millis(),
            "bss-products: a quota discovery outlasted its deadline"
        );
        incomplete("the snapshot was not read whole within the discovery deadline")
    })??;
    verify(pages)
}

fn incomplete(why: &str) -> SnapshotError {
    SnapshotError::Incomplete(why.to_owned())
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-pages
/// Every page, following `next_cursor` until it is absent: each call bounded by the timeout, no
/// cursor twice, no more records than the item bound, no record over [`MAX_RECORD_BYTES`] as JSON,
/// and no U+0000 in any string or key of a record (Postgres refuses it in the stored records,
/// `SQLite` stores it), each checked as its page arrives. A page may hold fewer records than it
/// asked for, so only the record bound limits a read; a page that is not the last holds at least
/// one record, so an adapter that never ends is cut off by that bound too.
async fn read_pages(
    client: &dyn ProductsQuotaDiscoveryPluginClientV1,
    ctx: &SecurityContext,
    target_key: &str,
    settings: QuotaSettings,
) -> Result<Vec<QuotaDiscoveryPage>, SnapshotError> {
    let page_size = settings.page_size.max(1);
    let max_items = u64::from(settings.max_items);
    let too_large = || incomplete("the snapshot exceeds the configured bound");
    let mut pages = Vec::new();
    let mut read: u64 = 0;
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let request = QuotaDiscoveryRequest {
            target_key: target_key.to_owned(),
            cursor: cursor.clone(),
            page_size,
        };
        let page =
            match tokio::time::timeout(settings.timeout, client.list_capabilities(ctx, request))
                .await
            {
                Ok(Ok(page)) => page,
                Ok(Err(e)) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        target_key,
                        error = %e,
                        "bss-products: a quota discovery page failed"
                    );
                    return Err(SnapshotError::Unavailable(
                        "the discovery adapter failed".to_owned(),
                    ));
                }
                Err(_) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        target_key,
                        timeout_ms = settings.timeout.as_millis(),
                        "bss-products: a quota discovery page did not answer in time"
                    );
                    return Err(SnapshotError::Unavailable(
                        "the discovery adapter did not answer in time".to_owned(),
                    ));
                }
            };
        read = read.saturating_add(u64::try_from(page.items.len()).unwrap_or(u64::MAX));
        if page.item_count > max_items || read > max_items {
            return Err(too_large());
        }
        if !page.items.iter().all(|item| fits(item, MAX_RECORD_BYTES)) {
            return Err(incomplete("a record exceeds the configured size"));
        }
        if page.items.iter().any(holds_nul) {
            return Err(incomplete("a record holds a NUL character (U+0000)"));
        }
        cursor.clone_from(&page.next_cursor);
        let empty = page.items.is_empty();
        pages.push(page);
        match &cursor {
            None => return Ok(pages),
            Some(_) if empty => {
                return Err(incomplete(
                    "an empty page is not the last: every page but the last holds a record",
                ));
            }
            Some(next) if !cursors.insert(next.clone()) => {
                return Err(incomplete("a cursor repeats: the pages loop"));
            }
            Some(_) => {}
        }
    }
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-pages

/// Whether any string, value or key, of `item` holds U+0000; the walk keeps its own stack.
fn holds_nul(item: &Value) -> bool {
    let mut left = vec![item];
    while let Some(value) = left.pop() {
        match value {
            Value::String(text) if text.contains('\0') => return true,
            Value::Array(items) => left.extend(items),
            Value::Object(fields) => {
                if fields.keys().any(|key| key.contains('\0')) {
                    return true;
                }
                left.extend(fields.values());
            }
            _ => {}
        }
    }
    false
}

/// Whether `item` serializes as JSON in at most `limit` bytes: the count stops at the limit, so a
/// large record is never rendered whole to be measured.
fn fits(item: &Value, limit: usize) -> bool {
    /// A sink that takes at most `left` bytes and refuses the rest.
    struct Bounded {
        left: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.left = self
                .left
                .checked_sub(buf.len())
                .ok_or_else(|| std::io::Error::other("over the bound"))?;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bounded { left: limit }, item).is_ok()
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-verify
/// The pages make one snapshot: the same identity on every page, a canonical revision, a UUID id
/// on every record and none twice, as many records as the count says, and the digest they hash
/// to.
fn verify(pages: Vec<QuotaDiscoveryPage>) -> Result<VerifiedSnapshot, SnapshotError> {
    let Some(first) = pages.first() else {
        return Err(incomplete("the adapter answered no page"));
    };
    let identity = |page: &QuotaDiscoveryPage| {
        (
            page.snapshot_id,
            page.source_revision.clone(),
            page.observed_at,
            page.item_count,
            page.content_digest.clone(),
        )
    };
    let fixed = identity(first);
    if pages.iter().any(|page| identity(page) != fixed) {
        return Err(incomplete("the pages name more than one snapshot"));
    }
    let (snapshot_id, source_revision, observed_at, item_count, content_digest) = fixed;
    if !is_canonical_revision(&source_revision) {
        return Err(incomplete(
            "the source revision is not a canonical decimal of at most 39 digits",
        ));
    }
    let observed_at = observed_at
        .checked_to_offset(UtcOffset::UTC)
        .ok_or_else(|| incomplete("the observation time has no UTC instant"))?;
    let mut ids = HashSet::new();
    let mut items = Vec::new();
    for item in pages.into_iter().flat_map(|page| page.items) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .and_then(|text| Uuid::parse_str(text).ok())
            .ok_or_else(|| incomplete("a record has no UUID id"))?;
        if !ids.insert(id) {
            return Err(incomplete("a record id repeats"));
        }
        items.push(item);
    }
    if u64::try_from(items.len()).ok() != Some(item_count) {
        return Err(incomplete("the count does not match the records read"));
    }
    let digest = snapshot_digest(
        snapshot_id,
        &source_revision,
        observed_at,
        item_count,
        &items,
    )
    .map_err(|_| incomplete("the records do not hash"))?;
    if digest != content_digest {
        return Err(incomplete("the content digest does not match the records"));
    }
    // As the digest sorts them: by the id's own text.
    items.sort_by(|left, right| id_text(left).cmp(&id_text(right)));
    Ok(VerifiedSnapshot {
        snapshot_id,
        source_revision,
        observed_at,
        item_count,
        content_digest,
        items,
    })
}

fn id_text(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

/// `0`, or 1 to 39 digits without a leading zero.
fn is_canonical_revision(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_REVISION_DIGITS
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-verify

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;

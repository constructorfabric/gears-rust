//! The usage-type collector as the publish door sees it
//! (`dod-usage-type-resolution`; P-D-184, P-D-203).
//!
//! One question, four answers ([`UsageTypeAnswer`]), asked **once per
//! publish** for the SKU's one `usage_type_ref`, **before** the publish
//! transaction opens — so a `503` or a `403` leaves no claimed idempotency key
//! and the retry is a fresh act. The judge is the domain's (`domain::sku`);
//! this module is the seam that fetches the answer.
//!
//! # Why a trait on `ApiState`, and why not a `cfg(test)` fork
//!
//! The first cut of this seam was a function that answered `Resolved` in the
//! test binary and `Unavailable` in production. That is two programs: every
//! probe exercised a path production never ran, and the production path — a
//! constant refusal — was exercised by nothing. So the door reads the catalog
//! as a trait object off `ApiState`, `gear.rs` installs the resolved one
//! (P-D-184), tests inject a stub per outcome, and no `cfg(test)` sits in the
//! path.
//!
//! # The four answers, and how the collector's errors become them
//!
//! - `Resolved` — the collector returned the type.
//! - `Unresolved` — the collector answered `NotFound`, **or the ref is not a
//!   valid GTS id** (an id that cannot name anything cannot resolve anywhere;
//!   asking the collector would only rephrase the same `400`).
//! - `Forbidden` — the collector answered `PermissionDenied`: the catalog is
//!   read **as the caller** (P-D-207, owner option b), so a caller without collector
//!   read is told so with a 403 `USAGE_TYPE_FORBIDDEN`, never the 503 of an
//!   outage it could retry forever.
//! - `Unavailable` — every other error, and a call that outlives
//!   `usage_type_resolver_timeout_ms`: fail-closed, the gear's `503` channel,
//!   for usage SKUs only (P-D-184, P-D-203 — a latency coupling, not a lock).
//!
//! # `q` is products' own search over the collector
//!
//! The collector's storage plugin translates comparison operators only, so a
//! `contains(gts_id,…)` is refused there (a deployed picker answered 503
//! `internal error: unsupported operator: Contains`), whatever the collector SDK's field
//! doc says. The adapter asks the collector with `kind eq` at most: without
//! `q` it hands the collector's page and cursor through; with `q` it walks the
//! collector's pages up to [`USAGE_TYPE_SEARCH_CAP`] types, narrows them
//! itself and pages the matches with a cursor bound to `q` and `kind`
//! (P-D-207).
//!
//! [`UnconfiguredUsageTypes`] is what a deployment with no catalog at all gets:
//! `Unavailable` from `resolve`, always, and a **501** from `list` — never an
//! empty page, because "this deployment has no usage types" and "nobody could
//! be asked" are opposite facts. `gear.rs` says so once at boot. That keeps the
//! decided posture — a usage SKU cannot publish without a catalog — instead
//! of a `Resolved` nobody asked for.
//!
//! # The port itself lives in the SDK now
//!
//! [`UsageTypeCatalog`] is `bss_products_sdk::usage_types`'s, so a module that
//! is not this collector can register one and be preferred over the adapter
//! below. This module is the two implementations this crate ships plus the
//! fabricated one a deployment may opt into.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use usage_collector_sdk::{UsageCollectorClientV1, UsageCollectorError, UsageTypeGtsId};

use bss_products_sdk::usage_types::{
    UsageTypeAnswer, UsageTypeBinding, UsageTypeCatalog, UsageTypePage, invalid_usage_type_cursor,
    unconfigured_usage_type_catalog, usage_type_catalog_denied,
    usage_type_catalog_rejected_the_query, usage_type_catalog_unreachable,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{CursorV1, ODataQuery, SortDir, parse_filter_string};

/// No catalog is wired: `resolve` is `Unavailable`, fail-closed (P-D-184), and
/// `list` is a **501**. Installed by `gear.rs` when nothing answers, with a
/// boot-time warning naming this type.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredUsageTypes;

#[async_trait]
impl UsageTypeCatalog for UnconfiguredUsageTypes {
    async fn resolve(&self, _ctx: &SecurityContext, _usage_type_ref: &str) -> UsageTypeAnswer {
        UsageTypeAnswer::Unavailable
    }

    async fn list(
        &self,
        _ctx: &SecurityContext,
        _q: Option<&str>,
        _kind: Option<&str>,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        // **Not an empty page.** A caller that cannot tell "no types" from
        // "no catalog" will render silence as a clean answer, which is the
        // failure this whole surface exists to avoid.
        Err(unconfigured_usage_type_catalog())
    }
}

/// The adapter over the usage collector's own client, bounded by the
/// configured timeout.
pub struct CollectorUsageTypes {
    client: Arc<dyn UsageCollectorClientV1>,
    timeout: Duration,
}

impl CollectorUsageTypes {
    /// `timeout` is `ProductsConfig::usage_type_resolver_timeout()` — read,
    /// never inlined (P-D-203).
    #[must_use]
    pub fn new(client: Arc<dyn UsageCollectorClientV1>, timeout: Duration) -> Self {
        Self { client, timeout }
    }
}

#[async_trait]
impl UsageTypeCatalog for CollectorUsageTypes {
    async fn resolve(&self, ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer {
        let Ok(gts_id) = UsageTypeGtsId::new(usage_type_ref) else {
            return UsageTypeAnswer::Unresolved;
        };
        match tokio::time::timeout(self.timeout, self.client.get_usage_type(ctx, gts_id)).await {
            Ok(Ok(usage_type)) => UsageTypeAnswer::Resolved(binding_of(&usage_type)),
            Ok(Err(UsageCollectorError::NotFound { .. })) => UsageTypeAnswer::Unresolved,
            // The PDP's reason is for operator logs only, as `list` below keeps it.
            Ok(Err(error @ UsageCollectorError::PermissionDenied { .. })) => {
                tracing::warn!(%error, usage_type_ref, "bss-products: usage-type collector refused the caller");
                UsageTypeAnswer::Forbidden
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, usage_type_ref, "bss-products: usage-type collector failed");
                UsageTypeAnswer::Unavailable
            }
            Err(_elapsed) => {
                tracing::warn!(
                    usage_type_ref,
                    timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
                    "bss-products: usage-type collector timed out"
                );
                UsageTypeAnswer::Unavailable
            }
        }
    }

    async fn list(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        let kind = kind.filter(|s| !s.is_empty());
        let cursor = cursor.filter(|s| !s.is_empty());
        match q.filter(|s| !s.is_empty()) {
            None => self.page(ctx, kind, limit, cursor).await,
            Some(needle) => self.search(ctx, needle, kind, limit, cursor).await,
        }
    }
}

impl CollectorUsageTypes {
    /// Without `q`: one page of the collector's own, under its own cursor.
    async fn page(
        &self,
        ctx: &SecurityContext,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        let cursor = match cursor {
            None => None,
            Some(token) => {
                let decoded = CursorV1::decode(token).map_err(|_| invalid_usage_type_cursor())?;
                // **A search's cursor is not the collector's.** Products sends the collector no
                // filter hash, so every cursor the collector mints here carries none; one that
                // carries a hash is a search's, replayed without its `q`. Handed on, the
                // collector would refuse it as an internal error, a 503 for the caller's 400.
                if decoded.f.is_some() {
                    return Err(invalid_usage_type_cursor());
                }
                Some(decoded)
            }
        };
        let query = collector_query(kind, u64::from(limit), cursor)?;
        // The same deadline `resolve` runs under. A pick-list that hangs is a
        // screen that hangs, and the operator learns nothing either way.
        match tokio::time::timeout(self.timeout, self.client.list_usage_types(ctx, &query)).await {
            Ok(Ok(page)) => Ok(UsageTypePage {
                items: page.items.iter().map(binding_of).collect(),
                next_cursor: page.page_info.next_cursor,
                prev_cursor: page.page_info.prev_cursor,
                // The catalog's own, not the caller's ask: it may have a
                // ceiling this gear does not know.
                limit: u32::try_from(page.page_info.limit).unwrap_or(limit),
            }),
            Ok(Err(error)) => Err(collector_refusal(error)),
            Err(_elapsed) => Err(self.late()),
        }
    }

    /// With `q`: products narrows, because the collector cannot (P-D-207).
    ///
    /// **The collector is asked with `kind eq` at most.** Its storage plugin translates
    /// comparison operators only, so a `contains(gts_id,…)` is refused there as an internal
    /// error, whatever the collector SDK's field doc says `gts_id` supports. So the walk reads
    /// the collector's pages for `kind` up to [`USAGE_TYPE_SEARCH_CAP`] types, keeps the ones
    /// whose id holds `q` case-insensitively, orders them by id, and pages them with a cursor
    /// products mints: the last id served, and a hash of `q` and `kind`, so a cursor replayed
    /// with other values is 400. Past the cap it is 503 `USAGE_TYPE_CATALOG_TOO_LARGE`, never a
    /// page of the part it read. The whole walk runs under the one deadline a single call has.
    async fn search(
        &self,
        ctx: &SecurityContext,
        needle: &str,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        let binding = search_binding(needle, kind);
        let after = match cursor {
            None => None,
            Some(token) => Some(search_position(token, &binding)?),
        };
        let walked = match tokio::time::timeout(self.timeout, self.walk(ctx, kind)).await {
            Ok(walked) => walked?,
            Err(_elapsed) => return Err(self.late()),
        };
        let mut matches: Vec<UsageTypeBinding> = walked
            .into_iter()
            .filter(|b| holds_q(&b.gts_id, needle))
            .filter(|b| {
                after
                    .as_deref()
                    .is_none_or(|after| b.gts_id.as_str() > after)
            })
            .collect();
        matches.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        matches.dedup_by(|a, b| a.gts_id == b.gts_id);
        let page = usize::try_from(limit).unwrap_or(usize::MAX);
        let more = matches.len() > page;
        matches.truncate(page);
        let next_cursor = match (more, matches.last()) {
            (true, Some(last)) => Some(search_cursor(&last.gts_id, &binding)?),
            _ => None,
        };
        Ok(UsageTypePage {
            items: matches,
            next_cursor,
            prev_cursor: None,
            limit,
        })
    }

    /// The collector's catalog for `kind`, page by page, up to the cap.
    // cancel-safe: `search` drives this under `tokio::time::timeout`, and dropping it at any
    // `.await` loses only the local `walked` buffer; every collector call it makes is a read
    // (RS-44).
    async fn walk(
        &self,
        ctx: &SecurityContext,
        kind: Option<&str>,
    ) -> Result<Vec<UsageTypeBinding>, CanonicalError> {
        let mut walked: Vec<UsageTypeBinding> = Vec::new();
        let mut cursor: Option<CursorV1> = None;
        loop {
            // One past the cap is enough to know the cap was passed; the collector may page
            // fewer, and the walk follows its cursor either way.
            let ask = USAGE_TYPE_SEARCH_CAP + 1 - walked.len();
            let query = collector_query(kind, u64::try_from(ask).unwrap_or(u64::MAX), cursor)?;
            let page = self
                .client
                .list_usage_types(ctx, &query)
                .await
                .map_err(collector_refusal)?;
            let progressed = !page.items.is_empty();
            walked.extend(page.items.iter().map(binding_of));
            if walked.len() > USAGE_TYPE_SEARCH_CAP {
                tracing::warn!(
                    cap = USAGE_TYPE_SEARCH_CAP,
                    kind,
                    "bss-products: the usage-type catalog is larger than the picker searches"
                );
                return Err(usage_type_catalog_unreachable(format!(
                    "USAGE_TYPE_CATALOG_TOO_LARGE: the usage-type catalog holds more than \
                     {USAGE_TYPE_SEARCH_CAP} types{}, more than `q` searches; narrow by \
                     `kind` or page without `q`",
                    kind.map_or_else(String::new, |k| format!(" of kind {k}"))
                )));
            }
            match page.page_info.next_cursor {
                None => return Ok(walked),
                // A continuation after an empty page would walk forever.
                Some(_) if !progressed => {
                    return Err(usage_type_catalog_unreachable(
                        "the usage-type catalog answered an empty page with a continuation",
                    ));
                }
                Some(token) => {
                    cursor = Some(CursorV1::decode(&token).map_err(|_| {
                        usage_type_catalog_unreachable(
                            "the usage-type catalog minted a continuation that does not decode",
                        )
                    })?);
                }
            }
        }
    }

    fn late(&self) -> CanonicalError {
        usage_type_catalog_unreachable(format!(
            "the usage-type catalog did not answer within {}ms",
            u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)
        ))
    }
}

/// How many usage types the picker's `q` walks at most (P-D-207): the collector's catalog for
/// the asked `kind`, read page by page. Past it the search is 503
/// `USAGE_TYPE_CATALOG_TOO_LARGE`; a page without `q` is the collector's and has no cap.
pub const USAGE_TYPE_SEARCH_CAP: usize = 1000;

/// A collector refusal as the port's error.
///
/// **Never an empty page on a failure**, and never one class for every failure. The 503 is what
/// lets a caller tell "this deployment has no usage types" from "the catalog did not answer" -
/// but a denial and a malformed filter are neither, and reporting them as an outage sends an
/// operator to retry forever against a permission problem.
///
/// **The PDP detail never reaches the wire.** The collector SDK's own `PermissionDenied` doc
/// says it is "kept for operator logs; the host lift drops it from the public wire body", so it
/// is logged here and the caller is told only that authorization was refused.
fn collector_refusal(error: UsageCollectorError) -> CanonicalError {
    tracing::warn!(%error, "bss-products: usage-type catalog list failed");
    match error {
        UsageCollectorError::PermissionDenied { .. } => usage_type_catalog_denied(),
        UsageCollectorError::InvalidArgument { .. } => usage_type_catalog_rejected_the_query(),
        other => usage_type_catalog_unreachable(other.to_string()),
    }
}

/// Does `gts_id` hold `needle`, case folded? One rule for every catalog this crate ships.
fn holds_q(gts_id: &str, needle: &str) -> bool {
    gts_id.to_lowercase().contains(&needle.to_lowercase())
}

/// The order a search's cursor names: `gts_id` ascending, as the matches are served.
const SEARCH_ORDER: &str = "+gts_id";

/// A search's binding: the first 8 bytes of the SHA-256 of `q` and `kind`, as hex.
fn search_binding(needle: &str, kind: Option<&str>) -> String {
    use crate::domain::canonical::{canonical_rendering, content_digest};
    content_digest(&canonical_rendering(&serde_json::json!({
        "usage_types": { "q": needle, "kind": kind },
    })))
    .iter()
    .take(8)
    .fold(String::with_capacity(16), |mut hex, b| {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        hex.push(char::from(DIGITS[usize::from(b >> 4)]));
        hex.push(char::from(DIGITS[usize::from(b & 0x0f)]));
        hex
    })
}

/// A search's continuation: the last id served, bound to its `q` and `kind`.
fn search_cursor(last: &str, binding: &str) -> Result<String, CanonicalError> {
    CursorV1 {
        k: vec![last.to_owned()],
        o: SortDir::Asc,
        s: SEARCH_ORDER.to_owned(),
        f: Some(binding.to_owned()),
        d: "fwd".to_owned(),
    }
    .encode()
    .map_err(|e| {
        CanonicalError::internal(format!("bss-products: a search cursor did not encode: {e}"))
            .create()
    })
}

/// The id a search's cursor continues after, when the cursor is this search's.
fn search_position(token: &str, binding: &str) -> Result<String, CanonicalError> {
    let cursor = CursorV1::decode(token).map_err(|_| invalid_usage_type_cursor())?;
    if cursor.f.as_deref() != Some(binding)
        || cursor.s != SEARCH_ORDER
        || cursor.o != SortDir::Asc
        || cursor.d != "fwd"
    {
        return Err(invalid_usage_type_cursor());
    }
    match <[String; 1]>::try_from(cursor.k) {
        Ok([after]) => Ok(after),
        Err(_) => Err(invalid_usage_type_cursor()),
    }
}

/// One collector [`UsageType`](usage_collector_sdk::models::UsageType) as the
/// port's binding.
///
/// **One spelling for both methods.** `resolve` used to build this inline; a
/// second copy for `list` is how a picker comes to disagree with the gate about
/// what a type's `kind` is.
fn binding_of(usage_type: &usage_collector_sdk::models::UsageType) -> UsageTypeBinding {
    UsageTypeBinding {
        gts_id: usage_type.gts_id.to_string(),
        kind: match usage_type.kind {
            usage_collector_sdk::models::UsageKind::Counter => "counter",
            usage_collector_sdk::models::UsageKind::Gauge => "gauge",
        }
        .to_owned(),
        metadata_fields: usage_type
            .metadata_fields
            .iter()
            .map(|key| key.as_str().to_owned())
            .collect(),
    }
}

/// The collector's question: `kind eq '…'` at most, a page size and the collector's own cursor.
///
/// **Equality only.** The collector's storage plugin translates comparison operators and
/// nothing else, so `q` is never sent (see [`CollectorUsageTypes::search`]). `kind` is equality
/// over the collector's own closed two-value set.
///
/// # Errors
///
/// [`CanonicalError`] when the filter will not parse.
fn collector_query(
    kind: Option<&str>,
    limit: u64,
    cursor: Option<CursorV1>,
) -> Result<ODataQuery, CanonicalError> {
    let mut odata = ODataQuery::new().with_limit(limit);
    if let Some(wanted) = kind {
        // **A caller's filter is a 400, not a 500.** The donor
        // `catalog_provider::search_odata` raises `internal` because its
        // cursor is one this gear minted for an in-process client; here the
        // operand arrives on a public query string, and paging an on-call for
        // somebody's typo is the wrong answer.
        // The operand is the closed set's own token, never the caller's text (RS-39).
        let token = match wanted.parse::<usage_collector_sdk::UsageKind>() {
            Ok(usage_collector_sdk::UsageKind::Counter) => "counter",
            Ok(usage_collector_sdk::UsageKind::Gauge) => "gauge",
            Err(_) => return Err(usage_type_catalog_rejected_the_query()),
        };
        let parsed = parse_filter_string(&format!("kind eq '{token}'"))
            .map_err(|_| usage_type_catalog_rejected_the_query())?;
        odata = odata.with_filter(parsed.into_expr());
    }
    if let Some(cursor) = cursor {
        odata = odata.with_cursor(cursor);
    }
    Ok(odata)
}

/// A catalog another module registered, under the resolver timeout (RS-42).
///
/// [`CollectorUsageTypes`] bounds its own calls with `usage_type_resolver_timeout_ms`; a registered
/// catalog wins over it (P-D-184) and is bounded here by the same setting, so a catalog that hangs
/// never hangs a submit, an approve or the pick-list. A resolve that outlives the bound is
/// `Unavailable` (the SDK's doc assumes the caller has a deadline), and a list a 503.
pub struct TimedUsageTypes {
    inner: Arc<dyn UsageTypeCatalog>,
    timeout: std::time::Duration,
}

impl TimedUsageTypes {
    /// Bound `inner`'s calls by `timeout`.
    #[must_use]
    pub fn new(inner: Arc<dyn UsageTypeCatalog>, timeout: std::time::Duration) -> Self {
        Self { inner, timeout }
    }
}

#[async_trait]
impl UsageTypeCatalog for TimedUsageTypes {
    async fn resolve(&self, ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer {
        tokio::time::timeout(self.timeout, self.inner.resolve(ctx, usage_type_ref))
            .await
            .unwrap_or_else(|_| {
                tracing::warn!(
                    timeout_ms = self.timeout.as_millis(),
                    "bss-products: the registered usage-type catalog did not resolve in time"
                );
                UsageTypeAnswer::Unavailable
            })
    }

    async fn list(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        tokio::time::timeout(self.timeout, self.inner.list(ctx, q, kind, limit, cursor))
            .await
            .unwrap_or_else(|_| {
                tracing::warn!(
                    timeout_ms = self.timeout.as_millis(),
                    "bss-products: the registered usage-type catalog did not list in time"
                );
                Err(usage_type_catalog_unreachable(
                    "the usage-type catalog did not answer in time",
                ))
            })
    }
}

/// A **fabricated** usage-type catalog, for a deployment with no supplier at all.
///
/// Selected only by an explicit config mode named at length, and warned about
/// at boot: a deployment running this is showing operators usage types no
/// collector issued, and a meter declared against one names a stream nothing
/// will ever report. Every id sits under a reserved prefix so the rows can be
/// found and swept when a real supplier arrives.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalDevStaticUsageTypes;

/// The reserved namespace every fabricated id sits under.
// A prefix, not an id: each fabricated id appends its leaf to it, so it ends on the dot.
#[allow(
    unknown_lints,
    de0901_gts_string_pattern,
    de0904_no_hardcoded_gts_prefix
)]
pub const DEV_LOCAL_USAGE_TYPE_PREFIX: &str =
    "gts.cf.core.uc.usage_record.v1~cf.dev.local.usage_type.";

/// The fabricated set, built once.
///
/// `resolve` is called per SKU on the publish gate, so rebuilding three
/// `String`s on every call was three allocations for a constant.
static FABRICATED: std::sync::LazyLock<Vec<UsageTypeBinding>> = std::sync::LazyLock::new(|| {
    ["cpu.v1", "storage.v1", "requests.v1"]
        .into_iter()
        .map(|leaf| UsageTypeBinding {
            gts_id: format!("{DEV_LOCAL_USAGE_TYPE_PREFIX}{leaf}"),
            kind: "counter".to_owned(),
            metadata_fields: Vec::new(),
        })
        .collect()
});

#[async_trait]
impl UsageTypeCatalog for LocalDevStaticUsageTypes {
    async fn resolve(&self, _ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer {
        FABRICATED
            .iter()
            .find(|binding| binding.gts_id == usage_type_ref)
            .cloned()
            .map_or(UsageTypeAnswer::Unresolved, UsageTypeAnswer::Resolved)
    }

    async fn list(
        &self,
        _ctx: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        // **A cursor is refused, not ignored.** The whole fabricated set fits
        // one page, so this type never mints one - but only the minting half
        // is under its control, and the port's own `list` doc says an
        // implementation "must not answer a page it did not narrow as though
        // it had". Handing page one back to a caller that asked to continue is
        // exactly that.
        if cursor.is_some_and(|token| !token.is_empty()) {
            return Err(invalid_usage_type_cursor());
        }
        // Narrowing is applied so a screen driving this mode behaves as it
        // will against a real catalog.
        let items = FABRICATED
            .iter()
            .filter(|b| {
                q.filter(|s| !s.is_empty())
                    .is_none_or(|n| holds_q(&b.gts_id, n))
            })
            .filter(|b| kind.filter(|s| !s.is_empty()).is_none_or(|k| b.kind == k))
            .take(limit as usize)
            .cloned()
            .collect();
        Ok(UsageTypePage {
            items,
            next_cursor: None,
            prev_cursor: None,
            limit,
        })
    }
}

#[cfg(test)]
#[path = "usage_types_tests.rs"]
mod usage_types_tests;

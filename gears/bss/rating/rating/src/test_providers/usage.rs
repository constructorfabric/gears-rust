//! Feature 04's doubles (T-D-84): the usage feed, the meter declaration read, the tenant hierarchy
//! and the `rating.child_work` depth, plus builders of v2 feed entries.
//!
//! `FakeUsageFeed` serves Rating's [`UsageFeedPort`] the way the collector's v2 contract describes
//! the feed: one log in feed order filtered by the subscription's meters, a cursor that continues
//! after a delivered page and binds its subscription, `next` on every live page, and
//! `CursorBeyondRetention` once retention removed an entry after the cursor. Entry ids are derived
//! as the collector derives them (usage-collector ADR-0007, `v2/id.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::domain::usage::declarations::{Fold, MeterDeclaration, MeterDeclarations};
use crate::domain::usage::entry::canonical_instant;
use crate::domain::usage::feed::{
    FeedCursor, FeedError, FeedPage, FeedStart, FeedSubscription, METER_BASE_TYPE, MeterId,
    UsageFeedPort,
};
use crate::domain::usage::normalize::{EntryType, Quantity};
use crate::domain::usage::reader::{QueueDepth, TenantHierarchy};
use crate::domain::usage::source::TenantScope;

/// The collector's entry-id namespace (usage-collector `USAGE_RECORD_ID_NAMESPACE`).
pub const USAGE_RECORD_ID_NAMESPACE: Uuid =
    Uuid::from_u128(0x5631_3026_863b_4de8_b32b_1f96_b673_06ed);

/// A meter id `gts.cf.core.uc.usage_record.v2~cf.test.<name>.v1~`.
///
/// # Panics
///
/// If `name` makes no valid meter id.
#[expect(clippy::expect_used, reason = "a test builder of known-good ids")]
#[must_use]
pub fn meter(name: &str) -> MeterId {
    MeterId::parse(format!("{METER_BASE_TYPE}cf.test.{name}.v1~")).expect("a valid meter id")
}

/// The id the collector derives for an entry: `UUIDv5` over the `0x1F`-joined dedup identity,
/// bounds in `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// # Panics
///
/// If a bound is not an RFC 3339 instant.
#[expect(clippy::expect_used, reason = "a test builder of known-good instants")]
#[must_use]
pub fn derive_entry_id(
    tenant_id: Uuid,
    meter: &MeterId,
    idempotency_key: &str,
    window_start: &str,
    window_end: &str,
    entry_type: EntryType,
) -> Uuid {
    let start = canonical_instant(window_start).expect("an RFC 3339 window_start");
    let end = canonical_instant(window_end).expect("an RFC 3339 window_end");
    let mut input = Vec::new();
    for (index, part) in [
        tenant_id.to_string().as_str(),
        meter.as_str(),
        idempotency_key,
        start.as_str(),
        end.as_str(),
        entry_type.as_str(),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            input.push(0x1F);
        }
        input.extend_from_slice(part.as_bytes());
    }
    Uuid::new_v5(&USAGE_RECORD_ID_NAMESPACE, &input)
}

/// What a test record says; [`record_entry`] fills in the rest.
#[derive(Debug, Clone)]
pub struct RecordSpec {
    /// `tenant_id`.
    pub tenant_id: Uuid,
    /// The meter.
    pub meter: MeterId,
    /// `idempotency_key`.
    pub key: String,
    /// `window_start`.
    pub window_start: String,
    /// `window_end`.
    pub window_end: String,
    /// `quantity`.
    pub quantity: String,
    /// `metadata`, omitted when empty (as the SDK serializes it).
    pub metadata: BTreeMap<String, String>,
    /// `accepted_at`.
    pub accepted_at: String,
}

impl RecordSpec {
    /// An hourly record of quantity `1` for `tenant_id` on `meter`, keyed `key`, covering the hour
    /// starting at `window_start` (`2026-10-08T10:00:00Z`); [`Self::quantity`] sets another.
    ///
    /// # Panics
    ///
    /// If `window_start` is not an RFC 3339 instant (a key passed in its place, say).
    #[expect(clippy::expect_used, reason = "a test builder of known-good instants")]
    #[must_use]
    pub fn hourly(tenant_id: Uuid, meter: &MeterId, key: &str, window_start: &str) -> Self {
        canonical_instant(window_start).expect("an RFC 3339 window_start");
        let window_end = shift_hour(window_start);
        Self {
            tenant_id,
            meter: meter.clone(),
            key: key.to_owned(),
            window_start: window_start.to_owned(),
            window_end: window_end.clone(),
            quantity: "1".to_owned(),
            metadata: BTreeMap::new(),
            accepted_at: window_end,
        }
    }

    /// The record with `quantity`.
    #[must_use]
    pub fn quantity(mut self, quantity: &Quantity) -> Self {
        quantity.as_str().clone_into(&mut self.quantity);
        self
    }
}

/// A quantity in the collector's pattern.
///
/// # Panics
///
/// If `text` is not one.
#[expect(
    clippy::expect_used,
    reason = "a test builder of known-good quantities"
)]
#[must_use]
pub fn qty(text: &str) -> Quantity {
    Quantity::parse(text).expect("a v2 quantity")
}

/// `start` plus one hour, for `YYYY-MM-DDTHH:00:00Z` within a day.
fn shift_hour(start: &str) -> String {
    let hour: u32 = start.get(11..13).and_then(|h| h.parse().ok()).unwrap_or(0);
    format!("{}{:02}{}", &start[..11], (hour + 1) % 24, &start[13..])
}

/// A v2 record entry, as the SDK serializes `UsageRecord` (no `entry_type`).
#[must_use]
pub fn record_entry(spec: &RecordSpec) -> Value {
    let id = derive_entry_id(
        spec.tenant_id,
        &spec.meter,
        &spec.key,
        &spec.window_start,
        &spec.window_end,
        EntryType::Record,
    );
    let mut entry = json!({
        "id": id,
        "gts_type_id": spec.meter.as_str(),
        "tenant_id": spec.tenant_id,
        "resource_ref": {"resource_id": "ct-7f3a", "resource_type": "container"},
        "quantity": spec.quantity,
        "idempotency_key": spec.key,
        "accepted_at": spec.accepted_at,
        "origin": "live",
        "window_start": spec.window_start,
        "window_end": spec.window_end,
    });
    if !spec.metadata.is_empty()
        && let Some(object) = entry.as_object_mut()
    {
        object.insert(
            "metadata".to_owned(),
            Value::Object(
                spec.metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect::<Map<_, _>>(),
            ),
        );
    }
    entry
}

/// The invalidation of `record` (a record entry): a faithful copy with `reason_code`, whose id is
/// derived with `entry_type = invalidation` and whose `invalidates` is the record's id.
#[must_use]
pub fn invalidation_of(record: &Value, reason: &str) -> Value {
    let text = |field: &str| {
        record
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let tenant = Uuid::parse_str(&text("tenant_id")).unwrap_or_else(|_| Uuid::nil());
    let meter_id = MeterId::parse(text("gts_type_id")).unwrap_or_else(|_| meter("unknown"));
    let id = derive_entry_id(
        tenant,
        &meter_id,
        &text("idempotency_key"),
        &text("window_start"),
        &text("window_end"),
        EntryType::Invalidation,
    );
    let mut entry = record.clone();
    if let Some(object) = entry.as_object_mut() {
        object.insert("id".to_owned(), json!(id));
        object.insert(
            "invalidates".to_owned(),
            record.get("id").cloned().unwrap_or(Value::Null),
        );
        object.insert("reason_code".to_owned(), json!(reason));
    }
    entry
}

/// A `SUM` declaration in `unit` closing `metadata` to `keys`.
#[must_use]
pub fn sum_declaration(unit: &str, keys: &[&str]) -> MeterDeclaration {
    MeterDeclaration {
        fold: Fold::Sum,
        canonical_unit: unit.to_owned(),
        metadata_keys: keys
            .iter()
            .map(|key| (*key).to_owned())
            .collect::<BTreeSet<_>>(),
    }
}

#[derive(Default)]
struct FeedState {
    log: Vec<Value>,
    /// The log position of the oldest entry retention keeps.
    first_retained: usize,
    fail_next: Vec<FeedError>,
}

/// A programmable usage feed.
#[derive(Default)]
pub struct FakeUsageFeed {
    state: Mutex<FeedState>,
    reads: AtomicUsize,
    unavailable: AtomicBool,
    stalled: AtomicBool,
    unfiltered: AtomicBool,
    overrun: AtomicUsize,
}

impl FakeUsageFeed {
    /// An empty feed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends `entries` to the feed, in order.
    pub fn push(&self, entries: impl IntoIterator<Item = Value>) {
        self.state().log.extend(entries);
    }

    /// Retention removes every entry before log position `position`.
    pub fn expire_before(&self, position: usize) {
        let mut state = self.state();
        state.first_retained = position.min(state.log.len());
    }

    /// The next read fails with `error` (queued; one per read).
    pub fn fail_next(&self, error: FeedError) {
        self.state().fail_next.push(error);
    }

    /// Every read fails as unavailable while set.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Every read is accepted and never answered while set.
    pub fn set_stalled(&self, stalled: bool) {
        self.stalled.store(stalled, Ordering::SeqCst);
    }

    /// Every read serves the entries of every meter while set, as a collector that ignores the
    /// subscription would.
    pub fn set_unfiltered(&self, unfiltered: bool) {
        self.unfiltered.store(unfiltered, Ordering::SeqCst);
    }

    /// Every read serves up to `extra` entries more than the page limit, as a collector that
    /// ignores the limit would.
    pub fn set_overrun(&self, extra: usize) {
        self.overrun.store(extra, Ordering::SeqCst);
    }

    /// The reads served or refused so far.
    #[must_use]
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// The log position a cursor this feed issued for `subscription` names.
    fn position(cursor: &FeedCursor, subscription: &FeedSubscription) -> Result<usize, FeedError> {
        let (bound, position) = cursor
            .as_str()
            .rsplit_once('@')
            .ok_or_else(|| FeedError::Refused("INVALID_CURSOR".to_owned()))?;
        if bound != subscription_key(subscription) {
            return Err(FeedError::Refused("FILTER_MISMATCH".to_owned()));
        }
        position
            .parse()
            .map_err(|_| FeedError::Refused("INVALID_CURSOR".to_owned()))
    }
}

/// What a cursor binds: the subscription's meters.
fn subscription_key(subscription: &FeedSubscription) -> String {
    let joined: Vec<&str> = subscription.meters().iter().map(MeterId::as_str).collect();
    crate::domain::digest::Sha256Hex::of_bytes(joined.join("|").as_bytes())
        .as_str()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

/// Whether the subscription reads `entry`: its meter is subscribed, or it names none it could
/// read (a broken entry reaches every reader, as a broken collector would send it).
fn subscribed(entry: &Value, subscription: &FeedSubscription) -> bool {
    match entry.get("gts_type_id").and_then(Value::as_str) {
        Some(meter) => subscription.meters().iter().any(|m| m.as_str() == meter),
        None => true,
    }
}

#[async_trait]
impl UsageFeedPort for FakeUsageFeed {
    async fn read_usage_feed(
        &self,
        _ctx: &SecurityContext,
        subscription: &FeedSubscription,
        start: FeedStart<'_>,
        until: Option<&FeedCursor>,
        limit: u16,
    ) -> Result<FeedPage, FeedError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.stalled.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(FeedError::Unavailable(
                "the fake feed is down".to_owned(),
                None,
            ));
        }
        let mut state = self.state();
        if !state.fail_next.is_empty() {
            return Err(state.fail_next.remove(0));
        }
        let from = match start {
            FeedStart::Oldest => state.first_retained,
            FeedStart::After(cursor) => {
                let after = Self::position(cursor, subscription)?;
                // Retention removed entries after the cursor that the reader never read.
                if after < state.first_retained {
                    return Err(FeedError::CursorBeyondRetention);
                }
                after
            }
        };
        let end = match until {
            Some(cursor) => Self::position(cursor, subscription)?.min(state.log.len()),
            None => state.log.len(),
        };
        let unfiltered = self.unfiltered.load(Ordering::SeqCst);
        let page_size = usize::from(limit).saturating_add(self.overrun.load(Ordering::SeqCst));
        let mut entries = Vec::new();
        let mut position = from;
        while position < end && entries.len() < page_size {
            let entry = &state.log[position];
            if unfiltered || subscribed(entry, subscription) {
                entries.push(entry.clone());
            }
            position += 1;
        }
        let next = if until.is_some() && position >= end {
            None
        } else {
            FeedCursor::parse(format!("{}@{position}", subscription_key(subscription))).ok()
        };
        Ok(FeedPage { entries, next })
    }
}

/// A programmable meter declaration read.
#[derive(Default)]
pub struct FakeMeterDeclarations {
    declared: Mutex<BTreeMap<MeterId, MeterDeclaration>>,
    unavailable: AtomicBool,
    resolutions: AtomicUsize,
}

impl FakeMeterDeclarations {
    /// A registry declaring nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares (or re-declares) `meter`.
    pub fn declare(&self, meter: &MeterId, declaration: MeterDeclaration) {
        self.declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(meter.clone(), declaration);
    }

    /// Every resolution fails as an unreachable registry does while set.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// The resolutions answered or refused so far.
    #[must_use]
    pub fn resolutions(&self) -> usize {
        self.resolutions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MeterDeclarations for FakeMeterDeclarations {
    async fn resolve(&self, meter: &MeterId) -> Result<Option<MeterDeclaration>, DomainError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                detail: "the fake registry is down".to_owned(),
                cause: None,
            });
        }
        Ok(self
            .declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(meter)
            .cloned())
    }
}

/// A programmable tenant hierarchy: the scope's roots and the tenants placed under them.
#[derive(Default)]
pub struct FakeTenantHierarchy {
    placed: Mutex<BTreeMap<Uuid, Uuid>>,
    unavailable: AtomicBool,
}

impl FakeTenantHierarchy {
    /// A hierarchy of roots alone: every tenant not placed lies under no root.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Places `tenant` directly under `parent`.
    pub fn place(&self, tenant: Uuid, parent: Uuid) {
        self.placed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tenant, parent);
    }

    /// Makes every read fail as unavailable, or answer again.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }
}

#[async_trait]
impl TenantHierarchy for FakeTenantHierarchy {
    async fn within(&self, scope: &TenantScope, tenant: Uuid) -> Result<bool, DomainError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(DomainError::DependencyUnavailable {
                dependency: Dependency::TenantHierarchy,
                detail: "the fake tenant hierarchy is unavailable".to_owned(),
                cause: None,
            });
        }
        let placed = self.placed.lock().unwrap_or_else(PoisonError::into_inner);
        let mut current = Some(tenant);
        while let Some(at) = current {
            if scope.roots().contains(&at) {
                return Ok(true);
            }
            current = placed.get(&at).copied();
        }
        Ok(false)
    }
}

/// A settable `rating.child_work` depth.
#[derive(Default)]
pub struct FakeQueueDepth {
    depth: AtomicU64,
}

impl FakeQueueDepth {
    /// A depth of zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the depth.
    pub fn set(&self, depth: u64) {
        self.depth.store(depth, Ordering::SeqCst);
    }
}

#[async_trait]
impl QueueDepth for FakeQueueDepth {
    async fn child_work_depth(&self) -> Result<u64, DomainError> {
        Ok(self.depth.load(Ordering::SeqCst))
    }
}

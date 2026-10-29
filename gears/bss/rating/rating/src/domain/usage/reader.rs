//! `UsageFeedReader` (feature 04 §2.1, §3.3, §3.5; DESIGN [12 §4.6]): the single active reader of
//! one canonical source, run while this instance holds the Cluster leadership
//! `usage-feed-{source_uuid}`.
//!
//! Leadership is advisory: the page transaction's cursor CAS is what keeps a stale reader from
//! writing. Everything that can fail transiently — the feed, the declaration read, the tenant
//! hierarchy, the PDP — runs before the transaction under a time limit, so a failure backs off with
//! nothing committed and nothing rejected.
//!
//! [12 §4.6]: ../../../../docs/features/04-usage-intake.md#contract-12-4-6

// @cpt-dod:cpt-cf-bss-rating-dod-intake-source-loss:p1

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use toolkit_db::Db;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::authz::service_context;
use crate::domain::backoff::Backoff;
use crate::domain::error::DomainError;
use crate::domain::ports::{Clock, Dependency, TenantGrant};
use crate::domain::usage::declarations::{DeclarationCache, PageDeclarations};
use crate::domain::usage::entry;
use crate::domain::usage::feed::{
    FeedError, FeedPage, FeedStart, MAX_PAGE_LIMIT, MeterId, UsageFeedPort,
};
use crate::domain::usage::intake::{
    PageEntry, PageOutcome, PageReport, PreparedPage, RestartOutcome, UsageIntake,
};
use crate::domain::usage::loss::LossKind;
use crate::domain::usage::metrics::UsageMetricsPort;
use crate::domain::usage::source::{
    Checkpoint, SourceDefinition, SourceId, SourceState, TenantScope, Transition,
};
use crate::domain::usage::{CheckpointRepo, SourceLossRepo};

/// The share of `max_queue_depth` below which a paused reader resumes (feature 04 §3.5).
const RESUME_NUMERATOR: u64 = 4;
const RESUME_DENOMINATOR: u64 = 5;

/// Whether Rating's grant on the feed read still covers a source's configured tenant scope
/// (feature 04 §3.3 `inst-src-grant`).
#[async_trait]
pub trait FeedGrant: Send + Sync {
    /// `true` when the PDP grants Rating's service identity the collector's feed read on every
    /// root of `source`'s tenant scope.
    ///
    /// # Errors
    ///
    /// [`DomainError::AuthzUnavailable`] when the PDP cannot be evaluated.
    async fn covers(&self, source: &SourceDefinition) -> Result<bool, DomainError>;
}

/// Whether a tenant lies inside a source's configured tenant scope (DESIGN §4.8: each entry's
/// tenant is checked against the configured scope).
#[async_trait]
pub trait TenantHierarchy: Send + Sync {
    /// `true` when `tenant` is a root of `scope` or a descendant of one (barriers ignored: a
    /// self-managed tenant still lies in its parent's subtree).
    ///
    /// # Errors
    ///
    /// [`DomainError::DependencyUnavailable`] when the hierarchy cannot be read.
    async fn within(&self, scope: &TenantScope, tenant: Uuid) -> Result<bool, DomainError>;
}

/// The depth of the `rating.child_work` queue, which backpressure reads (feature 04 §3.5).
#[async_trait]
pub trait QueueDepth: Send + Sync {
    /// The messages enqueued on `rating.child_work` and not yet processed.
    ///
    /// # Errors
    ///
    /// A storage failure.
    async fn child_work_depth(&self) -> Result<u64, DomainError>;
}

/// Why a source is paused: the `reason` label of `rating_feed_paused`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PauseReason {
    /// Rating's grant no longer covers the configured tenant scope (`usage_grant_narrowed`).
    GrantNarrowed,
    /// `rating.child_work` is deeper than `max_queue_depth`.
    Backpressure,
}

impl PauseReason {
    /// The label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GrantNarrowed => "grant_narrowed",
            Self::Backpressure => "backpressure",
        }
    }
}

/// How far behind a source is, from the page it last read (DESIGN §4.7).
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedPosition {
    /// `rating_feed_lag_seconds`: now − the latest `accepted_at` of the page; zero when caught up.
    pub lag: Duration,
    /// `rating_feed_cursor_age_seconds`: now − when the cursor last advanced.
    pub cursor_age: Duration,
    /// `rating_feed_oldest_unread_age_seconds`: now − the earliest `window_end` of the page while
    /// not caught up; zero when caught up.
    pub oldest_unread_age: Duration,
}

/// The reader's tuning, checked when it is built: a zero page limit or pause would turn the reader
/// into a hot loop against the feed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderSettings {
    page_limit: u16,
    poll_interval: Duration,
    max_queue_depth: u64,
    backoff_cap: Duration,
    call_timeout: Duration,
}

impl ReaderSettings {
    /// The tuning: `page_limit` entries per page (1 to [`MAX_PAGE_LIMIT`]), `poll_interval` after
    /// a short page or a pause, reading stopped above `max_queue_depth`, failures backed off up to
    /// `backoff_cap`, and every call to the feed, the meter registry or the tenant hierarchy given
    /// up after `call_timeout`.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] on the first value out of range: a page limit outside
    /// 1–[`MAX_PAGE_LIMIT`], a zero queue depth or a zero duration.
    pub fn new(
        page_limit: u16,
        poll_interval: Duration,
        max_queue_depth: u64,
        backoff_cap: Duration,
        call_timeout: Duration,
    ) -> Result<Self, DomainError> {
        let refuse = |field: &'static str, detail: &str| DomainError::InvalidArgument {
            field,
            detail: detail.to_owned(),
            reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
        };
        if !(1..=MAX_PAGE_LIMIT).contains(&page_limit) {
            return Err(refuse("page_limit", "a page holds 1 to 1000 entries"));
        }
        if max_queue_depth == 0 {
            return Err(refuse(
                "max_queue_depth",
                "the queue depth limit is at least 1",
            ));
        }
        for (field, duration) in [
            ("poll_interval", poll_interval),
            ("backoff_cap", backoff_cap),
            ("call_timeout", call_timeout),
        ] {
            if duration.is_zero() {
                return Err(refuse(field, "a reader pause or time limit is not zero"));
            }
        }
        Ok(Self {
            page_limit,
            poll_interval,
            max_queue_depth,
            backoff_cap,
            call_timeout,
        })
    }

    /// Entries per page.
    #[must_use]
    pub const fn page_limit(&self) -> u16 {
        self.page_limit
    }

    /// The pause after a short or empty page.
    #[must_use]
    pub const fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Above this `rating.child_work` depth the reader stops fetching.
    #[must_use]
    pub const fn max_queue_depth(&self) -> u64 {
        self.max_queue_depth
    }

    /// The longest pause after a failure.
    #[must_use]
    pub const fn backoff_cap(&self) -> Duration {
        self.backoff_cap
    }

    /// The time limit of one call to the feed, the meter registry or the tenant hierarchy.
    #[must_use]
    pub const fn call_timeout(&self) -> Duration {
        self.call_timeout
    }
}

/// What the reader is built from.
#[domain_model]
pub struct ReaderParts {
    /// The gear's database.
    pub db: Db,
    /// The usage feed.
    pub feed: Arc<dyn UsageFeedPort>,
    /// The meter declarations, cached.
    pub declarations: DeclarationCache,
    /// The PDP check of the feed read.
    pub feed_grant: Arc<dyn FeedGrant>,
    /// Whether an entry's tenant lies inside the source's configured scope.
    pub hierarchy: Arc<dyn TenantHierarchy>,
    /// Rating's service grant per tenant.
    pub tenant_grant: Arc<dyn TenantGrant>,
    /// `rating.child_work` depth.
    pub depth: Arc<dyn QueueDepth>,
    /// The page and restart transactions.
    pub intake: UsageIntake,
    /// The checkpoints.
    pub checkpoints: Arc<dyn CheckpointRepo>,
    /// The losses, for the open-loss gauge.
    pub losses: Arc<dyn SourceLossRepo>,
    /// The feed metrics.
    pub metrics: Arc<dyn UsageMetricsPort>,
    /// The time source.
    pub clock: Arc<dyn Clock>,
    /// Its tuning.
    pub settings: ReaderSettings,
}

/// What one step of the reader did.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A page was committed; `caught_up` when it was short or empty.
    Committed {
        /// What the page did.
        report: PageReport,
        /// Whether the reader reached the feed's current end.
        caught_up: bool,
    },
    /// The cursor was past retention: a loss is open and the source restarts from `Oldest`.
    Restarted,
    /// Another reader moved the cursor first; nothing was written.
    CursorMoved,
    /// The source is paused; nothing was read or nothing committed.
    Paused(PauseReason),
    /// A draining source reached its end and is now retired.
    Drained,
    /// The source is retired; it is not read.
    Retired,
}

/// Why [`UsageFeedReader::run`] returned.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// The term ended: the leadership was lost or the gear stops.
    TermEnded,
    /// The source is retired: there is nothing to read, so the caller gives up the leadership
    /// rather than hold it idle, and leads the source again only once it is not retired.
    Retired,
}

/// The reader of the usage sources.
#[domain_model]
#[derive(Clone)]
pub struct UsageFeedReader {
    parts: Arc<ReaderParts>,
}

impl UsageFeedReader {
    /// A reader over `parts`.
    #[must_use]
    pub fn new(parts: ReaderParts) -> Self {
        Self {
            parts: Arc::new(parts),
        }
    }

    /// The reader's tuning.
    #[must_use]
    pub fn settings(&self) -> ReaderSettings {
        self.parts.settings
    }

    /// One term of reading `source`: its pauses start empty and are cleared when it ends.
    #[must_use]
    pub fn session(&self, source: SourceId) -> SourceSession {
        SourceSession {
            reader: self.clone(),
            source,
            paused: BTreeSet::new(),
        }
    }

    /// Reads `source` until `term` ends (the leadership is lost or the gear stops) or the source
    /// is retired: a page after another while there is more, a poll after a short page or a
    /// pause, a growing jittered backoff after a failure.
    pub async fn run(&self, source: SourceId, term: CancellationToken) -> RunEnd {
        let mut session = self.session(source);
        let ended = session.run(&term).await;
        session.end();
        ended
    }

    /// Whether `source` is stored and not retired: a reader that gave up its leadership leads the
    /// source again once a reconciliation makes it active.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub async fn is_readable(&self, source: SourceId) -> Result<bool, DomainError> {
        let checkpoint = self.checkpoint(source).await?;
        Ok(checkpoint.state != SourceState::Retired)
    }

    async fn checkpoint(&self, source: SourceId) -> Result<Checkpoint, DomainError> {
        let checkpoints = Arc::clone(&self.parts.checkpoints);
        self.parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { checkpoints.lock(tx, source).await })
            })
            .await?
            .ok_or(DomainError::NotFound {
                what: "usage source checkpoint",
            })
    }

    /// The grant check before a read and before a commit.
    async fn grant_covers(&self, definition: &SourceDefinition) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-grant
        self.parts.feed_grant.covers(definition).await
        // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-grant
    }

    /// One page after the checkpoint's cursor, or `None` when the feed refused the cursor as
    /// beyond retention.
    async fn read(&self, checkpoint: &Checkpoint) -> Result<Option<FeedPage>, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-read
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-cursor
        // The cursor is only ever sent with the subscription of the source that received it.
        let definition = &checkpoint.definition;
        let ctx = service_context(definition.tenant_scope().owner())?;
        let limit = self.parts.settings.page_limit;
        let read = self.parts.feed.read_usage_feed(
            &ctx,
            definition.subscription(),
            FeedStart::from_checkpoint(checkpoint.cursor.as_ref()),
            None,
            limit,
        );
        let page = match self
            .bounded(Dependency::UsageFeed, "the feed read", read)
            .await?
        {
            Ok(page) => page,
            Err(FeedError::CursorBeyondRetention) => return Ok(None),
            Err(error) => return Err(feed_failure(error)),
        };
        // @cpt-end:cpt-cf-bss-rating-algo-intake-source-identity:p1:inst-src-cursor
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-read
        // The page size sets the transaction's size and the memory a page holds: a feed that
        // ignores the limit is refused, not applied.
        if page.entries.len() > usize::from(limit) {
            return Err(DomainError::DependencyUnavailable {
                dependency: Dependency::UsageFeed,
                detail: format!(
                    "the feed returned {} entries for a page of at most {limit}",
                    page.entries.len()
                ),
                cause: None,
            });
        }
        Ok(Some(page))
    }

    /// `call` given up after the reader's call timeout, as `dependency` being unavailable.
    async fn bounded<T>(
        &self,
        dependency: Dependency,
        what: &str,
        call: impl Future<Output = T>,
    ) -> Result<T, DomainError> {
        let timeout = self.parts.settings.call_timeout;
        tokio::time::timeout(timeout, call)
            .await
            .map_err(|_elapsed| DomainError::DependencyUnavailable {
                dependency,
                detail: format!("{what} took longer than {timeout:?}"),
                cause: None,
            })
    }

    /// The restart after `CursorBeyondRetention`, under the cursor CAS.
    async fn restart(&self, checkpoint: &Checkpoint) -> Result<Step, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-retention
        let definition = &checkpoint.definition;
        let owner = definition.tenant_scope().owner();
        let scope = self.parts.tenant_grant.scope_for(owner).await?;
        let outcome = self
            .parts
            .intake
            .restart_after_retention_loss(
                &self.parts.db,
                definition,
                checkpoint.cursor.clone(),
                scope,
            )
            .await?;
        let RestartOutcome::Restarted(loss) = outcome else {
            return Ok(Step::CursorMoved);
        };
        tracing::warn!(
            target: "bss_rating.usage",
            source = %checkpoint.source_id,
            loss_id = %loss.loss_id,
            code = LossKind::CursorBeyondRetention.as_str(),
            "the feed refused the cursor as beyond retention; source loss open, restarting from Oldest"
        );
        self.parts.metrics.feed_restart(checkpoint.source_id);
        self.report_losses(checkpoint.source_id).await;
        Ok(Step::Restarted)
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-retention
    }

    /// Sorts the page's entries, resolves their meter declarations and binds the scope of each
    /// tenant they name and of the source's owner root, before the transaction. An entry of a
    /// meter the source does not subscribe to, or of a tenant outside its configured scope, is
    /// set aside: the source never asked for it. `None` when a tenant inside the scope is outside
    /// Rating's grant (the source pauses as narrowed).
    async fn prepare(
        &self,
        definition: &SourceDefinition,
        page: FeedPage,
    ) -> Result<Option<PreparedPage>, DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-prepare
        let next = page
            .next
            .ok_or_else(|| DomainError::DependencyUnavailable {
                dependency: Dependency::UsageFeed,
                detail: "a live read returned no continuation cursor".to_owned(),
                cause: None,
            })?;
        let mut within: BTreeMap<Uuid, bool> = BTreeMap::new();
        let mut entries = Vec::with_capacity(page.entries.len());
        for raw in page.entries {
            let Some(identity) = entry::identify(&raw) else {
                entries.push(PageEntry::Unidentifiable(raw));
                continue;
            };
            let outside_meter = meter_of(&raw)
                .is_some_and(|meter| !definition.subscription().meters().contains(&meter));
            let inside = if outside_meter {
                false
            } else if let Some(known) = within.get(&identity.tenant_id) {
                *known
            } else {
                let answer = self
                    .within(definition.tenant_scope(), identity.tenant_id)
                    .await?;
                within.insert(identity.tenant_id, answer);
                answer
            };
            entries.push(if inside {
                PageEntry::Identified { identity, raw }
            } else {
                PageEntry::OutsideSubscription(raw)
            });
        }
        let meters: BTreeSet<MeterId> = entries
            .iter()
            .filter_map(PageEntry::identified)
            .filter_map(|(_, raw)| meter_of(raw))
            .collect();
        let declarations: PageDeclarations =
            self.parts.declarations.resolve_page(meters.iter()).await?;
        let mut tenants: BTreeSet<Uuid> = entries
            .iter()
            .filter_map(PageEntry::identified)
            .map(|(identity, _)| identity.tenant_id)
            .collect();
        tenants.insert(definition.tenant_scope().owner());
        let mut scopes: BTreeMap<Uuid, AccessScope> = BTreeMap::new();
        for tenant in tenants {
            match self.parts.tenant_grant.scope_for(tenant).await {
                Ok(scope) => {
                    scopes.insert(tenant, scope);
                }
                Err(DomainError::OutsideGrant { .. }) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        Ok(Some(PreparedPage {
            entries,
            next,
            declarations,
            scopes,
        }))
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-prepare
    }

    /// Whether `tenant` lies inside `scope`: a root without asking, a descendant as the tenant
    /// hierarchy answers.
    async fn within(&self, scope: &TenantScope, tenant: Uuid) -> Result<bool, DomainError> {
        if scope.roots().contains(&tenant) {
            return Ok(true);
        }
        let ask = self.parts.hierarchy.within(scope, tenant);
        self.bounded(
            Dependency::TenantHierarchy,
            "the tenant hierarchy read",
            ask,
        )
        .await?
    }

    /// Retires a draining source read to its end; `false` when it is no longer draining (a
    /// reconciliation made it active again meanwhile), so it is read on.
    async fn retire_drained(&self, source: SourceId) -> Result<bool, DomainError> {
        let checkpoints = Arc::clone(&self.parts.checkpoints);
        let now = self.parts.clock.now();
        let retired = self
            .parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    checkpoints
                        .set_state(tx, source, Transition::RETIRE_DRAINED, now)
                        .await
                })
            })
            .await?;
        if retired {
            tracing::info!(target: "bss_rating.usage", source = %source, "draining source reached its end; retired");
        } else {
            tracing::info!(target: "bss_rating.usage", source = %source, "draining source reached its end but is active again; reading on");
        }
        Ok(retired)
    }

    /// Sets the open-loss gauge of `source` from the stored losses; a failure is only logged.
    async fn report_losses(&self, source: SourceId) {
        let counted = self.count_losses(source).await;
        match counted {
            Ok(counts) => {
                for kind in LossKind::ALL {
                    let unresolved = counts.get(&kind).copied().unwrap_or(0);
                    self.parts.metrics.source_losses(source, kind, unresolved);
                }
            }
            Err(error) => {
                tracing::warn!(target: "bss_rating.usage", source = %source, %error, "could not count the source's open losses");
            }
        }
    }

    async fn count_losses(&self, source: SourceId) -> Result<BTreeMap<LossKind, u64>, DomainError> {
        let checkpoint = self.checkpoint(source).await?;
        let owner = checkpoint.definition.tenant_scope().owner();
        let scope = self.parts.tenant_grant.scope_for(owner).await?;
        let losses = Arc::clone(&self.parts.losses);
        self.parts
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move { losses.count_unresolved(tx, &scope, source).await })
            })
            .await
    }

    fn report_failure(&self, source: SourceId, error: &DomainError) {
        if let Some((dependency, code)) = error.dependency_failure() {
            self.parts.metrics.dependency_error(dependency, code);
        }
        tracing::warn!(target: "bss_rating.usage", source = %source, %error, "usage feed step failed; backing off");
    }
}

/// One term of reading one source: the reader and the pauses this term set. The pauses live no
/// longer than the term, so a source whose leadership moves away leaves no pause behind here.
#[domain_model]
pub struct SourceSession {
    reader: UsageFeedReader,
    source: SourceId,
    paused: BTreeSet<PauseReason>,
}

impl SourceSession {
    /// The steps of one term, until `term` ends or the source is retired.
    async fn run(&mut self, term: &CancellationToken) -> RunEnd {
        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-lock
        // Called only while this instance leads `usage-feed-{source_uuid}`; without it, no read.
        let reader = self.reader.clone();
        let settings = reader.parts.settings;
        reader.report_losses(self.source).await;
        let mut backoff = Backoff::new(settings.backoff_cap);
        while !term.is_cancelled() {
            let pause = match self.step().await {
                Ok(step) => {
                    backoff.reset();
                    match step {
                        Step::Committed {
                            caught_up: false, ..
                        }
                        | Step::Restarted
                        | Step::CursorMoved => continue,
                        // @cpt-begin:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-poll
                        Step::Committed {
                            caught_up: true, ..
                        }
                        | Step::Paused(_) => settings.poll_interval,
                        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-poll
                        Step::Drained | Step::Retired => return RunEnd::Retired,
                    }
                }
                Err(error) => {
                    reader.report_failure(self.source, &error);
                    backoff.pause()
                }
            };
            tokio::select! {
                () = term.cancelled() => return RunEnd::TermEnded,
                () = tokio::time::sleep(pause) => {}
            }
        }
        RunEnd::TermEnded
        // @cpt-end:cpt-cf-bss-rating-flow-intake-read-page:p1:inst-rp-lock
    }

    /// One step of the source's reader: at most one read and one transaction.
    ///
    /// # Errors
    ///
    /// A transient failure of the feed, the declaration read, the tenant hierarchy, the PDP or
    /// the database, or a page larger than the limit: nothing was committed and the caller backs
    /// off.
    pub async fn step(&mut self) -> Result<Step, DomainError> {
        let reader = self.reader.clone();
        let source = self.source;
        let checkpoint = reader.checkpoint(source).await?;
        if checkpoint.state == SourceState::Retired {
            return Ok(Step::Retired);
        }
        if self.backpressure().await? {
            return Ok(Step::Paused(PauseReason::Backpressure));
        }
        let definition = &checkpoint.definition;
        if !reader.grant_covers(definition).await? {
            return Ok(self.narrowed());
        }
        let now = reader.parts.clock.now();
        let cursor_age = elapsed(checkpoint.updated_at, now);
        let Some(page) = reader.read(&checkpoint).await? else {
            return reader.restart(&checkpoint).await;
        };
        let caught_up = page.entries.len() < usize::from(reader.parts.settings.page_limit);
        let position = position(&page.entries, cursor_age, caught_up, now);
        let Some(prepared) = reader.prepare(definition, page).await? else {
            return Ok(self.narrowed());
        };
        if !reader.grant_covers(definition).await? {
            return Ok(self.narrowed());
        }
        self.set_paused(PauseReason::GrantNarrowed, false);
        let outcome = reader
            .parts
            .intake
            .apply_page(
                &reader.parts.db,
                definition,
                checkpoint.cursor.clone(),
                prepared,
            )
            .await?;
        let PageOutcome::Committed(report) = outcome else {
            return Ok(Step::CursorMoved);
        };
        reader.parts.metrics.feed_page(source, &report);
        reader.parts.metrics.feed_position(source, position);
        if !report.losses_opened.is_empty() {
            reader.report_losses(source).await;
        }
        if caught_up
            && checkpoint.state == SourceState::Draining
            && reader.retire_drained(source).await?
        {
            return Ok(Step::Drained);
        }
        Ok(Step::Committed { report, caught_up })
    }

    /// Clears the pauses this term set, so the gauges of a source this instance no longer reads
    /// fall back to 0.
    pub fn end(&mut self) {
        for reason in std::mem::take(&mut self.paused) {
            self.reader
                .parts
                .metrics
                .feed_paused(self.source, reason, false);
        }
    }

    /// Whether the reader holds off for backpressure: it stops above `max_queue_depth` and
    /// resumes below 80 % of it; nothing is dropped, the cursor simply stays.
    async fn backpressure(&mut self) -> Result<bool, DomainError> {
        let depth = self.reader.parts.depth.child_work_depth().await?;
        let max = self.reader.parts.settings.max_queue_depth;
        let paused = self.paused.contains(&PauseReason::Backpressure);
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-backpressure:p2:inst-bp-stop
        if !paused && depth > max {
            self.set_paused(PauseReason::Backpressure, true);
            return Ok(true);
        }
        // @cpt-end:cpt-cf-bss-rating-algo-intake-backpressure:p2:inst-bp-stop
        // @cpt-begin:cpt-cf-bss-rating-algo-intake-backpressure:p2:inst-bp-resume
        // `depth < max × 4/5`, without dividing.
        let below_resume =
            depth.saturating_mul(RESUME_DENOMINATOR) < max.saturating_mul(RESUME_NUMERATOR);
        if paused && below_resume {
            self.set_paused(PauseReason::Backpressure, false);
            return Ok(false);
        }
        Ok(paused)
        // @cpt-end:cpt-cf-bss-rating-algo-intake-backpressure:p2:inst-bp-resume
    }

    /// Pauses the source as narrowed (`usage_grant_narrowed`); it resumes once a page passes every
    /// grant check again.
    fn narrowed(&mut self) -> Step {
        if !self.paused.contains(&PauseReason::GrantNarrowed) {
            self.set_paused(PauseReason::GrantNarrowed, true);
            tracing::warn!(
                target: "bss_rating.usage",
                source = %self.source,
                code = "usage_grant_narrowed",
                "Rating's grant no longer covers the source's configured tenant scope; paused"
            );
        }
        Step::Paused(PauseReason::GrantNarrowed)
    }

    /// Sets or clears one pause; the gauge moves only when the pause does.
    fn set_paused(&mut self, reason: PauseReason, paused: bool) {
        let changed = if paused {
            self.paused.insert(reason)
        } else {
            self.paused.remove(&reason)
        };
        if changed {
            self.reader
                .parts
                .metrics
                .feed_paused(self.source, reason, paused);
        }
    }
}

/// The meter an entry names, when it names a well-formed one.
fn meter_of(raw: &Value) -> Option<MeterId> {
    raw.get("gts_type_id")
        .and_then(Value::as_str)
        .and_then(|meter| MeterId::parse(meter).ok())
}

fn feed_failure(error: FeedError) -> DomainError {
    DomainError::DependencyUnavailable {
        dependency: Dependency::UsageFeed,
        detail: error.to_string(),
        cause: match error {
            FeedError::Unavailable(_, cause) => cause,
            FeedError::CursorBeyondRetention | FeedError::Refused(_) => None,
        },
    }
}

/// `now − at`, zero when `at` is in the future.
fn elapsed(at: OffsetDateTime, now: OffsetDateTime) -> Duration {
    Duration::try_from(now - at).unwrap_or(Duration::ZERO)
}

/// The position the page shows: lag from its latest `accepted_at`, the oldest unread age from its
/// earliest `window_end` (both zero once caught up).
fn position(
    entries: &[Value],
    cursor_age: Duration,
    caught_up: bool,
    now: OffsetDateTime,
) -> FeedPosition {
    if caught_up {
        return FeedPosition {
            cursor_age,
            ..FeedPosition::default()
        };
    }
    let instants = |field: &str| -> Vec<OffsetDateTime> {
        entries
            .iter()
            .filter_map(|raw| raw.get(field).and_then(Value::as_str))
            .filter_map(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
            .collect()
    };
    FeedPosition {
        lag: instants("accepted_at")
            .into_iter()
            .max()
            .map_or(Duration::ZERO, |latest| elapsed(latest, now)),
        cursor_age,
        oldest_unread_age: instants("window_end")
            .into_iter()
            .min()
            .map_or(Duration::ZERO, |oldest| elapsed(oldest, now)),
    }
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;

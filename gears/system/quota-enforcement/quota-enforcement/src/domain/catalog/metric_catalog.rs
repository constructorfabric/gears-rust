//! `MetricCatalog`: the process-local catalogue of the metric types QE has
//! read, and the cache of registered subject types (ADR-0012).
//!
//! A metric type is read from the registry at start when an active Quota or
//! Policy names it, otherwise the first time a request or a Quota or Policy
//! write names it; a held entry answers every later request without a
//! registry call. A scheduled conditional read brings each held type up to
//! date. An entry is built from the registry content alone, never from stored
//! state.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use gts::GtsTypeId;
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use quota_enforcement_sdk::{MetricId, SubjectRef};
use quota_resolver_sdk::{QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID};
use serde_json::json;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use super::model::{CatalogEntry, ServedMetric, build_entry, derives_from, validate};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{
    CatalogReadPhase, QeMetrics, ValidationReason, ValidationSurface,
};
use crate::domain::ports::type_registry::{RegisteredType, TypeEtag, TypeRead, TypeRegistry};
use crate::domain::tokens;

const LOG_TARGET: &str = "qe.catalog";

/// How long and how many "the registry does not know this type" answers the
/// catalogue remembers.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct UnknownMemory {
    /// How long one answer is remembered.
    pub ttl: Duration,
    /// How many answers are remembered; the least recently used goes first.
    pub capacity: NonZeroUsize,
}

/// How long first-use reads fail at once after one that failed.
const FAILURE_MEMORY: Duration = Duration::from_secs(1);

/// The catalogue and the cache of registered subject types.
#[domain_model]
pub struct MetricCatalog {
    registry: Arc<dyn TypeRegistry>,
    metrics: Arc<dyn QeMetrics>,
    entries: RwLock<HashMap<MetricId, (CatalogEntry, TypeEtag)>>,
    subjects: RwLock<HashMap<GtsTypeId, Arc<RegisteredType>>>,
    unknown: Mutex<LruCache<GtsTypeId, Instant>>,
    unknown_ttl: Duration,
    /// When each served metric may next be read again outside the schedule.
    rereads: Mutex<HashMap<MetricId, Instant>>,
    /// The first-use reads in progress, by type: a request that misses a type
    /// already being read waits for that read, and reads only the rest itself,
    /// so each type is read once and reads of other types never queue.
    in_flight: Mutex<HashMap<GtsTypeId, Flight>>,
    /// Until when a first-use read fails at once after one that failed.
    registry_failed_until: Mutex<Option<Instant>>,
}

impl fmt::Debug for MetricCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetricCatalog")
            .field("entries", &self.entries.read().len())
            .field("subjects", &self.subjects.read().len())
            .finish_non_exhaustive()
    }
}

impl MetricCatalog {
    /// An empty catalogue over `registry`.
    #[must_use]
    pub fn new(
        registry: Arc<dyn TypeRegistry>,
        metrics: Arc<dyn QeMetrics>,
        unknown: UnknownMemory,
    ) -> Self {
        Self {
            registry,
            metrics,
            entries: RwLock::new(HashMap::new()),
            subjects: RwLock::new(HashMap::new()),
            unknown: Mutex::new(LruCache::new(unknown.capacity)),
            unknown_ttl: unknown.ttl,
            rereads: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
            registry_failed_until: Mutex::new(None),
        }
    }

    /// The metric type `raw` names, when its id has the form of a metric type
    /// id: a GTS type id derived from the metric base. No registry call.
    #[must_use]
    pub fn parse_metric(raw: &str) -> Option<MetricId> {
        GtsTypeId::try_new(raw)
            .ok()
            .filter(|id| derives_from(id, QUOTA_METRIC_TYPE_ID))
            .map(MetricId::new)
    }

    /// The subject type `raw` names, when its id has the form of a quota
    /// subject type id. No registry call.
    #[must_use]
    pub fn parse_subject_type(raw: &str) -> Option<GtsTypeId> {
        GtsTypeId::try_new(raw)
            .ok()
            .filter(|id| derives_from(id, QUOTA_SUBJECT_TYPE_ID))
    }

    /// The entry the catalogue holds for `metric`, without a registry call.
    #[must_use]
    pub fn peek(&self, metric: &MetricId) -> Option<CatalogEntry> {
        self.entries
            .read()
            .get(metric)
            .map(|(entry, _)| entry.clone())
    }

    /// Every served entry the catalogue holds, without a registry call.
    #[must_use]
    pub fn served_entries(&self) -> Vec<Arc<ServedMetric>> {
        self.entries
            .read()
            .values()
            .filter_map(|(entry, _)| match entry {
                CatalogEntry::Served(served) => Some(Arc::clone(served)),
                CatalogEntry::Unavailable { .. } => None,
            })
            .collect()
    }

    /// The served entry of `metric`, read from the registry on first use.
    /// `surface` labels the rejections this records.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when `metric` does not have the form
    ///   of a metric type id; no registry call is made.
    /// - [`DomainError::MetricNotRegistered`] when the registry does not know
    ///   the type or holds it as deleted.
    /// - [`DomainError::TypesRegistryUnavailable`] when the first read fails
    ///   or reaches its time limit; nothing is remembered.
    /// - [`DomainError::ProjectionNotResolvable`] when the entry is
    ///   unavailable.
    pub async fn served(
        &self,
        metric: &MetricId,
        surface: ValidationSurface,
    ) -> Result<Arc<ServedMetric>, DomainError> {
        if !derives_from(metric.as_gts(), QUOTA_METRIC_TYPE_ID) {
            return Err(DomainError::InvalidArgument {
                field: "metric",
                reason: tokens::METRIC_INVALID,
            });
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-lookup
        let entry = self.entry(metric).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-lookup
        match entry {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unknown-if
            // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unknown
            None => {
                self.metrics.record_contract_validation_failure(
                    surface,
                    ValidationReason::MetricUnregistered,
                );
                Err(DomainError::MetricNotRegistered {
                    metric: metric.to_string(),
                })
            }
            // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unknown
            // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unknown-if
            Some(CatalogEntry::Served(served)) => Ok(served),
            // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-unavail-if
            // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-unavail
            Some(CatalogEntry::Unavailable { .. }) => {
                self.metrics.record_contract_validation_failure(
                    surface,
                    ValidationReason::ProjectionNotResolvable,
                );
                Err(DomainError::ProjectionNotResolvable {
                    metric: metric.to_string(),
                })
            } // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-unavail
              // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-unavail-if
        }
    }

    /// The entry of `metric`, read from the registry on first use. `Ok(None)`
    /// when the registry does not know the type; that answer is remembered
    /// for a short time.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the first read fails or
    /// reaches its time limit.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-resolve
    pub async fn entry(&self, metric: &MetricId) -> Result<Option<CatalogEntry>, DomainError> {
        if let Some(entry) = self.peek(metric) {
            return Ok(Some(entry));
        }
        if !is_metric_type(metric.as_gts()) {
            return Ok(Some(self.hold_not_derived(metric)));
        }
        if self.is_unknown(metric.as_gts()) {
            return Ok(None);
        }
        self.read_on_first_use(vec![metric.as_gts().clone()])
            .await?;
        Ok(self.peek(metric))
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-resolve

    /// Read a served metric type again outside the schedule, for a request
    /// its entry rejects: the owner may have revised the type since the last
    /// scheduled read. Returns the entry built from the revision, and `None`
    /// when the type did not change, is no longer served, the read fails, or
    /// the metric was read this way within the time an unknown id is
    /// remembered, so rejected requests cannot turn into a stream of registry
    /// reads.
    pub async fn reread(&self, metric: &MetricId) -> Option<Arc<ServedMetric>> {
        let etag = match self.entries.read().get(metric) {
            Some((CatalogEntry::Served(_), etag)) => etag.clone(),
            _ => return None,
        };
        {
            let now = Instant::now();
            let mut rereads = self.rereads.lock();
            if rereads.get(metric).is_some_and(|next| *next > now) {
                return None;
            }
            rereads.insert(metric.clone(), now + self.unknown_ttl);
        }
        let id = metric.as_gts().clone();
        let mut answers = self
            .registry
            .read_types(vec![(id.clone(), Some(etag.clone()))])
            .await
            .inspect_err(|_| {
                self.metrics
                    .record_catalog_read_failure(CatalogReadPhase::FirstUse);
            })
            .ok()?;
        let TypeRead::Found(registered) = answers.remove(&id)? else {
            return None;
        };
        // The read ran outside any lock: a scheduled read may have stored a
        // newer revision meanwhile, which this one must not overwrite.
        self.hold(id, *registered, &Hold::IfUnchangedFrom(etag));
        self.publish_unavailable();
        match self.peek(metric) {
            Some(CatalogEntry::Served(served)) => Some(served),
            _ => None,
        }
    }

    /// Read, with one batch read, every type of `ids` the catalogue neither
    /// holds nor remembers as unknown, so the per-type checks that follow make
    /// no registry call.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the read fails or
    /// reaches its time limit.
    pub async fn load<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a GtsTypeId>,
    ) -> Result<(), DomainError> {
        let missing = self.missing(ids);
        self.read_on_first_use(missing).await
    }

    /// Read the metric types the active Quotas and Policies name, so a
    /// replica does not take traffic with an empty catalogue.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the read fails or
    /// reaches its time limit.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-start
    pub async fn read_at_start(&self, metrics: &[MetricId]) -> Result<(), DomainError> {
        let (derived, not_derived): (Vec<_>, Vec<_>) = metrics
            .iter()
            .partition(|metric| is_metric_type(metric.as_gts()));
        for metric in not_derived {
            self.hold_not_derived(metric);
        }
        let missing = self.missing(derived.into_iter().map(MetricId::as_gts));
        self.read(missing, CatalogReadPhase::Start).await
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-start

    /// One scheduled read: ask the registry, with a conditional read, whether
    /// each held metric type and each cached subject type changed; build again
    /// each changed entry, remove the entry of a deleted metric type, and drop
    /// a deleted subject type. A read that fails changes nothing.
    ///
    /// # Errors
    ///
    /// [`DomainError::TypesRegistryUnavailable`] when the read fails or
    /// reaches its time limit.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-refresh
    pub async fn refresh(&self) -> Result<(), DomainError> {
        let mut reads: Vec<_> = self
            .entries
            .read()
            .iter()
            .filter(|(metric, _)| is_metric_type(metric.as_gts()))
            .map(|(metric, (_, etag))| (metric.as_gts().clone(), Some(etag.clone())))
            .collect();
        reads.extend(
            self.subjects
                .read()
                .iter()
                .map(|(id, registered)| (id.clone(), Some(registered.etag.clone()))),
        );
        if !reads.is_empty() {
            let answers = self.registry.read_types(reads).await.inspect_err(|err| {
                self.metrics
                    .record_catalog_read_failure(CatalogReadPhase::Scheduled);
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %err,
                    "the scheduled catalogue read failed; every entry stays as it is"
                );
            })?;
            for (id, read) in answers {
                match read {
                    // @cpt-begin:cpt-cf-quota-enforcement-state-projection-catalog:p1:inst-catst-revised
                    TypeRead::Found(registered) => self.hold(id, *registered, &Hold::Replace),
                    // @cpt-end:cpt-cf-quota-enforcement-state-projection-catalog:p1:inst-catst-revised
                    // @cpt-begin:cpt-cf-quota-enforcement-state-projection-catalog:p1:inst-catst-deleted
                    TypeRead::Absent => self.forget(&id),
                    // @cpt-end:cpt-cf-quota-enforcement-state-projection-catalog:p1:inst-catst-deleted
                    TypeRead::Unchanged => {}
                }
            }
            self.publish_unavailable();
            self.metrics.record_catalog_scheduled_read();
        }
        Ok(())
    }

    /// Run [`Self::refresh`] every `interval` until `cancel` fires. Every
    /// replica runs its own: the catalogue is process-local.
    pub async fn run_scheduled_reads(&self, interval: Duration, cancel: CancellationToken) {
        let mut ticks = tokio::time::interval_at(Instant::now() + interval, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                _ = ticks.tick() => {}
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                _outcome = self.refresh() => {}
            }
        }
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-refresh

    /// Check that `subject` names a registered, concrete subject type and fits
    /// its schema. A type the cache does not hold is read once; a registered
    /// type stays cached until a scheduled read finds it deleted.
    ///
    /// # Errors
    ///
    /// - [`DomainError::ProjectionNotRegistered`] when the registry does not
    ///   know the type.
    /// - [`DomainError::InvalidArgument`] when the type is abstract or the
    ///   subject does not fit its schema.
    /// - [`DomainError::TypesRegistryUnavailable`] when the read fails.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-registered
    pub async fn check_subject(
        &self,
        subject: &SubjectRef,
        surface: ValidationSurface,
    ) -> Result<(), DomainError> {
        self.load([&subject.subject_type]).await?;
        let Some(registered) = self.subjects.read().get(&subject.subject_type).cloned() else {
            self.metrics
                .record_contract_validation_failure(surface, ValidationReason::Unregistered);
            return Err(DomainError::ProjectionNotRegistered {
                projection: subject.subject_type.to_string(),
            });
        };
        let invalid = |reason, token| {
            self.metrics
                .record_contract_validation_failure(surface, reason);
            DomainError::InvalidArgument {
                field: "subject.type",
                reason: token,
            }
        };
        if registered.is_abstract {
            return Err(invalid(
                ValidationReason::Abstract,
                tokens::SUBJECT_TYPE_ABSTRACT,
            ));
        }
        let Ok(validator) = &registered.payload else {
            return Err(invalid(
                ValidationReason::SchemaInvalid,
                tokens::SUBJECT_INVALID,
            ));
        };
        let document = json!({ "type": subject.subject_type.as_ref(), "id": subject.subject_id });
        validate(validator, &document).map_err(|violations| {
            tracing::warn!(
                target: LOG_TARGET,
                subject_type = %subject.subject_type,
                ?violations,
                "the subject does not fit its subject type"
            );
            invalid(ValidationReason::SchemaViolation, tokens::SUBJECT_INVALID)
        })
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-registered

    /// The distinct ids of `ids` the catalogue neither holds nor remembers as
    /// unknown.
    fn missing<'a>(&self, ids: impl IntoIterator<Item = &'a GtsTypeId>) -> Vec<GtsTypeId> {
        let mut seen = HashSet::new();
        ids.into_iter()
            .filter(|id| seen.insert(*id))
            .filter(|id| !self.holds(id) && !self.is_unknown(id))
            .cloned()
            .collect()
    }

    fn holds(&self, id: &GtsTypeId) -> bool {
        if is_metric_type(id) {
            self.entries.read().contains_key(&MetricId::new(id.clone()))
        } else {
            self.subjects.read().contains_key(id)
        }
    }

    /// Whether the registry answered recently that it does not know `id`.
    fn is_unknown(&self, id: &GtsTypeId) -> bool {
        let mut unknown = self.unknown.lock();
        match unknown.get(id) {
            Some(deadline) if *deadline > Instant::now() => true,
            Some(_) => {
                unknown.pop(id);
                false
            }
            None => false,
        }
    }

    /// Read on first use, once per type: a request waits for the reads in
    /// progress of the types it shares with them and reads the rest itself in
    /// one batch, so reads of different types run side by side. After a read
    /// that failed, the next ones fail at once for [`FAILURE_MEMORY`], so a
    /// registry that does not answer costs the reads already in progress, not
    /// every later one.
    async fn read_on_first_use(&self, ids: Vec<GtsTypeId>) -> Result<(), DomainError> {
        let mut ids = ids;
        loop {
            ids = self.missing(&ids);
            if ids.is_empty() {
                return Ok(());
            }
            let (lead, joined) = self.join_or_lead(&ids);
            if let Some(lead) = lead {
                let outcome = self.read_led(&lead.ids).await;
                lead.finish(outcome.clone());
                outcome?;
            }
            // A read whose leader was dropped unfinished answers nothing: the
            // loop reads its types again, now that it is gone from the map.
            let mut abandoned = false;
            for mut flight in joined {
                match flight.wait_for(Option::is_some).await {
                    Ok(outcome) => {
                        if let Some(Err(error)) = &*outcome {
                            return Err(error.clone());
                        }
                    }
                    Err(_) => abandoned = true,
                }
            }
            if !abandoned {
                return Ok(());
            }
        }
    }

    /// Join the reads in progress of the types of `ids` they cover, and lead a
    /// new read of the rest, if any.
    fn join_or_lead(&self, ids: &[GtsTypeId]) -> (Option<Lead<'_>>, Vec<Flight>) {
        let mut in_flight = self.in_flight.lock();
        let mut joined: Vec<Flight> = Vec::new();
        let mut own = Vec::new();
        for id in ids {
            match in_flight.get(id) {
                Some(flight) => {
                    if !joined.iter().any(|known| known.same_channel(flight)) {
                        joined.push(flight.clone());
                    }
                }
                None => own.push(id.clone()),
            }
        }
        if own.is_empty() {
            return (None, joined);
        }
        let (sender, flight) = tokio::sync::watch::channel(None);
        for id in &own {
            in_flight.insert(id.clone(), flight.clone());
        }
        let lead = Lead {
            catalog: self,
            ids: own,
            sender,
            flight,
        };
        (Some(lead), joined)
    }

    /// The read a leader makes. A read that finished between the caller's
    /// check and its lead already holds its types, so they are checked again.
    async fn read_led(&self, ids: &[GtsTypeId]) -> Result<(), DomainError> {
        let ids = self.missing(ids);
        if ids.is_empty() {
            return Ok(());
        }
        if (*self.registry_failed_until.lock()).is_some_and(|until| until > Instant::now()) {
            return Err(DomainError::TypesRegistryUnavailable(
                "a read of types-registry failed a moment ago".to_owned(),
            ));
        }
        let read = self.read(ids, CatalogReadPhase::FirstUse).await;
        if read.is_err() {
            *self.registry_failed_until.lock() = Some(Instant::now() + FAILURE_MEMORY);
        }
        read
    }

    /// Read `ids` unconditionally with one batch read and hold what the
    /// registry returns.
    async fn read(&self, ids: Vec<GtsTypeId>, phase: CatalogReadPhase) -> Result<(), DomainError> {
        if ids.is_empty() {
            return Ok(());
        }
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-read-failed-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-read-failed
        let answers = self
            .registry
            .read_types(ids.into_iter().map(|id| (id, None)).collect())
            .await
            .inspect_err(|_| self.metrics.record_catalog_read_failure(phase))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-read-failed
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-read-failed-if
        let mut held_metric = false;
        for (id, read) in answers {
            match read {
                TypeRead::Found(registered) => {
                    held_metric |= is_metric_type(&id);
                    self.hold(id, *registered, &Hold::IfAbsent);
                }
                TypeRead::Absent => {
                    self.unknown
                        .lock()
                        .put(id, Instant::now() + self.unknown_ttl);
                }
                TypeRead::Unchanged => {}
            }
        }
        if held_metric {
            self.publish_unavailable();
        }
        Ok(())
    }

    /// Hold the entry built from `registered`, in place of the one held or not,
    /// as `hold` says.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-source
    fn hold(&self, id: GtsTypeId, registered: RegisteredType, hold: &Hold) {
        if !is_metric_type(&id) {
            let mut subjects = self.subjects.write();
            if !matches!(hold, Hold::IfAbsent) || !subjects.contains_key(&id) {
                subjects.insert(id, Arc::new(registered));
            }
            return;
        }
        let metric = MetricId::new(id);
        let etag = registered.etag.clone();
        let entry = build_entry(registered);
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unavail-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unavail
        if let CatalogEntry::Unavailable { reason } = &entry {
            self.report_unavailable(&metric, *reason);
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unavail
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-unavail-if
        let mut entries = self.entries.write();
        let store = match hold {
            Hold::Replace => true,
            Hold::IfAbsent => !entries.contains_key(&metric),
            Hold::IfUnchangedFrom(read) => entries
                .get(&metric)
                .is_some_and(|(_, current)| current == read),
        };
        if store {
            entries.insert(metric, (entry, etag));
        }
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-source

    /// Hold the id of a stored Quota or Policy that does not derive from the
    /// metric base as an unavailable entry, without a registry read.
    fn hold_not_derived(&self, metric: &MetricId) -> CatalogEntry {
        let entry = CatalogEntry::Unavailable {
            reason: ValidationReason::NotDerived,
        };
        let inserted = {
            let mut entries = self.entries.write();
            if entries.contains_key(metric) {
                false
            } else {
                entries.insert(metric.clone(), (entry.clone(), TypeEtag(Vec::new())));
                true
            }
        };
        if inserted {
            self.report_unavailable(metric, ValidationReason::NotDerived);
            self.publish_unavailable();
        }
        entry
    }

    fn report_unavailable(&self, metric: &MetricId, reason: ValidationReason) {
        self.metrics
            .record_contract_validation_failure(ValidationSurface::Bootstrap, reason);
        tracing::warn!(
            target: LOG_TARGET,
            metric = %metric,
            %reason,
            "the metric type is not a valid metric type; the metric is unavailable"
        );
    }

    /// Remove a deleted type.
    fn forget(&self, id: &GtsTypeId) {
        if is_metric_type(id) {
            self.entries.write().remove(&MetricId::new(id.clone()));
            tracing::info!(
                target: LOG_TARGET,
                metric = %id,
                "the metric type was deleted; its entry is removed"
            );
        } else {
            self.subjects.write().remove(id);
        }
    }

    fn publish_unavailable(&self) {
        let count = self
            .entries
            .read()
            .values()
            .filter(|(entry, _)| matches!(entry, CatalogEntry::Unavailable { .. }))
            .count();
        self.metrics
            .set_catalog_unavailable_metrics(u64::try_from(count).unwrap_or(u64::MAX));
    }
}

fn is_metric_type(id: &GtsTypeId) -> bool {
    derives_from(id, QUOTA_METRIC_TYPE_ID)
}

/// When [`MetricCatalog::hold`] stores what a read returned.
enum Hold {
    /// Always: a scheduled read, the newest answer.
    Replace,
    /// Only when nothing is held: a first-use read.
    IfAbsent,
    /// Only while the held entry is still the one with this etag, which the
    /// read was based on.
    IfUnchangedFrom(TypeEtag),
}

/// A first-use read in progress, as its waiters see it: `None` until it ends.
type Flight = tokio::sync::watch::Receiver<Option<Result<(), DomainError>>>;

/// The read one request leads for `ids`. Dropped, finished or not, it takes
/// its types off the map, so a later request reads them again rather than
/// wait for a read that will not answer.
struct Lead<'a> {
    catalog: &'a MetricCatalog,
    ids: Vec<GtsTypeId>,
    sender: tokio::sync::watch::Sender<Option<Result<(), DomainError>>>,
    flight: Flight,
}

impl Lead<'_> {
    /// Answer every waiter with `outcome`.
    fn finish(self, outcome: Result<(), DomainError>) {
        self.sender.send_replace(Some(outcome));
    }
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        let mut in_flight = self.catalog.in_flight.lock();
        for id in &self.ids {
            if in_flight
                .get(id)
                .is_some_and(|flight| flight.same_channel(&self.flight))
            {
                in_flight.remove(id);
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metric_catalog_tests.rs"]
mod metric_catalog_tests;

// @cpt-dod:cpt-cf-settings-service-dod-audit-store-actor-names:p2
//! Actor names for the history read, through Account Management.
//!
//! A page's unmasked user-id actors are looked up once, each in the tenant its
//! record says the actor acted from, with the caller's own context: AM decides
//! what this caller may see and no privileged context is used. A name is kept
//! in this process for [`NAME_TTL`] for the caller AM gave it to and nobody
//! else. AM is a soft dependency: the client is found in the hub at each pass,
//! and a refused, absent, failed or late lookup reads `null`; the history never
//! fails because of it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use account_management_sdk::{AccountManagementClient, IdpUser, IdpUserPagination, ListUsersQuery};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::ports::ActorNameMetrics;

/// The most AM lookups one page runs at once.
pub const LOOKUP_CONCURRENCY: usize = 4;
/// The one budget of a page's lookups, queued chunks included.
pub const LOOKUP_BUDGET: Duration = Duration::from_secs(1);
/// How long a resolved name is reused for the caller it was resolved for.
pub const NAME_TTL: Duration = Duration::from_secs(300);
/// The most names one process keeps.
pub const NAME_CACHE_CAPACITY: usize = 10_000;

/// The SDK's largest id-set chunk.
const CHUNK_SIZE: usize = IdpUserPagination::MAX_TOP as usize;
/// What the nil actor reads when unmasked.
const SYSTEM_NAME: &str = "System";

/// The narrow boundary to AM's public user read. Tests supply a fake.
#[async_trait]
pub trait ActorDirectory: Send + Sync {
    /// Whether a directory is there to ask at all.
    fn is_available(&self) -> bool {
        true
    }

    /// Read one filtered page of `tenant_id`'s users with the caller's rights.
    ///
    /// # Errors
    /// Returns AM's authorization, absence and provider errors unchanged.
    // cancel-safe: the pass drops this future at its deadline; an implementation
    // only reads and keeps no state that a drop could leave half written.
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError>;
}

/// AM through the client hub, looked up at each call.
struct AmDirectory {
    hub: Arc<ClientHub>,
}

#[async_trait]
impl ActorDirectory for AmDirectory {
    fn is_available(&self) -> bool {
        self.hub.try_get::<dyn AccountManagementClient>().is_some()
    }

    // cancel-safe: one read through AM's public user read, with no side effect here.
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let am = self
            .hub
            .try_get::<dyn AccountManagementClient>()
            .ok_or_else(|| CanonicalError::service_unavailable().create())?;
        am.list_users(ctx, tenant_id, query).await
    }
}

/// The caller a name was resolved for: its tenant and subject.
type Caller = (Uuid, Uuid);

/// A resolved name and the instant it stops being reused.
type Entry = (String, tokio::time::Instant);

/// Resolved names by caller and actor.
#[derive(Default)]
struct NameCache {
    entries: Mutex<HashMap<(Caller, Uuid), Entry>>,
}

impl NameCache {
    /// The names of `ids` this caller was given and that are fresh at `now`.
    fn fresh(
        &self,
        caller: Caller,
        ids: &BTreeSet<Uuid>,
        now: tokio::time::Instant,
    ) -> BTreeMap<Uuid, String> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        ids.iter()
            .filter_map(|id| {
                entries
                    .get(&(caller, *id))
                    .filter(|(_, until)| *until > now)
                    .map(|(name, _)| (*id, name.clone()))
            })
            .collect()
    }

    /// Keep this caller's names until `now + ttl`; a full cache drops its expired
    /// names, then all.
    fn keep(
        &self,
        caller: Caller,
        names: &BTreeMap<Uuid, String>,
        now: tokio::time::Instant,
        ttl: Duration,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        if entries.len() + names.len() > NAME_CACHE_CAPACITY {
            entries.retain(|_, (_, until)| *until > now);
            if entries.len() + names.len() > NAME_CACHE_CAPACITY {
                entries.clear();
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        for (id, name) in names.iter().take(NAME_CACHE_CAPACITY) {
            entries.insert((caller, *id), (name.clone(), now + ttl));
        }
    }
}

/// Why a chunk's remaining ids stay unresolved with a warn line.
#[derive(Clone, Copy)]
enum Degraded {
    /// The page's budget ran out before or during a lookup.
    Budget,
    /// The directory failed.
    Directory,
    /// The provider's pages drifted: a repeated or unrequested id, no progress, a repeated cursor.
    Pagination,
    /// The id-set query or its cursor did not build.
    Query,
}

impl Degraded {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Directory => "directory",
            Self::Pagination => "pagination",
            Self::Query => "query",
        }
    }
}

/// What a lookup learned: the names it got, and whether any chunk degraded.
#[derive(Default)]
struct Outcome {
    names: BTreeMap<Uuid, String>,
    degraded: bool,
}

/// One actor of a history page, as the resolver needs it.
#[derive(Clone, Debug)]
pub struct ActorRef {
    /// The tenant the actor acted from, as the record has it; without one, no
    /// name is looked up.
    pub actor_tenant_id: Option<Uuid>,
    /// The actor as rendered: a user id, a module name or the mask token.
    pub actor: String,
    /// Whether the caller may not see the actor: it reads no name and is never looked up.
    pub masked: bool,
}

/// What a history record reads for its actor's name, as the resolver decides
/// it; [`ActorName::as_str`] spells it for the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActorName {
    /// The nil id, the platform's system context, unmasked: reads `"System"`.
    System,
    /// A user's current name, as Account Management gave it to this caller.
    User(String),
    /// No name, read as `null`: the actor is masked or withheld, is a module,
    /// or its name could not be had — no tenant recorded, unknown to Account
    /// Management, or the lookup was refused, failed or late.
    Absent,
}

impl ActorName {
    /// The name the record shows, or `None` for `null`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::System => Some(SYSTEM_NAME),
            Self::User(name) => Some(name),
            Self::Absent => None,
        }
    }
}

/// The names of a history page, read through AM for the caller.
pub struct ActorNameResolver {
    directory: Arc<dyn ActorDirectory>,
    metrics: Arc<dyn ActorNameMetrics>,
    cache: NameCache,
    /// How long a resolved name is reused; `None` keeps nothing.
    ttl: Option<Duration>,
}

impl ActorNameResolver {
    /// AM through the process client hub, keeping names for [`NAME_TTL`].
    #[must_use]
    pub fn from_hub(hub: Arc<ClientHub>, metrics: Arc<dyn ActorNameMetrics>) -> Self {
        Self::with_directory(Arc::new(AmDirectory { hub }), metrics).caching_for(NAME_TTL)
    }

    /// A given directory, for tests. It keeps no name until [`Self::caching_for`].
    #[must_use]
    pub fn with_directory(
        directory: Arc<dyn ActorDirectory>,
        metrics: Arc<dyn ActorNameMetrics>,
    ) -> Self {
        Self {
            directory,
            metrics,
            cache: NameCache::default(),
            ttl: None,
        }
    }

    /// These names, keeping each resolved name for `ttl` for its caller.
    #[must_use]
    pub fn caching_for(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// The name of each actor of a page, in input order, and report the pass once.
    ///
    /// An actor reads [`ActorName::Absent`] when it is masked, is not a user id, is
    /// not found, or its lookup was refused, failed or late; the nil id reads
    /// [`ActorName::System`] when unmasked.
    pub async fn names_for_page(
        &self,
        ctx: &SecurityContext,
        actors: &[ActorRef],
    ) -> Vec<ActorName> {
        let started = tokio::time::Instant::now();
        let (names, result) = self.lookup(ctx, actors).await;
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-16
        let out = actors
            .iter()
            .map(|a| {
                if a.masked {
                    ActorName::Absent
                } else {
                    match Uuid::parse_str(&a.actor) {
                        Ok(id) if id.is_nil() => ActorName::System,
                        Ok(id) => names
                            .get(&id)
                            .cloned()
                            .map_or(ActorName::Absent, ActorName::User),
                        Err(_) => ActorName::Absent,
                    }
                }
            })
            .collect();
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-16
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-14
        self.metrics.pass(result, started.elapsed());
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-14
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-15
        // The names return in the page's order, for the handler to assign.
        out
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-15
    }

    /// The resolved names by actor id, and the pass's result.
    async fn lookup(
        &self,
        ctx: &SecurityContext,
        actors_in: &[ActorRef],
    ) -> (BTreeMap<Uuid, String>, &'static str) {
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-1
        // An actor is asked for in the tenant it acted from. A record without
        // one, written before the tenant was recorded, reads `null`.
        let mut collected: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for record in actors_in.iter().filter(|r| !r.masked) {
            if let Some(tenant) = record.actor_tenant_id
                && let Ok(actor) = Uuid::parse_str(&record.actor)
                && !actor.is_nil()
            {
                collected.entry(tenant).or_default().insert(actor);
            }
        }
        let actors: BTreeSet<Uuid> = collected.values().flatten().copied().collect();
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-1
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-17
        if actors.is_empty() {
            return (BTreeMap::new(), "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-17
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-2
        if ctx.is_anonymous() {
            return (BTreeMap::new(), "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-2
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-3
        if !self.directory.is_available() {
            return (BTreeMap::new(), "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-3

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        let now = tokio::time::Instant::now();
        let caller = self
            .ttl
            .map(|ttl| ((ctx.subject_tenant_id(), ctx.subject_id()), ttl));
        let mut names = caller.map_or_else(BTreeMap::new, |(caller, _)| {
            self.cache.fresh(caller, &actors, now)
        });
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-5
        if names.len() == actors.len() {
            return (names, "ok");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-5

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-6
        let budget_end = now + LOOKUP_BUDGET;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-6

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-7
        let requests: Vec<(Uuid, Vec<Uuid>)> = collected
            .iter()
            .map(|(tenant, ids)| {
                let ids = ids.iter().filter(|id| !names.contains_key(id)).copied();
                (*tenant, ids.collect::<Vec<_>>())
            })
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        let fresh = self.resolve(ctx, &requests, budget_end).await;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-7
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
        if let Some((caller, ttl)) = caller {
            self.cache.keep(caller, &fresh.names, now, ttl);
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-19
        // Whatever AM did not answer is absent from the map and reads `null`.
        names.extend(fresh.names);
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-19
        (names, if fresh.degraded { "degraded" } else { "ok" })
    }

    /// Run the lookup: the requests in chunks, at most [`LOOKUP_CONCURRENCY`] at once.
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        requests: &[(Uuid, Vec<Uuid>)],
        budget_end: tokio::time::Instant,
    ) -> Outcome {
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-9
        let chunks: Vec<(Uuid, &[Uuid])> = requests
            .iter()
            .flat_map(|(tenant, ids)| ids.chunks(CHUNK_SIZE).map(|c| (*tenant, c)))
            .collect();
        // Boxed where its lifetimes are concrete, so a `Send` handler's future does not have to
        // prove the borrowing closure `Send` for every lifetime.
        let outcomes: Vec<Outcome> = stream::iter(chunks)
            .map(|(tenant, ids)| self.resolve_chunk(ctx, tenant, ids, budget_end))
            .buffer_unordered(LOOKUP_CONCURRENCY)
            .boxed()
            .collect()
            .await;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-9
        let mut all = Outcome::default();
        for outcome in outcomes {
            all.degraded |= outcome.degraded;
            all.names.extend(outcome.names);
        }
        all
    }

    /// One chunk: the whole filtered set is read before an id counts as absent. Every page
    /// must make progress and return only new requested ids; a provider that drifts never
    /// shows an unrelated profile. A chunk that degrades with ids left logs one warn line:
    /// the reason kind and the id count.
    async fn resolve_chunk(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
        budget_end: tokio::time::Instant,
    ) -> Outcome {
        let mut names = BTreeMap::new();
        let mut remaining: BTreeSet<Uuid> = ids.iter().copied().collect();
        let mut seen_cursors = BTreeSet::new();
        let mut cursor = None;
        let failure = loop {
            if tokio::time::Instant::now() >= budget_end {
                break Some(Degraded::Budget);
            }
            let Ok(mut query) = ListUsersQuery::with_ids(ids.iter().copied()) else {
                break Some(Degraded::Query);
            };
            let Ok(pagination) = IdpUserPagination::new(query.pagination.top(), cursor) else {
                break Some(Degraded::Query);
            };
            query.pagination = pagination;
            // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-10
            let result =
                tokio::time::timeout_at(budget_end, self.directory.list_users(ctx, tenant, query))
                    .await;
            // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-10
            let page = match result {
                Ok(Ok(page)) => page,
                // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-12
                // Refused or not found: AM's own answer, unresolved, not cached, not logged.
                Ok(Err(
                    CanonicalError::PermissionDenied { .. }
                    | CanonicalError::Unauthenticated { .. }
                    | CanonicalError::NotFound { .. },
                )) => break None,
                // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-12
                Ok(Err(_)) => break Some(Degraded::Directory),
                Err(_) => break Some(Degraded::Budget),
            };
            let returned: BTreeSet<Uuid> = page.items.iter().map(|u| u.id).collect();
            if returned.len() != page.items.len() || !returned.is_subset(&remaining) {
                break Some(Degraded::Pagination);
            }
            // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
            for user in page.items {
                remaining.remove(&user.id);
                if let Some(name) = project_name(&user) {
                    names.insert(user.id, name);
                }
            }
            // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
            let Some(next) = page.page_info.next_cursor else {
                break None;
            };
            if remaining.is_empty() {
                break None;
            }
            if returned.is_empty() || !seen_cursors.insert(next.clone()) {
                break Some(Degraded::Pagination);
            }
            cursor = Some(next);
        };
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-13
        let unresolved = ids.len() - names.len();
        if let Some(reason) = failure
            && unresolved > 0
        {
            tracing::warn!(
                reason = reason.as_str(),
                ids = unresolved,
                "actor names: a lookup degraded, these names read null"
            );
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-13
        Outcome {
            names,
            degraded: failure.is_some(),
        }
    }
}

/// The provider's own fields, in order: display name, then first and last name, then
/// username. A value of only whitespace counts as missing.
fn project_name(user: &IdpUser) -> Option<String> {
    fn non_blank(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    if let Some(name) = non_blank(user.display_name.as_deref()) {
        return Some(name.to_owned());
    }
    let full = [user.first_name.as_deref(), user.last_name.as_deref()]
        .into_iter()
        .filter_map(non_blank)
        .collect::<Vec<_>>()
        .join(" ");
    if !full.is_empty() {
        return Some(full);
    }
    non_blank(Some(user.username.as_str())).map(str::to_owned)
}

#[cfg(test)]
#[path = "actor_names_tests.rs"]
mod actor_names_tests;

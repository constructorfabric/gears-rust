// @cpt-dod:cpt-cf-settings-service-dod-audit-store-actor-names:p2
//! Actor names for the history read, through Account Management.
//!
//! A page's unmasked user-id actors are looked up once, each in the tenant its
//! record says the actor acted from, with the caller's own context: AM decides
//! what this caller may see and no privileged context is used. A name is kept
//! in this process for [`NAME_TTL`] for the caller AM gave it to and nobody
//! else. AM is a soft dependency: the client is found in the hub at each pass,
//! and a refused, absent, failed or late lookup reads no name and a status that
//! says why ([`ActorName`]); the history never fails because of it.

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

/// A user as a lookup asks for it: the actor, in the tenant it acted from. A
/// name and a failure both belong to the pair, never to the actor alone. A
/// type of its own, so the pair cannot be passed where a [`Caller`] is meant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Asked {
    /// The tenant the actor acted from, as its record has it.
    tenant: Uuid,
    /// The actor's user id.
    actor: Uuid,
}

/// A resolved name and the instant it stops being reused.
type Entry = (String, tokio::time::Instant);

/// Resolved names by caller and asked user. It holds names only: a lookup that
/// gave no name has nothing to keep here.
#[derive(Default)]
struct NameCache {
    entries: Mutex<HashMap<(Caller, Asked), Entry>>,
}

impl NameCache {
    /// The names of `asked` this caller was given and that are fresh at `now`.
    fn fresh(
        &self,
        caller: Caller,
        asked: &BTreeSet<Asked>,
        now: tokio::time::Instant,
    ) -> BTreeMap<Asked, String> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        asked
            .iter()
            .filter_map(|user| {
                entries
                    .get(&(caller, *user))
                    .filter(|(_, until)| *until > now)
                    .map(|(name, _)| (*user, name.clone()))
            })
            .collect()
    }

    /// Keep this caller's names until `now + ttl`; a full cache drops its expired
    /// names, then all.
    fn keep(
        &self,
        caller: Caller,
        names: &BTreeMap<Asked, String>,
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
        for (user, name) in names.iter().take(NAME_CACHE_CAPACITY) {
            entries.insert((caller, *user), (name.clone(), now + ttl));
        }
    }
}

/// Why a chunk's remaining ids stay unresolved; the pass counts its degraded
/// chunks by this in its warn line.
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

/// The degraded chunks of one pass, counted for its one warn line: how many
/// chunks degraded for each reason, how many ids they left without a name,
/// and the first failure of the directory among them.
#[derive(Default)]
struct DegradedChunks {
    budget: usize,
    directory: usize,
    pagination: usize,
    query: usize,
    ids: usize,
    first_directory_failure: Option<DirectoryFailure>,
}

impl DegradedChunks {
    /// Count one degraded chunk that left `ids` of its ids without a name,
    /// keeping its directory failure when it is the pass's first.
    fn count(&mut self, reason: Degraded, ids: usize, failure: Option<DirectoryFailure>) {
        match reason {
            Degraded::Budget => self.budget += 1,
            Degraded::Directory => self.directory += 1,
            Degraded::Pagination => self.pagination += 1,
            Degraded::Query => self.query += 1,
        }
        self.ids += ids;
        if self.first_directory_failure.is_none() {
            self.first_directory_failure = failure;
        }
    }
}

/// A chunk the directory failed: the tenant it was asked in, and the error
/// it answered with.
struct DirectoryFailure {
    tenant: Uuid,
    error: CanonicalError,
}

/// What a lookup learned of the users it asked for.
#[derive(Default)]
struct Outcome {
    /// The names it got.
    names: BTreeMap<Asked, String>,
    /// The users no finished lookup answered for: those a degraded chunk had
    /// not been answered when it stopped, or all of them without a directory.
    unanswered: BTreeSet<Asked>,
    /// Whether any chunk degraded.
    degraded: bool,
}

impl Outcome {
    /// What one asked user reads: its name; without one, `Unavailable` when no
    /// finished lookup answered for it and `Unknown` when one did.
    fn of(&self, user: Asked) -> ActorName {
        match self.names.get(&user) {
            Some(name) => ActorName::Resolved(name.clone()),
            None if self.unanswered.contains(&user) => ActorName::Unavailable,
            None => ActorName::Unknown,
        }
    }
}

/// What one chunk learned of the ids it asked for.
struct ChunkOutcome {
    /// The names it got.
    names: BTreeMap<Asked, String>,
    /// The ids it had not been answered for when it degraded; none when the
    /// directory answered or refused.
    unanswered: BTreeSet<Asked>,
    /// Why it degraded, if it did.
    failure: Option<Degraded>,
    /// The directory's error, when that is why it degraded.
    directory_failure: Option<DirectoryFailure>,
    /// How many of its ids read no name.
    unnamed: usize,
}

/// One actor of a history page, as the resolver needs it.
#[derive(Clone, Debug)]
pub struct ActorRef {
    /// The tenant the actor acted from, as the record has it; without one, no
    /// name is looked up.
    pub actor_tenant_id: Option<Uuid>,
    /// The actor as rendered: a user id, a module name or the mask token.
    pub actor: String,
    /// Whether the caller may not see the actor: it reads [`ActorName::Masked`] and is
    /// never looked up.
    pub masked: bool,
}

/// What a history record reads for its actor's name, as the resolver decides
/// it: [`ActorName::status`] says which case it is and [`ActorName::as_str`]
/// spells the name, which only `Resolved` and `System` have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActorName {
    /// A user's current name, as Account Management gave it to this caller.
    Resolved(String),
    /// The nil id, the platform's system context, unmasked: reads `"System"`.
    System,
    /// The caller may not see the actor, for the PII entitlement or the upward
    /// disclosure boundary. It is never looked up, whatever it is.
    Masked,
    /// The actor is not a user id: a module, whose name is the actor itself.
    Module,
    /// A user whose record names no tenant it acted from, so no tenant is asked.
    NotRecorded,
    /// No finished lookup answered for the user: no directory, a failed or late
    /// chunk, or one whose pages drifted. A later read may resolve it.
    Unavailable,
    /// A lookup answered and gave no name: Account Management refused the
    /// read, does not list the user for this caller or lists it without a
    /// name, or the context has no subject to ask as.
    Unknown,
}

impl ActorName {
    /// The name the record shows, if it has one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Resolved(name) => Some(name),
            Self::System => Some(SYSTEM_NAME),
            Self::Masked | Self::Module | Self::NotRecorded | Self::Unavailable | Self::Unknown => {
                None
            }
        }
    }

    /// The status as the record spells it.
    #[must_use]
    pub const fn status(&self) -> &'static str {
        match self {
            Self::Resolved(_) => "resolved",
            Self::System => "system",
            Self::Masked => "masked",
            Self::Module => "module",
            Self::NotRecorded => "not_recorded",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
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
    /// The mask decides first: a masked actor reads [`ActorName::Masked`] whatever
    /// it is. Then the actor's shape: the nil id reads [`ActorName::System`], a
    /// text that is no user id [`ActorName::Module`]. A user without a recorded
    /// tenant reads [`ActorName::NotRecorded`]; any other reads what the lookup
    /// learned of it in the tenant it acted from.
    pub async fn names_for_page(
        &self,
        ctx: &SecurityContext,
        actors: &[ActorRef],
    ) -> Vec<ActorName> {
        let started = tokio::time::Instant::now();
        let (looked, result) = self.lookup(ctx, actors).await;
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-16
        let out = actors
            .iter()
            .map(|a| {
                if a.masked {
                    ActorName::Masked
                } else {
                    match (Uuid::parse_str(&a.actor), a.actor_tenant_id) {
                        (Err(_), _) => ActorName::Module,
                        (Ok(id), _) if id.is_nil() => ActorName::System,
                        (Ok(_), None) => ActorName::NotRecorded,
                        (Ok(actor), Some(tenant)) => looked.of(Asked { tenant, actor }),
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

    /// What the pass learned of each user it asked for, and the pass's result.
    async fn lookup(
        &self,
        ctx: &SecurityContext,
        actors_in: &[ActorRef],
    ) -> (Outcome, &'static str) {
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-1
        // An actor is asked for in the tenant it acted from. A record without
        // one, written before the tenant was recorded, is asked nowhere.
        let mut asked: BTreeSet<Asked> = BTreeSet::new();
        for record in actors_in.iter().filter(|r| !r.masked) {
            if let Some(tenant) = record.actor_tenant_id
                && let Ok(actor) = Uuid::parse_str(&record.actor)
                && !actor.is_nil()
            {
                asked.insert(Asked { tenant, actor });
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-1
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-17
        if asked.is_empty() {
            return (Outcome::default(), "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-17
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-2
        // Nobody to ask as: the answer Account Management would give, final.
        if ctx.is_anonymous() {
            return (Outcome::default(), "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-2
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-3
        if !self.directory.is_available() {
            let unanswered = Outcome {
                names: BTreeMap::new(),
                unanswered: asked,
                degraded: false,
            };
            return (unanswered, "skipped");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-3

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        let now = tokio::time::Instant::now();
        let caller = self
            .ttl
            .map(|ttl| ((ctx.subject_tenant_id(), ctx.subject_id()), ttl));
        let names = caller.map_or_else(BTreeMap::new, |(caller, _)| {
            self.cache.fresh(caller, &asked, now)
        });
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-4
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-5
        if names.len() == asked.len() {
            let cached = Outcome {
                names,
                unanswered: BTreeSet::new(),
                degraded: false,
            };
            return (cached, "ok");
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-5

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-6
        let budget_end = now + LOOKUP_BUDGET;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-6

        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-7
        let mut requests: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
        for user in asked.iter().filter(|user| !names.contains_key(user)) {
            requests.entry(user.tenant).or_default().push(user.actor);
        }
        let requests: Vec<(Uuid, Vec<Uuid>)> = requests.into_iter().collect();
        let mut fresh = self.resolve(ctx, &requests, budget_end).await;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-7
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
        if let Some((caller, ttl)) = caller {
            self.cache.keep(caller, &fresh.names, now, ttl);
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-11
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-19
        // Whatever AM gave no name for is absent from the names, final: no
        // other tenant is asked.
        fresh.names.extend(names);
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-19
        let result = if fresh.degraded { "degraded" } else { "ok" };
        (fresh, result)
    }

    /// Run the lookup: the requests in chunks, at most [`LOOKUP_CONCURRENCY`] at once.
    ///
    /// The chunks that degrade with ids left are logged once for the pass, on one warn line:
    /// how many chunks degraded for each reason kind, the id count, and of the first chunk the
    /// directory failed, its tenant and the kind of error — the category, never the error's
    /// text, which may repeat the ids asked for. One line, not one per chunk: a page over a
    /// subtree asks a tenant per chunk, and a directory that is down fails them all.
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
        let outcomes: Vec<ChunkOutcome> = stream::iter(chunks)
            .map(|(tenant, ids)| self.resolve_chunk(ctx, tenant, ids, budget_end))
            .buffer_unordered(LOOKUP_CONCURRENCY)
            .boxed()
            .collect()
            .await;
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-9
        let mut all = Outcome::default();
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-13
        let mut degraded = DegradedChunks::default();
        for outcome in outcomes {
            if let Some(reason) = outcome.failure {
                all.degraded = true;
                if outcome.unnamed > 0 {
                    degraded.count(reason, outcome.unnamed, outcome.directory_failure);
                }
            }
            all.names.extend(outcome.names);
            all.unanswered.extend(outcome.unanswered);
        }
        if degraded.ids > 0 {
            let failed = degraded.first_directory_failure.as_ref();
            tracing::warn!(
                budget = degraded.budget,
                directory = degraded.directory,
                pagination = degraded.pagination,
                query = degraded.query,
                ids = degraded.ids,
                tenant = failed.map(|f| tracing::field::display(f.tenant)),
                error = failed.map(|f| f.error.title()),
                "actor names: lookups degraded, these actors read no name"
            );
        }
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-actor-names:p2:inst-as-names-13
        all
    }

    /// One chunk: the whole filtered set is read before an id counts as absent. Every page
    /// must make progress and return only new requested ids; a provider that drifts never
    /// shows an unrelated profile. A chunk that degrades says why and how many of its ids
    /// read no name, for the pass's warn line; the ids it had not been answered for are
    /// unanswered, while a chunk AM answered or refused leaves none.
    async fn resolve_chunk(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
        budget_end: tokio::time::Instant,
    ) -> ChunkOutcome {
        let mut names = BTreeMap::new();
        let mut remaining: BTreeSet<Uuid> = ids.iter().copied().collect();
        let mut seen_cursors = BTreeSet::new();
        let mut cursor = None;
        let mut directory_failure = None;
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
                Ok(Err(error)) => {
                    directory_failure = Some(DirectoryFailure { tenant, error });
                    break Some(Degraded::Directory);
                }
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
                    names.insert(
                        Asked {
                            tenant,
                            actor: user.id,
                        },
                        name,
                    );
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
        let unanswered = if failure.is_some() {
            remaining
                .into_iter()
                .map(|actor| Asked { tenant, actor })
                .collect()
        } else {
            BTreeSet::new()
        };
        let unnamed = ids.len() - names.len();
        ChunkOutcome {
            names,
            unanswered,
            failure,
            directory_failure,
            unnamed,
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

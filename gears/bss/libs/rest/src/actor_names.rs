//! Actor names through Account Management, for the BSS reads.
//!
//! A read collects the actor ids of its whole page or document and calls
//! [`ActorNames::resolve`] once, or [`ActorNames::fill`] over a response whose
//! types implement [`ActorFields`]. The names come from AM's public user read
//! (`AccountManagementClient::list_users` with an id-set filter), with the
//! caller's own context and tenant: AM decides what this caller may see, and no
//! privileged context is used.
//!
//! - Through [`ActorNames::from_hub`], a resolved name is kept in this process for `NAME_TTL`
//!   (five minutes), for the caller
//!   AM gave it to and for nobody else: the key is the caller's tenant and subject and the actor.
//!   A rename shows within that time. A refused, absent or failed lookup is not kept, so a new
//!   user's name shows on the next read, and nothing is kept for an anonymous caller. The cache
//!   holds at most `NAME_CACHE_CAPACITY` names; a full cache drops its expired names, then all,
//!   and one answer with more names than that keeps only that many.
//! - The ids are deduplicated and read in chunks of `IdpUserPagination::MAX_TOP`,
//!   at most four chunks at once, inside one 2 s budget for the response.
//! - A read never fails because of AM: a refused, absent, failed or late lookup
//!   becomes an [`ActorName`] that carries no label.
//! - A chunk whose names degrade to [`ActorName::Unavailable`] logs one warn line
//!   with the reason kind and the id count: no id, no profile and no error text.
//! - A gear's system actors read [`SYSTEM_LABEL`] and never reach AM.
//!
//! AM is a soft dependency. The client is looked up in the hub at each lookup,
//! so the registration order of the gears cannot turn names off, and a process
//! without AM reads every name as [`ActorName::Unavailable`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use account_management_sdk::{AccountManagementClient, IdpUserFilterField, IdpUserPagination};
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Page;
use toolkit_odata::filter::{FilterNode, ODataValue};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The SDK types of the [`ActorDirectory`] boundary, so that a gear's test fake needs no other
/// dependency.
pub use account_management_sdk::{IdpUser, ListUsersQuery};
/// The id type of [`actor_fields!`](crate::actor_fields)'s expansion.
#[doc(hidden)]
pub use uuid::Uuid as __Uuid;

/// The names [`ActorNames::resolve`] answers, by actor id.
pub type Names = BTreeMap<Uuid, ActorName>;

/// The most AM lookups one response runs at once.
const LOOKUP_CONCURRENCY: usize = 4;
/// The SDK's largest id-set chunk. Each chunk may follow the provider's pagination.
const LOOKUP_BATCH_SIZE: usize = IdpUserPagination::MAX_TOP as usize;
/// One budget for the whole response, queued lookups included.
const LOOKUP_BUDGET: Duration = Duration::from_secs(2);

/// How long a resolved name is reused for the caller it was resolved for.
const NAME_TTL: Duration = Duration::from_secs(300);
/// The most names one process keeps.
const NAME_CACHE_CAPACITY: usize = 10_000;

/// The label of a gear's system actor.
pub const SYSTEM_LABEL: &str = "System";

/// The caller a name was resolved for: its tenant and subject.
type Caller = (Uuid, Uuid);

/// A resolved name and the instant it stops being reused, by caller and actor.
type Entries = HashMap<(Caller, Uuid), (String, tokio::time::Instant)>;

/// Resolved names by caller and actor, each with the instant it stops being reused.
#[derive(Default)]
struct NameCache {
    entries: Mutex<Entries>,
}

impl NameCache {
    /// The names of `ids` this caller was given and that are still fresh at `now`.
    fn fresh(&self, caller: Caller, ids: &[Uuid], now: tokio::time::Instant) -> Names {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        ids.iter()
            .filter_map(|id| {
                entries
                    .get(&(caller, *id))
                    .filter(|(_, until)| *until > now)
                    .map(|(name, _)| (*id, ActorName::Resolved(name.clone())))
            })
            .collect()
    }

    /// Keep this caller's resolved names until `now + ttl`. Nothing else is kept, and the cache
    /// never holds more than `NAME_CACHE_CAPACITY` names.
    fn keep(&self, caller: Caller, names: &Names, now: tokio::time::Instant, ttl: Duration) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() + names.len() > NAME_CACHE_CAPACITY {
            entries.retain(|_, (_, until)| *until > now);
            if entries.len() + names.len() > NAME_CACHE_CAPACITY {
                entries.clear();
            }
        }
        // The names fit now, or the cache is empty and one answer holds more names than the
        // cache: then only the first `NAME_CACHE_CAPACITY` are kept.
        let resolved = names.iter().filter_map(|(id, name)| match name {
            ActorName::Resolved(label) => Some((*id, label)),
            _ => None,
        });
        for (id, label) in resolved.take(NAME_CACHE_CAPACITY) {
            entries.insert((caller, id), (label.clone(), now + ttl));
        }
    }
}

/// A current name, or the reason that no name can be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorName {
    /// The current non-blank display name, full name, or username from the `IdP`.
    Resolved(String),
    /// AM refused this caller access to the profile. No permission is added.
    Restricted,
    /// AM found no user visible to this caller. This does not prove a deletion.
    NotFound,
    /// No AM client, an AM or provider failure, the budget ran out, or the profile is invalid.
    Unavailable,
    /// One of the gear's own system actors. AM is not asked.
    System,
}

impl ActorName {
    /// The text a response shows: the resolved name, or [`SYSTEM_LABEL`] for a system actor.
    /// `None` when no name is available now.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Resolved(name) => Some(name),
            Self::System => Some(SYSTEM_LABEL),
            Self::Restricted | Self::NotFound | Self::Unavailable => None,
        }
    }
}

/// The narrow boundary to AM's public user read. Tests supply a fake.
#[async_trait]
pub trait ActorDirectory: Send + Sync {
    /// Read one filtered page in the caller's tenant, with the caller's rights.
    ///
    /// # Errors
    /// Returns AM's authorization, absence and provider errors unchanged.
    // cancel-safe: [`ActorNames::resolve`] drops this future at the response's deadline. An
    // implementation only reads, and keeps no state that a drop part way through could leave
    // half written.
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError>;
}

/// The ids an id-set lookup of [`ActorNames::resolve`] asks for, in ascending order. Empty for
/// a query without an id-set filter. A test's [`ActorDirectory`] reads its request with it.
#[must_use]
pub fn queried_ids(query: &ListUsersQuery) -> Vec<Uuid> {
    let Some(FilterNode::InList {
        field: IdpUserFilterField::Id,
        values,
    }) = &query.filter
    else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            ODataValue::Uuid(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// A response value that shows actor ids, each with a `*_name` sibling.
///
/// A gear implements it for its response types. The containers of the response
/// (`Vec`, `Option`) are implemented here, so a nested value delegates to them.
pub trait ActorFields {
    /// Push every actor id the value shows.
    fn actor_ids(&self, ids: &mut Vec<Uuid>);
    /// Set every `*_name` sibling from `names`, with [`label`].
    fn fill_names(&mut self, names: &Names);
}

impl<T: ActorFields> ActorFields for Vec<T> {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        for value in self {
            value.actor_ids(ids);
        }
    }
    fn fill_names(&mut self, names: &Names) {
        for value in self {
            value.fill_names(names);
        }
    }
}

impl<T: ActorFields> ActorFields for Option<T> {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        if let Some(value) = self {
            value.actor_ids(ids);
        }
    }
    fn fill_names(&mut self, names: &Names) {
        if let Some(value) = self {
            value.fill_names(names);
        }
    }
}

/// Implements [`ActorFields`] for a response type: each actor id field (a `Uuid`) with its
/// `*_name` sibling, then the nested values (`Vec`, `Option` or another [`ActorFields`] type) that
/// carry their own.
///
/// ```ignore
/// bss_rest::actor_fields!(PlanDto { created_by => created_by_name } [revisions, current]);
/// ```
#[macro_export]
macro_rules! actor_fields {
    ($type:ty { $($id:ident => $name:ident),* } [$($nested:ident),*]) => {
        impl $crate::actor_names::ActorFields for $type {
            fn actor_ids(&self, ids: &mut ::std::vec::Vec<$crate::actor_names::__Uuid>) {
                $(ids.push(self.$id);)*
                $($crate::actor_names::ActorFields::actor_ids(&self.$nested, ids);)*
            }
            fn fill_names(&mut self, names: &$crate::actor_names::Names) {
                $(self.$name = $crate::actor_names::label(names, self.$id);)*
                $($crate::actor_names::ActorFields::fill_names(&mut self.$nested, names);)*
            }
        }
    };
}

/// The `*_name` a response shows for `id`: its label among `names`, or `None`.
#[must_use]
pub fn label(names: &Names, id: Uuid) -> Option<String> {
    names.get(&id).and_then(ActorName::label).map(str::to_owned)
}

/// AM through the client hub, looked up at each call.
struct AmDirectory {
    hub: Arc<ClientHub>,
}

#[async_trait]
impl ActorDirectory for AmDirectory {
    // cancel-safe: one read through AM's public user read, with no side effect here; a drop at the
    // deadline leaves nothing behind.
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, CanonicalError> {
        let am = self
            .hub
            .try_get::<dyn AccountManagementClient>()
            .ok_or_else(|| CanonicalError::service_unavailable().create())?;
        am.list_users(ctx, ctx.subject_tenant_id(), query).await
    }
}

/// Read-side names only. They are not part of a transaction, a pin, an audit row
/// or a stored record.
#[derive(Clone)]
pub struct ActorNames {
    directory: Arc<dyn ActorDirectory>,
    system_ids: BTreeSet<Uuid>,
    /// Shared by every clone, so [`Self::with_system_ids`] keeps the gear's cache.
    cache: Arc<NameCache>,
    /// How long a resolved name is reused; `None` keeps nothing.
    ttl: Option<Duration>,
}

/// The system ids; the directory is a trait object and is not shown.
impl std::fmt::Debug for ActorNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorNames")
            .field("system_ids", &self.system_ids)
            .finish_non_exhaustive()
    }
}

/// How one chunk's lookups ended for the ids it did not name.
enum Stop {
    /// AM refused this caller the profiles.
    Restricted,
    /// AM read the whole filtered set and did not find them.
    NotFound,
    /// No answer that can be trusted: the reason kind of the chunk's warn line.
    Degraded(Degraded),
}

/// Why a chunk's remaining ids read [`ActorName::Unavailable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Degraded {
    /// The response's budget ran out before or during a lookup.
    Budget,
    /// The directory failed, or no AM client is registered.
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

impl ActorNames {
    /// AM through the process client hub. `system_ids` are the gear's own system actors.
    #[must_use]
    pub fn from_hub(hub: Arc<ClientHub>, system_ids: &[Uuid]) -> Self {
        Self::with_directory(Arc::new(AmDirectory { hub }), system_ids).caching_for(NAME_TTL)
    }

    /// A given directory, for tests and transport-level fakes. It keeps no name: every read asks
    /// the directory, unless [`Self::caching_for`] turns the cache on.
    #[must_use]
    pub fn with_directory(directory: Arc<dyn ActorDirectory>, system_ids: &[Uuid]) -> Self {
        Self {
            directory,
            system_ids: system_ids.iter().copied().collect(),
            cache: Arc::new(NameCache::default()),
            ttl: None,
        }
    }

    /// These names, keeping each resolved name for `ttl` for the caller it was resolved for.
    /// [`Self::from_hub`] keeps them for `NAME_TTL`.
    #[must_use]
    pub fn caching_for(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// These names with `more` system actors beside the gear's own: a facade adds the system
    /// actors its sources declare, for one answer.
    #[must_use]
    pub fn with_system_ids(&self, more: impl IntoIterator<Item = Uuid>) -> Self {
        let mut names = self.clone();
        names.system_ids.extend(more);
        names
    }

    /// The name of every id, in one bounded set of lookups.
    ///
    /// The caller has already authorized and loaded the records the ids come
    /// from. A system id reads [`ActorName::System`] and is not sent to AM. No
    /// queued chunk starts after the shared deadline, and a lookup still running
    /// at the deadline is cancelled. No profile or error detail is logged.
    pub async fn resolve(
        &self,
        ctx: &SecurityContext,
        ids: impl IntoIterator<Item = Uuid>,
    ) -> Names {
        let (system, unique): (BTreeSet<_>, BTreeSet<_>) =
            ids.into_iter().partition(|id| self.system_ids.contains(id));
        let unique: Vec<_> = unique.into_iter().collect();
        let now = tokio::time::Instant::now();
        // A name is reused only for the caller AM gave it to; an anonymous caller keeps nothing.
        let caller = self
            .ttl
            .filter(|_| !ctx.is_anonymous())
            .map(|ttl| ((ctx.subject_tenant_id(), ctx.subject_id()), ttl));
        let cached = caller.map_or_else(Names::new, |(caller, _)| {
            self.cache.fresh(caller, &unique, now)
        });
        let unique: Vec<_> = unique
            .into_iter()
            .filter(|id| !cached.contains_key(id))
            .collect();
        let deadline = now + LOOKUP_BUDGET;
        // Boxed where its lifetimes are concrete, so a `Send` handler's future does not have to
        // prove the borrowing closure `Send` for every lifetime.
        let mut names: BTreeMap<_, _> = stream::iter(unique.chunks(LOOKUP_BATCH_SIZE))
            .map(|ids| self.resolve_chunk(ctx, ids, deadline))
            .buffer_unordered(LOOKUP_CONCURRENCY)
            .flat_map(stream::iter)
            .boxed()
            .collect()
            .await;
        if let Some((caller, ttl)) = caller {
            self.cache.keep(caller, &names, now, ttl);
        }
        names.extend(cached);
        names.extend(system.into_iter().map(|id| (id, ActorName::System)));
        names
    }

    /// Collect every actor id of `value`, [`Self::resolve`] them once, and set every `*_name`.
    ///
    /// A value without ids makes no lookup. A name that cannot be shown is `None`.
    pub async fn fill<T: ActorFields + ?Sized>(&self, ctx: &SecurityContext, value: &mut T) {
        let mut ids = Vec::new();
        value.actor_ids(&mut ids);
        let names = if ids.is_empty() {
            BTreeMap::new()
        } else {
            self.resolve(ctx, ids).await
        };
        value.fill_names(&names);
    }

    /// Read the whole filtered set before claiming that an id is absent. Every
    /// page must make progress and return only new requested ids. A provider that
    /// drifts or fails in pagination never shows an unrelated profile and never
    /// implies a deletion. A chunk that ends degraded with ids left logs one warn
    /// line: the reason kind and the id count.
    async fn resolve_chunk(
        &self,
        ctx: &SecurityContext,
        ids: &[Uuid],
        deadline: tokio::time::Instant,
    ) -> Names {
        let mut names = BTreeMap::new();
        let mut remaining: BTreeSet<_> = ids.iter().copied().collect();
        let mut seen_cursors = BTreeSet::new();
        let mut cursor = None;
        let stop = loop {
            if tokio::time::Instant::now() >= deadline {
                break Stop::Degraded(Degraded::Budget);
            }
            let Ok(mut query) = ListUsersQuery::with_ids(ids.iter().copied()) else {
                break Stop::Degraded(Degraded::Query);
            };
            let Ok(pagination) = IdpUserPagination::new(query.pagination.top(), cursor) else {
                break Stop::Degraded(Degraded::Query);
            };
            query.pagination = pagination;
            let result =
                tokio::time::timeout_at(deadline, self.directory.list_users(ctx, query)).await;
            let page = match result {
                Ok(Ok(page)) => page,
                Ok(Err(
                    CanonicalError::PermissionDenied { .. }
                    | CanonicalError::Unauthenticated { .. },
                )) => break Stop::Restricted,
                Ok(Err(CanonicalError::NotFound { .. })) => break Stop::NotFound,
                Ok(Err(_)) => break Stop::Degraded(Degraded::Directory),
                Err(_) => break Stop::Degraded(Degraded::Budget),
            };
            let returned: BTreeSet<_> = page.items.iter().map(|user| user.id).collect();
            if returned.len() != page.items.len() || !returned.is_subset(&remaining) {
                break Stop::Degraded(Degraded::Pagination);
            }
            for user in page.items {
                remaining.remove(&user.id);
                names.insert(user.id, project_name(&user));
            }
            let Some(next) = page.page_info.next_cursor else {
                break Stop::NotFound;
            };
            if remaining.is_empty() {
                break Stop::NotFound;
            }
            if returned.is_empty() || !seen_cursors.insert(next.clone()) {
                break Stop::Degraded(Degraded::Pagination);
            }
            cursor = Some(next);
        };
        let failure = match stop {
            Stop::Restricted => ActorName::Restricted,
            Stop::NotFound => ActorName::NotFound,
            Stop::Degraded(reason) => {
                if !remaining.is_empty() {
                    tracing::warn!(
                        reason = reason.as_str(),
                        ids = remaining.len(),
                        "actor names: a lookup degraded, these names read null"
                    );
                }
                ActorName::Unavailable
            }
        };
        names.extend(remaining.into_iter().map(|id| (id, failure.clone())));
        names
    }
}

/// The provider's own fields, in order: display name, then first and last name,
/// then username. A value of only whitespace counts as missing.
fn project_name(user: &IdpUser) -> ActorName {
    if let Some(name) = user
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return ActorName::Resolved(name.to_owned());
    }
    let full_name = [user.first_name.as_deref(), user.last_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !full_name.is_empty() {
        return ActorName::Resolved(full_name);
    }
    let username = user.username.trim();
    if username.is_empty() {
        ActorName::Unavailable
    } else {
        ActorName::Resolved(username.to_owned())
    }
}

#[cfg(test)]
#[path = "actor_names_tests.rs"]
mod tests;

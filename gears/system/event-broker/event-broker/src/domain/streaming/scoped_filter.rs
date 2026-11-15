//! Delivery's consume decision for each event, read from the member's
//! consume scope instead of asked of the PDP per event.
//!
//! An event is delivered when some interest on its topic selects it, passes
//! the interest's ownership narrowing, and that interest's plan for the
//! event's type admits it. A
//! plan comes from the interest group's stored scope with no PDP call when
//! the type is one of the group's candidates, and otherwise from one
//! single-type evaluation per type per scope generation of the group. A read
//! batch asks its evaluations in one wave before any event of it is decided;
//! a batch that needs more than one wave is cut before the first event past
//! it, and the session reads the rest again.
//!
//! A new scope generation clears the plans of every group whose scope,
//! candidates or expiry changed, so an `Admit` never outlives a revocation or
//! a refresh.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use gts::{GtsIdPattern, GtsTypeId};
use tokio::task::JoinSet;
use toolkit::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument;
use uuid::Uuid;

use crate::domain::consumer_group_coordinator::member_scope::CachedScope;
use crate::domain::error::DomainError;
use crate::domain::join_scope::{ConsumeCall, GroupKey};
use crate::domain::model::{Event, Interest, OwnershipFilter};
use crate::domain::specification::SpecificationManager;
use crate::domain::streaming::filter::{CompiledInterest, EventFilter, Prepared};
use crate::domain::streaming::read::EventBatch;
use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner, TypePlan};

/// The most single-type evaluations one read batch asks: one wave, all at
/// once under the authorizer's one deadline, so a batch waits at most once.
/// A batch needing more is cut before the first event of a type past the
/// wave; only its first event may need more, and those run in further rounds
/// of at most this many, each round under its own deadline. A batch fails when
/// an evaluation fails or outlasts its round's deadline; its finished answers
/// are then dropped with it.
///
/// Per stream, at most half the gear database pool's default size. Each
/// evaluation holds a pool connection only for its one indexed event-type
/// read, released before its PDP call, so streams with new types queue on the
/// pool as publish's own event-type reads do. No bound is shared across
/// streams: under each batch's deadline, a shared one would turn load from
/// every stream into deadline misses that end the waiting streams together.
pub const SINGLE_TYPE_CONCURRENCY: usize = 5;

/// The most plans one interest keeps per scope generation of its group: room
/// for every candidate a group lists (at most 256) and every single-type
/// result it keeps ([`SINGLE_CAP`]). A type past it is decided without being
/// kept.
pub const PLAN_CAP: usize = 1_280;

/// The most single-type results one group keeps per scope generation, so a
/// stream reading ever new unlisted types holds bounded memory; a new one
/// past it evicts the oldest.
pub const SINGLE_CAP: usize = 1_024;

/// The most (event type, subject type) selector results one interest keeps,
/// so a stream over ever new subject types holds bounded memory. A pair past
/// it is matched without being kept.
pub const SELECTOR_CACHE_CAP: usize = 1_024;

/// An [`EventFilter`] that decides each event from the member's consume
/// scope.
///
/// Its state sits behind one lock, held for lookups and stores only, never
/// across an await: [`EventFilter::prepare`] collects what it must ask under
/// the lock, releases it, asks, and takes it again to store the answers.
#[domain_model]
pub struct ScopedFilter {
    interests: Box<[ScopedInterest]>,
    /// The indexes into `interests` of each topic's interests, keyed by the
    /// topic's string form, which is what an event carries.
    by_topic: HashMap<String, Vec<usize>>,
    /// The indexes into `interests` of each group's interests.
    group_interests: Box<[Vec<usize>]>,
    /// Each group's event-type patterns, sorted and deduplicated, as its E
    /// names them.
    group_patterns: Box<[Box<[String]>]>,
    authorizer: Arc<SingleTypeAuthorizer>,
    state: Mutex<FilterState>,
}

/// Asks the PDP about one event type outside a group's candidates.
///
/// One per stream, holding the stream caller and the delivery service's
/// dependencies.
#[domain_model]
pub struct SingleTypeAuthorizer {
    enforcer: PolicyEnforcer,
    specs: Arc<dyn SpecificationManager>,
    interner: Arc<TenantSetInterner>,
    ctx: SecurityContext,
    /// The deadline each round of a batch's single-type evaluations shares.
    deadline: Duration,
    /// The `x-request-id` of the request that opened the stream, which each
    /// evaluation runs under.
    request_id: Option<String>,
    /// The span the stream was opened in, which each evaluation runs in.
    span: tracing::Span,
}

/// One interest and the group it belongs to.
#[derive(Debug)]
struct ScopedInterest {
    compiled: CompiledInterest,
    narrowing: Narrowing,
    /// The index of the interest's group in the scope's groups.
    group: usize,
}

/// Which events an interest's ownership filter lets through, checked by the
/// broker itself whatever the scope admits.
#[derive(Debug, Clone, Copy)]
enum Narrowing {
    /// `only_mine`: events of the caller's own tenant that the caller owns.
    /// The tenant is compared here too, so a scope that names an owner and no
    /// tenant cannot deliver another tenant's events addressed to the caller.
    Mailbox { caller_tenant: Uuid, caller: Uuid },
    /// `entire_tenant`: every event of the interest's tenant.
    Tenant(Uuid),
    /// `tenant_subtree_included`: whatever the subtree scope admits.
    Any,
}

/// Everything [`ScopedFilter`] changes as it decides, under its lock.
struct FilterState {
    scope: Arc<CachedScope>,
    /// Per interest: its selector results.
    selectors: Vec<SelectorCache>,
    /// Per interest: the plan of each event type seen in this generation of
    /// the interest's group.
    plans: Vec<HashMap<GtsTypeId, TypePlan>>,
    /// Per group: the single-type results of this generation.
    single: Vec<HashMap<GtsTypeId, TypePlan>>,
    /// Per group: the types of `single`, oldest first.
    single_order: Vec<VecDeque<GtsTypeId>>,
    /// Per group: every single-type decision the batch last prepared uses,
    /// asked or already held, kept until the next `prepare` so an eviction
    /// from `single` within the batch cannot leave one of its events
    /// undecided.
    prepared: Vec<HashMap<GtsTypeId, TypePlan>>,
    /// Per group: whether the `SINGLE_CAP` warning was logged in this
    /// generation.
    single_cap_warned: Vec<bool>,
    /// Per interest: whether the `PLAN_CAP` warning was logged in this
    /// generation of its group.
    plan_cap_warned: Vec<bool>,
    /// Set when a scope arrived that the filter must not decide from; every
    /// later batch fails.
    unusable: Option<UnusableScope>,
}

/// Why a scope the session handed over cannot decide any event.
#[derive(Debug, Clone, Copy)]
enum UnusableScope {
    /// Its groups do not line up with the interests.
    Misaligned,
    /// It was stored for another caller than the stream's.
    OtherPrincipal,
}

/// One interest's selector results, by event type then subject type, so a
/// hit is two lookups by reference with no parse and no allocation.
#[derive(Default)]
struct SelectorCache {
    by_type: HashMap<GtsTypeId, HashMap<GtsTypeId, bool>>,
    len: usize,
}

/// One single-type evaluation: a group's type outside its candidates.
#[derive(Debug, Clone)]
struct SingleTypeAsk {
    group: usize,
    key: GroupKey,
    patterns: Box<[String]>,
    event_type: GtsTypeId,
}

/// What one single-type evaluation decided.
#[derive(Debug)]
enum SingleTypeAnswer {
    /// The PDP allowed: the plan its scope gives the type.
    Planned(TypePlan),
    /// The type is denied until the group's next generation.
    Refused(Refusal),
}

/// Why a single-type evaluation denies its type.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    Denied,
    CompileFailed,
    NotEnforceable,
}

impl ScopedFilter {
    /// Compiles `interests` against `scope`, the member's consume scope,
    /// whose groups are the groups the interests form.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] only on a broken internal invariant: an
    /// interest whose group `scope` does not hold.
    pub fn new(
        interests: &[Interest],
        scope: Arc<CachedScope>,
        authorizer: SingleTypeAuthorizer,
    ) -> Result<Self, DomainError> {
        let groups = scope.groups.len();
        let mut scoped = Vec::with_capacity(interests.len());
        let mut by_topic: HashMap<String, Vec<usize>> = HashMap::new();
        let mut group_interests = vec![Vec::new(); groups];
        let mut group_patterns = vec![BTreeSet::new(); groups];
        for (index, interest) in interests.iter().enumerate() {
            let key = GroupKey::new(interest, authorizer.ctx.subject_tenant_id());
            let group = scope
                .groups
                .iter()
                .position(|held| held.key == key)
                .ok_or_else(|| {
                    DomainError::Internal(
                        "the consume scope holds no group for an interest".to_owned(),
                    )
                })?;
            by_topic
                .entry(interest.topic.as_ref().to_owned())
                .or_default()
                .push(index);
            group_interests[group].push(index);
            group_patterns[group].extend(interest.types.iter().map(GtsIdPattern::pattern));
            scoped.push(ScopedInterest {
                compiled: CompiledInterest::new(interest),
                narrowing: Narrowing::new(interest, &authorizer.ctx),
                group,
            });
        }
        let group_patterns = group_patterns
            .into_iter()
            .map(|patterns| patterns.into_iter().map(str::to_owned).collect())
            .collect();
        let state = FilterState {
            scope,
            selectors: (0..interests.len())
                .map(|_| SelectorCache::default())
                .collect(),
            plans: vec![HashMap::new(); interests.len()],
            single: vec![HashMap::new(); groups],
            single_order: vec![VecDeque::new(); groups],
            prepared: vec![HashMap::new(); groups],
            single_cap_warned: vec![false; groups],
            plan_cap_warned: vec![false; interests.len()],
            unusable: None,
        };
        Ok(Self {
            interests: scoped.into(),
            by_topic,
            group_interests: group_interests.into(),
            group_patterns,
            authorizer: Arc::new(authorizer),
            state: Mutex::new(state),
        })
    }

    /// How many selector results interest `interest` keeps.
    #[cfg(test)]
    pub(crate) fn selector_count(&self, interest: usize) -> usize {
        self.state().selectors[interest].len
    }

    /// How many plans interest `interest` keeps.
    #[cfg(test)]
    pub(crate) fn plan_count(&self, interest: usize) -> usize {
        self.state().plans[interest].len()
    }

    /// How many single-type results group `group` keeps.
    #[cfg(test)]
    pub(crate) fn single_count(&self, group: usize) -> usize {
        self.state().single[group].len()
    }

    /// The indexes of the interests on `event`'s topic, in index order.
    fn on_topic(&self, event: &Event) -> &[usize] {
        self.by_topic
            .get(event.topic.as_ref())
            .map_or(&[], Vec::as_slice)
    }

    /// Whether interest `index`, one on `event`'s topic, selects `event`:
    /// its ownership narrowing lets the event through and its selector
    /// matches.
    fn selects(&self, state: &mut FilterState, index: usize, event: &Event) -> bool {
        let interest = &self.interests[index];
        interest.narrowing.holds(event) && state.selectors[index].selects(&interest.compiled, event)
    }

    /// Whether interest `index`'s plan for `event`'s type admits `event`,
    /// making and keeping the plan when it has none yet.
    fn admits_through(
        &self,
        state: &mut FilterState,
        index: usize,
        event: &Event,
    ) -> Result<bool, DomainError> {
        if let Some(plan) = state.plans[index].get(&event.r#type) {
            return Ok(plan.admits(event));
        }
        let group = self.interests[index].group;
        let held = &state.scope.groups[group];
        let plan = if held.candidates.contains(&event.r#type) {
            held.scope.plan_for(&event.r#type)
        } else if let Some(plan) = state.single[group].get(&event.r#type) {
            plan.clone()
        } else {
            // Evicted from `single` while this batch's answers were stored:
            // it decides the batch's events, and is not kept, so the type is
            // asked again later.
            return state.prepared[group]
                .get(&event.r#type)
                .map(|plan| plan.admits(event))
                .ok_or_else(|| {
                    DomainError::Internal(
                        "no consume decision was prepared for an event type".to_owned(),
                    )
                });
        };
        let admits = plan.admits(event);
        if state.plans[index].len() < PLAN_CAP {
            state.plans[index].insert(event.r#type.clone(), plan);
        } else if !std::mem::replace(&mut state.plan_cap_warned[index], true) {
            tracing::warn!(
                topic = %event.topic,
                cap = PLAN_CAP,
                "an interest holds the most event-type plans; further types are decided without being kept"
            );
        }
        Ok(admits)
    }

    /// The single-type evaluations `events` need, and how many leading
    /// events they make decidable: all of them, or fewer when the batch needs
    /// more than one wave.
    ///
    /// Each (group, type) pair of a selected event whose type is not a
    /// candidate of the group is asked once, in the order the events show
    /// them, unless it is decided already; a decided one is copied into
    /// `prepared`. The batch is cut at the first event that needs a pair past
    /// [`SINGLE_TYPE_CONCURRENCY`], but never before its first event: every
    /// subtree group on a topic can select one event, so the first event may
    /// need more pairs than a wave holds, and a cut before it would leave
    /// every pass deciding nothing.
    fn missing(&self, state: &mut FilterState, events: &EventBatch) -> (Vec<SingleTypeAsk>, usize) {
        let mut asks: Vec<SingleTypeAsk> = Vec::new();
        for (position, event) in events.iter().enumerate() {
            for &index in self.on_topic(event) {
                if !self.selects(state, index, event) {
                    continue;
                }
                let group = self.interests[index].group;
                let held = &state.scope.groups[group];
                if held.candidates.contains(&event.r#type)
                    || state.prepared[group].contains_key(&event.r#type)
                {
                    continue;
                }
                let decided = state.plans[index]
                    .get(&event.r#type)
                    .or_else(|| state.single[group].get(&event.r#type));
                if let Some(plan) = decided {
                    state.prepared[group].insert(event.r#type.clone(), plan.clone());
                    continue;
                }
                if asks
                    .iter()
                    .any(|ask| ask.group == group && ask.event_type == event.r#type)
                {
                    continue;
                }
                if position > 0 && asks.len() >= SINGLE_TYPE_CONCURRENCY {
                    return (asks, position);
                }
                asks.push(SingleTypeAsk {
                    group,
                    key: held.key.clone(),
                    patterns: self.group_patterns[group].clone(),
                    event_type: event.r#type.clone(),
                });
            }
        }
        (asks, events.len())
    }

    /// Keeps the answers of one batch's single-type evaluations.
    fn store(&self, state: &mut FilterState, answers: Vec<(SingleTypeAsk, SingleTypeAnswer)>) {
        for (ask, answer) in answers {
            let plan = match answer {
                SingleTypeAnswer::Planned(plan) => plan,
                SingleTypeAnswer::Refused(refusal) => {
                    tracing::warn!(
                        topic = %ask.key.topic,
                        event_type = %ask.event_type,
                        reason = ?refusal,
                        "the consume evaluation of an event type refused it; its events are skipped until the scope changes"
                    );
                    TypePlan::Deny
                }
            };
            let group = ask.group;
            if state.single[group].len() >= SINGLE_CAP {
                self.evict_oldest(state, group);
            }
            state.single[group].insert(ask.event_type.clone(), plan.clone());
            state.single_order[group].push_back(ask.event_type.clone());
            state.prepared[group].insert(ask.event_type, plan);
        }
    }

    /// Drops group `group`'s oldest single-type result, and every interest
    /// plan copied from it, so the type is evaluated again on its next event.
    fn evict_oldest(&self, state: &mut FilterState, group: usize) {
        let Some(oldest) = state.single_order[group].pop_front() else {
            return;
        };
        state.single[group].remove(&oldest);
        for &index in &self.group_interests[group] {
            state.plans[index].remove(&oldest);
        }
        if !std::mem::replace(&mut state.single_cap_warned[group], true) {
            tracing::warn!(
                topic = %state.scope.groups[group].key.topic,
                cap = SINGLE_CAP,
                "an interest group holds the most single-type decisions; the oldest are evicted"
            );
        }
    }

    /// Forgets every decision of group `group`, for its next generation.
    fn clear_group(&self, state: &mut FilterState, group: usize) {
        state.single[group].clear();
        state.single_order[group].clear();
        state.prepared[group].clear();
        state.single_cap_warned[group] = false;
        for &index in &self.group_interests[group] {
            state.plans[index].clear();
            state.plan_cap_warned[index] = false;
        }
    }

    /// Recovers a poisoned guard instead of panicking again. A panic under
    /// the lock unwinds through the session that owns the filter, so no
    /// later decision reads a half-written state.
    fn state(&self) -> MutexGuard<'_, FilterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl EventFilter for ScopedFilter {
    fn matches(&self, event: &Event) -> bool {
        let mut state = self.state();
        self.on_topic(event)
            .iter()
            .any(|&index| self.selects(&mut state, index, event))
    }

    /// Runs the batch's single-type evaluations in one wave, at most
    /// [`SINGLE_TYPE_CONCURRENCY`] of them at once under one deadline (more
    /// only for the batch's first event, in further rounds of that many, each
    /// under its own deadline); candidate types need none. A batch needing
    /// more is cut: [`Prepared::UpTo`] the first event of a type past the
    /// wave, which a later read decides.
    ///
    /// # Errors
    ///
    /// What [`SingleTypeAuthorizer`] fails with: a failed event-type read, a
    /// failed evaluation, or the deadline. [`DomainError::Internal`] on a
    /// broken internal invariant: the member's scope changed while the
    /// evaluations ran, or a scope arrived whose groups do not line up with
    /// the interests or that was stored for another caller.
    async fn prepare(&self, events: &EventBatch) -> Result<Prepared, DomainError> {
        let (asks, decidable, generation) = {
            let mut state = self.state();
            if let Some(unusable) = state.unusable {
                return Err(unusable.into());
            }
            for prepared in &mut state.prepared {
                prepared.clear();
            }
            let (asks, decidable) = self.missing(&mut state, events);
            (asks, decidable, state.scope.generation)
        };
        let prepared = if decidable < events.len() {
            Prepared::UpTo(decidable)
        } else {
            Prepared::All
        };
        if asks.is_empty() {
            return Ok(prepared);
        }
        let answers = Arc::clone(&self.authorizer).evaluate_all(asks).await?;
        let mut state = self.state();
        // Answers to the questions of one generation never decide events
        // under another.
        if state.scope.generation != generation {
            return Err(DomainError::Internal(
                "the consume scope changed during a single-type evaluation".to_owned(),
            ));
        }
        self.store(&mut state, answers);
        Ok(prepared)
    }

    /// Delivered when some interest on the event's topic selects it, its
    /// ownership narrowing lets it through, and its plan for the event's type
    /// admits it.
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] only on a broken internal invariant: a
    /// type outside a group's candidates with no single-type answer, which
    /// `prepare` makes for every event it makes decidable, or a scope that
    /// `prepare` refuses.
    fn admits(&self, event: &Event) -> Result<bool, DomainError> {
        let mut state = self.state();
        if let Some(unusable) = state.unusable {
            return Err(unusable.into());
        }
        for &index in self.on_topic(event) {
            if self.selects(&mut state, index, event)
                && self.admits_through(&mut state, index, event)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Takes `scope` when it was stored for another caller or its generation
    /// differs from the one held. A group is unchanged, and keeps its
    /// decisions, only when its scope and candidates are the very `Arc`s held
    /// and its expiry is the same; any other group starts over, so a group
    /// re-evaluated into a new window asks again. A scope stored for another
    /// caller, even under the generation held, or whose groups do not line up
    /// with the interests, fails every later batch.
    fn swap_scope(&self, scope: Arc<CachedScope>) {
        let mut state = self.state();
        if scope.principal != state.scope.principal {
            state.unusable = Some(UnusableScope::OtherPrincipal);
            state.scope = scope;
            return;
        }
        if scope.generation == state.scope.generation {
            return;
        }
        let aligned = scope.groups.len() == state.scope.groups.len()
            && scope
                .groups
                .iter()
                .zip(state.scope.groups.iter())
                .all(|(new, held)| new.key == held.key);
        if !aligned {
            state.unusable = Some(UnusableScope::Misaligned);
            state.scope = scope;
            return;
        }
        for group in 0..scope.groups.len() {
            let (new, held) = (&scope.groups[group], &state.scope.groups[group]);
            let unchanged = Arc::ptr_eq(&new.scope, &held.scope)
                && Arc::ptr_eq(&new.candidates, &held.candidates)
                && new.expires_at == held.expires_at;
            if !unchanged {
                self.clear_group(&mut state, group);
            }
        }
        state.scope = scope;
    }
}

impl From<UnusableScope> for DomainError {
    fn from(unusable: UnusableScope) -> Self {
        Self::Internal(
            match unusable {
                UnusableScope::Misaligned => {
                    "the consume scope's groups do not match the subscription's interests"
                }
                UnusableScope::OtherPrincipal => "the consume scope belongs to another caller",
            }
            .to_owned(),
        )
    }
}

impl std::fmt::Debug for ScopedFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedFilter")
            .field("interests", &self.interests)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SingleTypeAuthorizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleTypeAuthorizer")
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl Narrowing {
    /// The narrowing of `interest` for the stream caller `ctx`.
    fn new(interest: &Interest, ctx: &SecurityContext) -> Self {
        match interest.ownership {
            OwnershipFilter::OnlyMine => Self::Mailbox {
                caller_tenant: ctx.subject_tenant_id(),
                caller: ctx.subject_id(),
            },
            OwnershipFilter::EntireTenant => Self::Tenant(interest.subject_tenant),
            OwnershipFilter::TenantSubtreeIncluded => Self::Any,
        }
    }

    /// No allocation and no await.
    fn holds(self, event: &Event) -> bool {
        match self {
            Self::Mailbox {
                caller_tenant,
                caller,
            } => event.subject_tenant == caller_tenant && event.subject_owner == Some(caller),
            Self::Tenant(tenant) => event.subject_tenant == tenant,
            Self::Any => true,
        }
    }
}

impl SelectorCache {
    /// Whether `interest`'s selector matches `event`, kept while the cache
    /// has room.
    fn selects(&mut self, interest: &CompiledInterest, event: &Event) -> bool {
        if let Some(&selected) = self
            .by_type
            .get(&event.r#type)
            .and_then(|subjects| subjects.get(&event.subject_type))
        {
            return selected;
        }
        let selected = interest.selects(event);
        if self.len < SELECTOR_CACHE_CAP {
            self.by_type
                .entry(event.r#type.clone())
                .or_default()
                .insert(event.subject_type.clone(), selected);
            self.len += 1;
        }
        selected
    }
}

impl SingleTypeAuthorizer {
    /// Asks as `ctx`, through `enforcer` and `specs`, compiling with
    /// `interner`, and gives each round of a batch's evaluations `deadline`
    /// in all.
    ///
    /// Called in the task of the request that opens the stream: it takes that
    /// request's `x-request-id` and span here, because the stream's batches
    /// are prepared later, outside both (a REST body is polled after the
    /// handler returns), and every evaluation must stay correlated with the
    /// request the stream belongs to.
    #[must_use]
    pub fn new(
        enforcer: PolicyEnforcer,
        specs: Arc<dyn SpecificationManager>,
        interner: Arc<TenantSetInterner>,
        ctx: SecurityContext,
        deadline: Duration,
    ) -> Self {
        Self {
            enforcer,
            specs,
            interner,
            ctx,
            deadline,
            request_id: toolkit::api::current_request_id(),
            span: tracing::Span::current(),
        }
    }

    /// Runs one wave in rounds of at most [`SINGLE_TYPE_CONCURRENCY`] asks,
    /// one round after another, each round under its own deadline: a wave of
    /// the first event that needs more asks than one round holds still gives
    /// every ask the time a single round gets. The first failure, or a
    /// round's deadline, drops the rest, which aborts them. Each evaluation
    /// runs under the opening request's `x-request-id` and in its span, which
    /// a spawned task does not inherit.
    async fn evaluate_all(
        self: Arc<Self>,
        asks: Vec<SingleTypeAsk>,
    ) -> Result<Vec<(SingleTypeAsk, SingleTypeAnswer)>, DomainError> {
        let mut answers = Vec::with_capacity(asks.len());
        let mut asks = asks.into_iter().peekable();
        while asks.peek().is_some() {
            let round: Vec<SingleTypeAsk> = asks.by_ref().take(SINGLE_TYPE_CONCURRENCY).collect();
            answers.extend(Arc::clone(&self).evaluate_round(round).await?);
        }
        Ok(answers)
    }

    /// Runs `round`, at most [`SINGLE_TYPE_CONCURRENCY`] asks, all at once
    /// under one deadline. Dropping the set on an error or at the deadline
    /// aborts the evaluations still running.
    async fn evaluate_round(
        self: Arc<Self>,
        round: Vec<SingleTypeAsk>,
    ) -> Result<Vec<(SingleTypeAsk, SingleTypeAnswer)>, DomainError> {
        let deadline = self.deadline;
        let run = async move {
            let mut running = JoinSet::new();
            for ask in round {
                running.spawn(
                    toolkit::api::with_request_id(
                        self.request_id.clone(),
                        Arc::clone(&self).evaluate(ask),
                    )
                    .instrument(self.span.clone()),
                );
            }
            let mut answers = Vec::with_capacity(running.len());
            while let Some(joined) = running.join_next().await {
                let answered = joined.map_err(|_| {
                    DomainError::Internal("a single-type evaluation did not finish".to_owned())
                })??;
                answers.push(answered);
            }
            Ok(answers)
        };
        tokio::time::timeout(deadline, run).await.map_err(|_| {
            DomainError::Internal(format!(
                "the single-type consume evaluations did not finish within {deadline:?}"
            ))
        })?
    }

    /// One single-type evaluation: the type's point read, then one E naming
    /// the type and the subject types it allows.
    async fn evaluate(
        self: Arc<Self>,
        ask: SingleTypeAsk,
    ) -> Result<(SingleTypeAsk, SingleTypeAnswer), DomainError> {
        let event_type = self.specs.get_event_type(&ask.event_type).await?;
        let subject_types: BTreeSet<&str> = event_type
            .iter()
            .flat_map(|registered| &registered.allowed_subject_types)
            .map(GtsIdPattern::pattern)
            .collect();
        let call = ConsumeCall::single_type(
            &ask.key,
            ask.patterns.iter().map(String::as_str).collect(),
            &ask.event_type,
            subject_types,
        );
        let answer = match call.send(&self.ctx, &self.enforcer).await {
            Ok(scope) => match CompiledScope::compile(&scope, &ask.key.mode, &self.interner) {
                Ok(compiled) => SingleTypeAnswer::Planned(compiled.plan_for(&ask.event_type)),
                Err(_) => SingleTypeAnswer::Refused(Refusal::NotEnforceable),
            },
            Err(EnforcerError::AccessDenied(_)) => SingleTypeAnswer::Refused(Refusal::Denied),
            Err(EnforcerError::CompileFailed(_)) => {
                SingleTypeAnswer::Refused(Refusal::CompileFailed)
            }
            // A failed evaluation, or an outcome class this build does not
            // know: the batch ends without skipping the event.
            Err(error) => return Err(error.into()),
        };
        Ok((ask, answer))
    }
}

#[cfg(test)]
#[path = "scoped_filter_tests.rs"]
pub(crate) mod scoped_filter_tests;

//! The consume scope a member holds: each row of the compare-and-swap table,
//! the generations the coordinator assigns, every removal path that drops the
//! scope, and the principal it is cached for.

use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use authz_resolver_sdk::TenantMode;
use tokio::time::Instant;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopeGeneration, ScopePrincipal,
    ScopeWrite, ScopeWriteOutcome,
};
use crate::domain::consumer_group_coordinator::{
    ConsumerGroupCoordinator, MemberState, Removal, TopicInterest,
};
use crate::domain::join_scope::GroupKey;
use crate::domain::model::BarrierMode;
use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::test_support::event_type_id;

const GROUP: &str = "gts.cf.core.events.consumer_group.v1~x.eb.cg.scope.v1";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.t1.scope.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.orders.created.v1~";

const SUB_A: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00a1);
const SUB_B: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00b1);
const SUB_C: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00c1);
const UNKNOWN: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00ff);

const SUBJECT: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0011);
const TENANT_1: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001);
const TENANT_2: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0002);
const TENANT_3: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0003);

const JOIN_TIMEOUT: Duration = Duration::from_mins(1);
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounds the wait for a removed member's scope channel to report its close.
/// The sender is gone before the wait starts, so `changed` returns at once;
/// the bound only turns a scope that outlived its member into a failure
/// instead of a hang.
const BOUND: Duration = Duration::from_secs(5);

fn coordinator() -> Arc<ConsumerGroupCoordinator> {
    Arc::new(ConsumerGroupCoordinator::new(JOIN_TIMEOUT))
}

fn group(raw: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(raw).unwrap()
}

fn topic_id() -> GtsInstanceId {
    GtsInstanceId::try_new(TOPIC).unwrap()
}

/// JOINs `sub_id` to `group_id`; returns the group's topology version after
/// the join.
fn join(coordinator: &ConsumerGroupCoordinator, group_id: &str, sub_id: Uuid) -> i64 {
    let (_, version, _) = coordinator.join_member(
        &group(group_id),
        sub_id,
        &[TopicInterest {
            id: topic_id(),
            partitions: 4,
        }],
        SESSION_TIMEOUT,
    );
    version
}

fn principal() -> ScopePrincipal {
    ScopePrincipal {
        subject_id: SUBJECT,
        subject_type: Some("user".to_owned()),
        subject_tenant_id: TENANT_1,
        token_scopes: vec!["events:consume".to_owned()],
    }
}

/// The content of a scope of one root-only group at `tenant`, expiring a
/// minute after `base`.
fn scope_content(tenant: Uuid, base: Instant) -> ScopeContent {
    let compiled = CompiledScope::compile(
        &AccessScope::for_tenant(tenant),
        &TenantMode::RootOnly,
        &TenantSetInterner::new(),
    )
    .expect("a tenant scope compiles");
    ScopeContent {
        principal: principal(),
        groups: Arc::from([AuthorizedGroup {
            key: GroupKey {
                topic: topic_id(),
                tenant,
                mode: TenantMode::RootOnly,
                barrier_mode: BarrierMode::Respect,
            },
            scope: Arc::new(compiled),
            candidates: Arc::new(HashSet::from([event_type_id(EVENT_TYPE)])),
            candidates_stale: false,
            expires_at: base + Duration::from_mins(1),
        }]),
    }
}

/// The scope a member holds once `content` is stored under `generation`.
fn held_at(generation: u64, content: &ScopeContent) -> Arc<CachedScope> {
    Arc::new(CachedScope {
        generation: ScopeGeneration(generation),
        principal: content.principal.clone(),
        groups: Arc::clone(&content.groups),
    })
}

fn store(expected: Expected, content: &ScopeContent) -> ScopeWrite {
    ScopeWrite::Store {
        expected,
        content: content.clone(),
    }
}

fn clear(expected: Expected) -> ScopeWrite {
    ScopeWrite::Clear { expected }
}

// --- the compare-and-swap table ---

/// A JOIN starts the member with no scope, also when another member of its
/// group holds one.
#[test]
fn a_new_member_holds_no_scope() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(coordinator.scope_of(SUB_A), None);

    let content = scope_content(TENANT_1, base);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &content)),
        ScopeWriteOutcome::Applied
    );
    join(&coordinator, GROUP, SUB_B);

    assert_eq!(
        (coordinator.scope_of(SUB_A), coordinator.scope_of(SUB_B)),
        (Some(held_at(1, &content)), None)
    );
}

#[test]
fn storing_on_an_absent_scope_applies() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    let mut watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let content = scope_content(TENANT_1, base);

    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &content)),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(1, &content)));
    assert_eq!(watcher.has_changed().ok(), Some(true));
    assert_eq!(*watcher.borrow_and_update(), Some(held_at(1, &content)));
}

/// Clearing what is already absent changes nothing, so a session waiting on
/// the scope is not woken.
#[test]
fn clearing_an_absent_scope_applies_without_waking_watchers() {
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    let watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");

    assert_eq!(
        coordinator.write_scope(SUB_A, clear(Expected::Absent)),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(
        (coordinator.scope_of(SUB_A), watcher.has_changed().ok()),
        (None, Some(false))
    );
}

#[test]
fn storing_over_the_expected_generation_applies() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let mut watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let second = scope_content(TENANT_2, base);

    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Generation(ScopeGeneration(1)), &second)
        ),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(2, &second)));
    assert_eq!(watcher.has_changed().ok(), Some(true));
    assert_eq!(*watcher.borrow_and_update(), Some(held_at(2, &second)));
}

/// Clearing a held scope wakes its watchers, so a session waiting on the
/// scope learns of a revocation at once.
#[test]
fn clearing_the_expected_generation_applies_and_wakes_watchers() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");

    assert_eq!(
        coordinator.write_scope(SUB_A, clear(Expected::Generation(ScopeGeneration(1)))),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(watcher.has_changed().ok(), Some(true));
    assert_eq!(*watcher.borrow(), None);
    assert_eq!(coordinator.scope_of(SUB_A), None);
}

/// The store goes to `SUB_A` and the clear to `SUB_B`, so neither write can
/// change what the other one compares against.
#[test]
fn a_generation_expectation_on_an_absent_scope_is_superseded() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    join(&coordinator, GROUP, SUB_B);
    let watcher_a = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let watcher_b = coordinator.watch_scope(SUB_B).expect("SUB_B is a member");

    let stored = coordinator.write_scope(
        SUB_A,
        store(
            Expected::Generation(ScopeGeneration(1)),
            &scope_content(TENANT_1, base),
        ),
    );
    let cleared = coordinator.write_scope(SUB_B, clear(Expected::Generation(ScopeGeneration(1))));

    assert_eq!(
        (
            stored,
            cleared,
            coordinator.scope_of(SUB_A),
            coordinator.scope_of(SUB_B),
            watcher_a.has_changed().ok(),
            watcher_b.has_changed().ok(),
        ),
        (
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::Superseded,
            None,
            None,
            Some(false),
            Some(false),
        )
    );
}

/// The store goes to `SUB_A` and the clear to `SUB_B`, so neither write can
/// change what the other one compares against.
#[test]
fn an_absent_expectation_on_a_held_scope_is_superseded() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    join(&coordinator, GROUP, SUB_B);
    let content_a = scope_content(TENANT_1, base);
    let content_b = scope_content(TENANT_2, base);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &content_a)),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(
        coordinator.write_scope(SUB_B, store(Expected::Absent, &content_b)),
        ScopeWriteOutcome::Applied
    );
    let watcher_a = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let watcher_b = coordinator.watch_scope(SUB_B).expect("SUB_B is a member");

    let stored = coordinator.write_scope(
        SUB_A,
        store(Expected::Absent, &scope_content(TENANT_3, base)),
    );
    let cleared = coordinator.write_scope(SUB_B, clear(Expected::Absent));

    assert_eq!(
        (
            stored,
            cleared,
            coordinator.scope_of(SUB_A),
            coordinator.scope_of(SUB_B),
            watcher_a.has_changed().ok(),
            watcher_b.has_changed().ok(),
        ),
        (
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::Superseded,
            Some(held_at(1, &content_a)),
            Some(held_at(1, &content_b)),
            Some(false),
            Some(false),
        )
    );
}

/// Both members hold generation 2 and both writers expect generation 1: a
/// writer that read the scope before another writer swapped it loses. The
/// store goes to `SUB_A` and the clear to `SUB_B`, so neither write can change
/// what the other one compares against.
#[test]
fn a_stale_generation_is_superseded() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    join(&coordinator, GROUP, SUB_B);
    let second_a = scope_content(TENANT_1, base);
    let second_b = scope_content(TENANT_2, base);
    for (sub_id, first, second) in [
        (SUB_A, scope_content(TENANT_3, base), &second_a),
        (SUB_B, scope_content(TENANT_3, base), &second_b),
    ] {
        assert_eq!(
            coordinator.write_scope(sub_id, store(Expected::Absent, &first)),
            ScopeWriteOutcome::Applied
        );
        assert_eq!(
            coordinator.write_scope(
                sub_id,
                store(Expected::Generation(ScopeGeneration(1)), second)
            ),
            ScopeWriteOutcome::Applied
        );
    }
    let watcher_a = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let watcher_b = coordinator.watch_scope(SUB_B).expect("SUB_B is a member");

    let stored = coordinator.write_scope(
        SUB_A,
        store(
            Expected::Generation(ScopeGeneration(1)),
            &scope_content(TENANT_3, base),
        ),
    );
    let cleared = coordinator.write_scope(SUB_B, clear(Expected::Generation(ScopeGeneration(1))));

    assert_eq!(
        (
            stored,
            cleared,
            coordinator.scope_of(SUB_A),
            coordinator.scope_of(SUB_B),
            watcher_a.has_changed().ok(),
            watcher_b.has_changed().ok(),
        ),
        (
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::Superseded,
            Some(held_at(2, &second_a)),
            Some(held_at(2, &second_b)),
            Some(false),
            Some(false),
        )
    );
}

/// A write for a subscription the coordinator does not hold lands nowhere,
/// not on another member.
#[test]
fn writing_to_an_unknown_subscription_reports_the_member_gone() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);

    let stored = coordinator.write_scope(
        UNKNOWN,
        store(Expected::Absent, &scope_content(TENANT_1, base)),
    );
    let cleared = coordinator.write_scope(UNKNOWN, clear(Expected::Absent));

    assert_eq!(
        (
            stored,
            cleared,
            coordinator.scope_of(UNKNOWN),
            coordinator.watch_scope(UNKNOWN).is_none(),
            coordinator.scope_of(SUB_A),
        ),
        (
            ScopeWriteOutcome::MemberGone,
            ScopeWriteOutcome::MemberGone,
            None,
            true,
            None,
        )
    );
}

// --- generations ---

#[test]
fn the_first_store_is_generation_one() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    let content = scope_content(TENANT_1, base);

    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &content)),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(
        coordinator.scope_of(SUB_A),
        Some(Arc::new(CachedScope {
            generation: ScopeGeneration(1),
            principal: principal(),
            groups: Arc::clone(&content.groups),
        }))
    );
}

/// A clear does not reset the count: the store after it continues from the
/// last generation the member held.
#[test]
fn each_applied_store_advances_the_generation_by_one() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    let first = scope_content(TENANT_1, base);
    let second = scope_content(TENANT_2, base);
    let third = scope_content(TENANT_3, base);

    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &first)),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(1, &first)));
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Generation(ScopeGeneration(1)), &second)
        ),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(2, &second)));
    assert_eq!(
        coordinator.write_scope(SUB_A, clear(Expected::Generation(ScopeGeneration(2)))),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(coordinator.scope_of(SUB_A), None);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &third)),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(3, &third)));
}

/// Writes that apply nothing, a superseded store or clear and a write to a
/// subscription that is not a member, use up no number: the next applied
/// store follows the last one with no gap.
#[test]
fn a_superseded_or_gone_write_assigns_no_generation() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    let first = scope_content(TENANT_1, base);
    let next = scope_content(TENANT_2, base);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &first)),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(
        (
            coordinator.write_scope(SUB_A, store(Expected::Absent, &next)),
            coordinator.write_scope(
                SUB_A,
                store(Expected::Generation(ScopeGeneration(7)), &next)
            ),
            coordinator.write_scope(SUB_A, clear(Expected::Generation(ScopeGeneration(7)))),
            coordinator.write_scope(UNKNOWN, store(Expected::Absent, &next)),
        ),
        (
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::Superseded,
            ScopeWriteOutcome::MemberGone,
        )
    );
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Generation(ScopeGeneration(1)), &next)
        ),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(coordinator.scope_of(SUB_A), Some(held_at(2, &next)));
}

/// Each member counts its own stores: a member's first store is generation
/// 1 whatever another member of the group has stored.
#[test]
fn generations_are_per_member() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    join(&coordinator, GROUP, SUB_B);
    let content_a = scope_content(TENANT_1, base);
    let content_b = scope_content(TENANT_2, base);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_3, base))
        ),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Generation(ScopeGeneration(1)), &content_a)
        ),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(
        coordinator.write_scope(SUB_B, store(Expected::Absent, &content_b)),
        ScopeWriteOutcome::Applied
    );

    assert_eq!(
        (coordinator.scope_of(SUB_A), coordinator.scope_of(SUB_B)),
        (Some(held_at(2, &content_a)), Some(held_at(1, &content_b)))
    );
}

/// Writer B stores a scope and writer A reads it. B then clears the scope and
/// stores a fresh one. A's write, made against what it read, must not land on
/// B's fresh scope: the generation A read is never handed out again.
#[test]
fn a_generation_is_never_reused_after_a_clear() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let read_by_a = coordinator.scope_of(SUB_A).expect("B's store applied");
    assert_eq!(
        coordinator.write_scope(SUB_A, clear(Expected::Generation(ScopeGeneration(1)))),
        ScopeWriteOutcome::Applied
    );
    let fresh = scope_content(TENANT_2, base);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &fresh)),
        ScopeWriteOutcome::Applied
    );

    let stale = coordinator.write_scope(
        SUB_A,
        store(
            Expected::Generation(read_by_a.generation),
            &scope_content(TENANT_3, base),
        ),
    );

    assert_eq!(
        (stale, coordinator.scope_of(SUB_A)),
        (
            ScopeWriteOutcome::Superseded,
            Some(Arc::new(CachedScope {
                generation: ScopeGeneration(2),
                principal: principal(),
                groups: Arc::clone(&fresh.groups),
            }))
        )
    );
}

// --- removal paths ---

#[tokio::test]
async fn leaving_drops_the_scope() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let held = coordinator.scope_of(SUB_A).expect("the store applied");
    let mut watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");

    assert_eq!(
        coordinator.leave(&group(GROUP), SUB_A),
        Some(Removal {
            subscription_id: SUB_A,
            group: group(GROUP),
            group_emptied: true,
        })
    );

    assert_eq!(coordinator.scope_of(SUB_A), None);
    let closed = tokio::time::timeout(BOUND, watcher.changed())
        .await
        .expect("the close is seen within BOUND");
    assert_eq!(closed.ok(), None, "the sender is dropped with the member");
    drop(watcher);
    assert_eq!(Arc::strong_count(&held), 1, "nothing else holds the scope");
}

#[tokio::test]
async fn a_join_that_evicts_a_disconnected_member_drops_its_scope() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let held = coordinator.scope_of(SUB_A).expect("the store applied");
    let mut watcher = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group(GROUP), SUB_A)
            .expect("SUB_A is a member");
    drop(membership);
    assert_eq!(coordinator.state_of(SUB_A), Some(MemberState::Disconnected));

    let (_, _, evicted) = coordinator.join_member(
        &group(GROUP),
        SUB_B,
        &[TopicInterest {
            id: topic_id(),
            partitions: 4,
        }],
        SESSION_TIMEOUT,
    );

    assert_eq!(
        evicted,
        vec![Removal {
            subscription_id: SUB_A,
            group: group(GROUP),
            group_emptied: false,
        }]
    );
    assert_eq!(coordinator.scope_of(SUB_A), None);
    let closed = tokio::time::timeout(BOUND, watcher.changed())
        .await
        .expect("the close is seen within BOUND");
    assert_eq!(closed.ok(), None, "the sender is dropped with the member");
    drop(watcher);
    assert_eq!(Arc::strong_count(&held), 1, "nothing else holds the scope");
}

/// Reaped from each state that has a lifetime: `Joined` past the join
/// timeout, `Disconnected` past its session timeout, each on its own
/// coordinator. Each sweep instant is taken after the member entered its
/// state, so it is past the lifetime whatever the scheduling.
#[tokio::test]
async fn reaping_drops_the_scope() {
    let base = Instant::now();

    let joined = coordinator();
    join(&joined, GROUP, SUB_A);
    assert_eq!(
        joined.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let held_a = joined.scope_of(SUB_A).expect("the store applied");
    let mut watcher_a = joined.watch_scope(SUB_A).expect("SUB_A is a member");
    let joined_by = Instant::now();

    assert_eq!(
        joined.sweep(joined_by + JOIN_TIMEOUT),
        vec![Removal {
            subscription_id: SUB_A,
            group: group(GROUP),
            group_emptied: true,
        }]
    );
    assert_eq!(joined.scope_of(SUB_A), None);
    let closed_a = tokio::time::timeout(BOUND, watcher_a.changed())
        .await
        .expect("the close is seen within BOUND");
    assert_eq!(closed_a.ok(), None, "the sender is dropped with the member");

    let disconnected = coordinator();
    join(&disconnected, GROUP, SUB_B);
    assert_eq!(
        disconnected.write_scope(
            SUB_B,
            store(Expected::Absent, &scope_content(TENANT_2, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let held_b = disconnected.scope_of(SUB_B).expect("the store applied");
    let mut watcher_b = disconnected.watch_scope(SUB_B).expect("SUB_B is a member");
    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&disconnected, &group(GROUP), SUB_B)
            .expect("SUB_B is a member");
    drop(membership);
    assert_eq!(
        disconnected.state_of(SUB_B),
        Some(MemberState::Disconnected)
    );
    let disconnected_by = Instant::now();

    assert_eq!(
        disconnected.sweep(disconnected_by + SESSION_TIMEOUT),
        vec![Removal {
            subscription_id: SUB_B,
            group: group(GROUP),
            group_emptied: true,
        }]
    );
    assert_eq!(disconnected.scope_of(SUB_B), None);
    let closed_b = tokio::time::timeout(BOUND, watcher_b.changed())
        .await
        .expect("the close is seen within BOUND");
    assert_eq!(closed_b.ok(), None, "the sender is dropped with the member");

    drop((watcher_a, watcher_b));
    assert_eq!(
        (Arc::strong_count(&held_a), Arc::strong_count(&held_b)),
        (1, 1),
        "nothing else holds either scope"
    );
}

/// A write racing a removal cannot leave a scope behind: after each removal
/// path the write finds no member.
#[test]
fn a_write_after_removal_reports_the_member_gone() {
    let base = Instant::now();
    let content = scope_content(TENANT_1, base);
    let write_both = |coordinator: &ConsumerGroupCoordinator, sub_id: Uuid| {
        (
            coordinator.write_scope(sub_id, store(Expected::Absent, &content)),
            coordinator.write_scope(sub_id, clear(Expected::Absent)),
        )
    };
    let gone = (ScopeWriteOutcome::MemberGone, ScopeWriteOutcome::MemberGone);

    let left = coordinator();
    join(&left, GROUP, SUB_A);
    left.leave(&group(GROUP), SUB_A)
        .expect("SUB_A was a member");
    assert_eq!(write_both(&left, SUB_A), gone, "after a leave");

    let evicting = coordinator();
    join(&evicting, GROUP, SUB_A);
    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&evicting, &group(GROUP), SUB_A)
            .expect("SUB_A is a member");
    drop(membership);
    join(&evicting, GROUP, SUB_B);
    assert_eq!(write_both(&evicting, SUB_A), gone, "after an eviction");
    assert_eq!(
        evicting.scope_of(SUB_B),
        None,
        "nothing landed on the joiner"
    );

    let reaped = coordinator();
    join(&reaped, GROUP, SUB_A);
    let joined_by = Instant::now();
    assert_eq!(
        reaped.sweep(joined_by + JOIN_TIMEOUT),
        vec![Removal {
            subscription_id: SUB_A,
            group: group(GROUP),
            group_emptied: true,
        }]
    );
    assert_eq!(write_both(&reaped, SUB_A), gone, "after a reap");
}

// --- concurrency ---

/// Eight writers that read the same generation race to replace it: the
/// compare-and-swap runs under the coordinator lock, so exactly one applies,
/// its scope is generation 2, and the rest see a newer generation than they
/// expected.
#[test]
fn concurrent_writers_with_one_expectation_apply_exactly_once() {
    const WRITERS: u128 = 8;
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    assert_eq!(
        coordinator.write_scope(
            SUB_A,
            store(Expected::Absent, &scope_content(TENANT_1, base))
        ),
        ScopeWriteOutcome::Applied
    );
    let candidates: Vec<ScopeContent> = (0..WRITERS)
        .map(|i| scope_content(Uuid::from_u128(TENANT_2.as_u128() + i), base))
        .collect();
    let barrier = Barrier::new(candidates.len());

    let outcomes: Vec<ScopeWriteOutcome> = thread::scope(|s| {
        let handles: Vec<_> = candidates
            .iter()
            .map(|content| {
                let (coordinator, barrier) = (&*coordinator, &barrier);
                s.spawn(move || {
                    barrier.wait();
                    coordinator.write_scope(
                        SUB_A,
                        store(Expected::Generation(ScopeGeneration(1)), content),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a writer panicked"))
            .collect()
    });

    let applied: Vec<usize> = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| **outcome == ScopeWriteOutcome::Applied)
        .map(|(i, _)| i)
        .collect();
    let superseded = outcomes
        .iter()
        .filter(|outcome| **outcome == ScopeWriteOutcome::Superseded)
        .count();
    assert_eq!((applied.len(), superseded), (1, 7), "{outcomes:?}");
    let [winner] = applied.as_slice() else {
        panic!("exactly one writer applies: {outcomes:?}");
    };
    assert_eq!(
        coordinator.scope_of(SUB_A),
        Some(held_at(2, &candidates[*winner]))
    );
}

/// A rebalance rewrites every member's assignment and leaves every member's
/// scope as it was, the same value it held, without waking its watchers.
#[test]
fn a_rebalance_keeps_every_members_scope() {
    let base = Instant::now();
    let coordinator = coordinator();
    join(&coordinator, GROUP, SUB_A);
    join(&coordinator, GROUP, SUB_B);
    let content_a = scope_content(TENANT_1, base);
    let content_b = scope_content(TENANT_2, base);
    assert_eq!(
        coordinator.write_scope(SUB_A, store(Expected::Absent, &content_a)),
        ScopeWriteOutcome::Applied
    );
    assert_eq!(
        coordinator.write_scope(SUB_B, store(Expected::Absent, &content_b)),
        ScopeWriteOutcome::Applied
    );
    let held_a = coordinator.scope_of(SUB_A).expect("the store applied");
    let held_b = coordinator.scope_of(SUB_B).expect("the store applied");
    let watcher_a = coordinator.watch_scope(SUB_A).expect("SUB_A is a member");
    let watcher_b = coordinator.watch_scope(SUB_B).expect("SUB_B is a member");

    assert_eq!(
        join(&coordinator, GROUP, SUB_C),
        3,
        "the third join rebalances"
    );

    let now_a = coordinator.scope_of(SUB_A).expect("SUB_A keeps its scope");
    let now_b = coordinator.scope_of(SUB_B).expect("SUB_B keeps its scope");
    assert!(Arc::ptr_eq(&now_a, &held_a));
    assert!(Arc::ptr_eq(&now_b, &held_b));
    assert_eq!(
        (
            now_a,
            now_b,
            watcher_a.has_changed().ok(),
            watcher_b.has_changed().ok(),
            coordinator.scope_of(SUB_C),
        ),
        (
            held_at(1, &content_a),
            held_at(1, &content_b),
            Some(false),
            Some(false),
            None
        )
    );
}

// --- types ---

/// Every identity field is carried; the bearer token is not, so a refreshed
/// token alone does not change the principal a scope was cached for.
#[test]
fn a_principal_is_taken_from_the_security_context() {
    let context = |token: &str| {
        SecurityContext::builder()
            .subject_id(SUBJECT)
            .subject_type("user")
            .subject_tenant_id(TENANT_1)
            .token_scopes(vec!["events:consume".to_owned(), "events:read".to_owned()])
            .bearer_token(token.to_owned())
            .build()
            .expect("a complete context")
    };
    let untyped = SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT_2)
        .build()
        .expect("a complete context");

    assert_eq!(
        ScopePrincipal::from(&context("token-one")),
        ScopePrincipal {
            subject_id: SUBJECT,
            subject_type: Some("user".to_owned()),
            subject_tenant_id: TENANT_1,
            token_scopes: vec!["events:consume".to_owned(), "events:read".to_owned()],
        }
    );
    assert_eq!(
        ScopePrincipal::from(&context("token-two")),
        ScopePrincipal::from(&context("token-one"))
    );
    assert_eq!(
        ScopePrincipal::from(&untyped),
        ScopePrincipal {
            subject_id: SUBJECT,
            subject_type: None,
            subject_tenant_id: TENANT_2,
            token_scopes: Vec::new(),
        }
    );
}

/// Two groups expiring a minute and two minutes after `base`.
#[test]
fn a_scope_serves_until_its_earliest_group_expiry() {
    let base = Instant::now();
    let content = scope_content(TENANT_1, base);
    let [first] = content.groups.as_ref() else {
        panic!("one group: {:?}", content.groups);
    };
    let later = AuthorizedGroup {
        expires_at: first.expires_at + Duration::from_mins(1),
        ..first.clone()
    };
    let scope = CachedScope {
        generation: ScopeGeneration(1),
        principal: principal(),
        groups: Arc::from([first.clone(), later]),
    };
    let expires_at = first.expires_at;
    let other = ScopePrincipal {
        subject_id: Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0012),
        ..principal()
    };

    assert!(scope.serves(&principal(), expires_at - Duration::from_nanos(1)));
    assert!(!scope.serves(&principal(), expires_at));
    assert!(!scope.serves(&principal(), expires_at + Duration::from_secs(30)));
    assert!(!scope.serves(&other, expires_at - Duration::from_nanos(1)));
}

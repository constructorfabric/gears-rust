//! The requests one group sends, built without a harness, and the memo write.
//!
//! Each request test builds the group's call, turns it into the request the
//! PDP receives (`PolicyEnforcer::build_request_with`, as `send` does) and
//! asserts the whole serialized value. The enforcer has no capabilities and
//! the caller no subject type and no token scopes. The caller's tenant
//! (`0x123`) is never the interest tenant (`0x234`), so a request rooted at
//! the wrong one shows in the literal.
//!
//! The post-check tests compile a root-only scope, as E's answer is compiled,
//! and check one group of one interest against it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::{PolicyEnforcer, TenantMode};
use gts::{GtsIdPattern, GtsTypeId};
use serde_json::{Value, json};
use tokio::time::Instant;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{
    AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::{ConsumeCall, ConsumeGroup, ConsumeMemo, GroupKey, InterestGroup};
use crate::domain::consumer_group_coordinator::member_scope::AuthorizedGroup;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{BarrierMode, Interest, OwnershipFilter};
use crate::domain::specification::{EventTypeCandidate, EventTypeCandidates};
use crate::domain::streaming::scope::{
    CompiledScope, EVENT_SUBJECT_TYPE, EVENT_TYPE, TenantSetInterner,
};
use crate::domain::streaming::scope_memo::{MemoKey, MemoResult, ScopeMemo};
use crate::test_support::authz_doubles::AllowAllAuthZ;

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.joinscope.orders.v1";
const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.joinscope.created.v1~";
const SHIPPED: &str = "gts.cf.core.events.event.v1~x.eb.joinscope.shipped.v1~";
const UNRELATED: &str = "gts.cf.core.events.event.v1~x.eb.joinscope.unrelated.v1~";
/// An event type derived from `CREATED`, which `CREATED` as a selector matches.
const EXPRESS_CREATED: &str =
    "gts.cf.core.events.event.v1~x.eb.joinscope.created.v1~x.eb.joinscope.express.v1~";
/// Another event type derived from `CREATED`, beside `EXPRESS_CREATED`.
const RUSH_CREATED: &str =
    "gts.cf.core.events.event.v1~x.eb.joinscope.created.v1~x.eb.joinscope.rush.v1~";
const ORDER: &str = "gts.x.eb.joinscope.order.v1~";
const INVOICE: &str = "gts.x.eb.joinscope.invoice.v1~";
const AUDIT: &str = "gts.x.eb.joinscope.audit.v1~";
/// A subject type derived from `ORDER`, which `ORDER` as a selector covers.
const SPECIAL_ORDER: &str = "gts.x.eb.joinscope.order.v1~x.eb.joinscope.special.v1~";
/// Another subject type derived from `ORDER`, beside `SPECIAL_ORDER`.
const RUSH_ORDER: &str = "gts.x.eb.joinscope.order.v1~x.eb.joinscope.rush.v1~";
/// A minor version of `ORDER`, which `ORDER` as a selector covers.
const ORDER_MINOR: &str = "gts.x.eb.joinscope.order.v1.2~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
const TENANT: Uuid = Uuid::from_u128(0x234);

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(CALLER_TENANT)
        .build()
        .expect("valid security context")
}

fn enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(AllowAllAuthZ))
}

fn pattern(raw: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(raw).expect("valid pattern")
}

fn interest(barrier_mode: BarrierMode, types: &[&str], subject_types: &[&str]) -> Interest {
    Interest {
        topic: GtsInstanceId::try_new(TOPIC).expect("valid topic"),
        subject_tenant: TENANT,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode,
        types: types.iter().map(|raw| pattern(raw)).collect(),
        subject_types: subject_types.iter().map(|raw| pattern(raw)).collect(),
        filter: None,
    }
}

fn candidate(id: &str, allowed_subject_types: &[&str]) -> EventTypeCandidate {
    EventTypeCandidate {
        id: GtsTypeId::new(id),
        allowed_subject_types: allowed_subject_types
            .iter()
            .map(|raw| pattern(raw))
            .collect(),
    }
}

/// The one group `interests` form, with `candidates`.
fn group(interests: &[Interest], candidates: EventTypeCandidates) -> ConsumeGroup {
    let groups = InterestGroup::form(interests, CALLER_TENANT).expect("valid interests");
    let [group] = groups.as_slice() else {
        panic!("expected one group, got {groups:?}");
    };
    group.clone().with_candidates(candidates)
}

fn sent(call: &ConsumeCall) -> Value {
    serde_json::to_value(call.evaluation_request(&caller(), &enforcer())).expect("serializes")
}

/// The one group of an interest in `CREATED` selecting `subject_types`, whose
/// only candidate is `CREATED` allowing `allowed_subject_types`.
fn created_group(subject_types: &[&str], allowed_subject_types: &[&str]) -> ConsumeGroup {
    group(
        &[interest(BarrierMode::Respect, &[CREATED], subject_types)],
        EventTypeCandidates::Listed(vec![candidate(CREATED, allowed_subject_types)]),
    )
}

/// `In(property, ids)` over GTS ids, as a PDP answers a type rule.
fn ids_in(property: &str, ids: &[&str]) -> ScopeFilter {
    ScopeFilter::r#in(
        property,
        ids.iter()
            .map(|id| ScopeValue::String((*id).to_owned()))
            .collect(),
    )
}

/// E's answer, an OR of `constraints` each holding the interest tenant,
/// compiled root-only as JOIN compiles it.
fn compiled(constraints: Vec<Vec<ScopeFilter>>) -> CompiledScope {
    let scope = AccessScope::from_constraints(
        constraints
            .into_iter()
            .map(|filters| {
                let mut all = vec![ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, TENANT)];
                all.extend(filters);
                ScopeConstraint::new(all)
            })
            .collect(),
    );
    CompiledScope::compile(&scope, &TenantMode::RootOnly, &TenantSetInterner::new())
        .expect("the scope compiles")
}

#[test]
fn the_e_request_names_the_topic_tenant_patterns_candidates_and_subject_types() {
    let group = group(
        &[
            interest(BarrierMode::Respect, &[SHIPPED, CREATED], &[]),
            interest(BarrierMode::Respect, &[CREATED], &[]),
        ],
        EventTypeCandidates::Listed(vec![
            candidate(SHIPPED, &[ORDER, INVOICE, ORDER]),
            candidate(CREATED, &[ORDER]),
        ]),
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED, SHIPPED],
                    "event_types": [CREATED, SHIPPED],
                    "event_subject_type_patterns": [INVOICE, ORDER],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

#[test]
fn the_i_request_asks_for_the_barrier_right_root_only() {
    let group = group(
        &[interest(BarrierMode::Ignore, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &[ORDER])]),
    );

    assert_eq!(
        sent(&group.i_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume_ignore_barrier" },
            "resource": {
                "type": TOPIC,
                "properties": { "topic_id": TOPIC, "owner_tenant_id": TENANT },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "ignore",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })
    );
}

#[test]
fn the_r_request_repeats_the_tenant_context_of_e() {
    for (barrier_mode, wire) in [
        (BarrierMode::Respect, "respect"),
        (BarrierMode::Ignore, "ignore"),
    ] {
        let group = group(
            &[interest(barrier_mode, &[CREATED], &[])],
            EventTypeCandidates::Listed(vec![candidate(CREATED, &[ORDER])]),
        );

        assert_eq!(
            sent(&group.r_call()),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": "gts.cf.core.events.request.v1~",
                    "properties": { "owner_tenant_id": TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": wire,
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            "{wire}"
        );
    }
}

/// `interest` with `ownership` on `tenant`.
fn owning(interest: Interest, ownership: OwnershipFilter, tenant: Uuid) -> Interest {
    Interest {
        subject_tenant: tenant,
        ownership,
        ..interest
    }
}

/// `only_mine` asks root-only at the caller's own tenant and respects
/// barriers, even when the interest asks to ignore them, so no I is sent.
#[test]
fn an_only_mine_e_request_is_root_only_at_the_callers_tenant_and_sends_no_i() {
    let group = group(
        &[owning(
            interest(BarrierMode::Ignore, &[CREATED], &[]),
            OwnershipFilter::OnlyMine,
            CALLER_TENANT,
        )],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &[ORDER])]),
    );

    assert!(!group.crosses_barriers());
    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": CALLER_TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [CREATED],
                    "event_subject_type_patterns": [ORDER],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": CALLER_TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

/// `tenant_subtree_included` asks E and R for the subtree under the interest
/// tenant, and I root-only there.
#[test]
fn a_subtree_interest_asks_e_and_r_for_the_subtree_and_i_root_only() {
    let group = group(
        &[owning(
            interest(BarrierMode::Ignore, &[CREATED], &[]),
            OwnershipFilter::TenantSubtreeIncluded,
            TENANT,
        )],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &[ORDER])]),
    );
    let tenant_context = |call: &ConsumeCall| sent(call)["context"]["tenant_context"].clone();

    assert_eq!(group.tenant_mode(), &TenantMode::Subtree);
    assert_eq!(
        (
            tenant_context(&group.i_call()),
            tenant_context(&group.e_call()),
            tenant_context(&group.r_call()),
        ),
        (
            json!({ "mode": "root_only", "root_id": TENANT, "barrier_mode": "ignore" }),
            json!({ "mode": "subtree", "root_id": TENANT, "barrier_mode": "ignore" }),
            json!({ "mode": "subtree", "root_id": TENANT, "barrier_mode": "ignore" }),
        )
    );
}

/// `only_mine` and `entire_tenant` interests on the caller's own tenant share
/// the tenant context, so they form one group and cost one E.
#[test]
fn only_mine_and_entire_tenant_on_the_callers_tenant_share_one_group() {
    let mine = owning(
        interest(BarrierMode::Respect, &[CREATED], &[]),
        OwnershipFilter::OnlyMine,
        CALLER_TENANT,
    );
    let tenant_wide = owning(
        interest(BarrierMode::Respect, &[SHIPPED], &[]),
        OwnershipFilter::EntireTenant,
        CALLER_TENANT,
    );

    let groups = InterestGroup::form(&[mine, tenant_wide], CALLER_TENANT).expect("valid interests");

    let [group] = groups.as_slice() else {
        panic!("expected one group, got {groups:?}");
    };
    let group = group
        .clone()
        .with_candidates(EventTypeCandidates::Listed(Vec::new()));
    assert_eq!(
        sent(&group.e_call())["resource"]["properties"]["event_type_patterns"],
        json!([CREATED, SHIPPED])
    );
}

#[test]
fn candidates_outside_the_groups_patterns_are_not_sent() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![
            candidate(CREATED, &[ORDER]),
            candidate(UNRELATED, &[AUDIT]),
        ]),
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [CREATED],
                    "event_subject_type_patterns": [ORDER],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

#[test]
fn an_over_cap_topic_sends_no_candidates_and_no_subject_types() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::OverCap,
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

/// Over the cap the spec cache is asked about the exact type ids only, once
/// each, in order; an exact pattern that names an instance is never a
/// registered type, so it is reported without being asked about.
#[test]
fn over_the_cap_only_exact_type_ids_are_looked_up_and_an_instance_pattern_is_reported() {
    const INSTANCE: &str = "gts.cf.core.events.event.v1~x.eb.joinscope.instance.v1";
    const WILDCARD: &str = "gts.cf.core.events.event.v1~x.eb.joinscope.*";
    let group = group(
        &[
            interest(BarrierMode::Respect, &[SHIPPED, CREATED], &[]),
            interest(BarrierMode::Respect, &[CREATED, WILDCARD, INSTANCE], &[]),
        ],
        EventTypeCandidates::OverCap,
    );

    assert_eq!(
        group.unlisted_exact_type_ids(),
        vec![GtsTypeId::new(CREATED), GtsTypeId::new(SHIPPED)]
    );
    assert_eq!(
        group.unmatched_types(&[GtsTypeId::new(CREATED)]),
        vec![(0, vec![pattern(SHIPPED)]), (1, vec![pattern(INSTANCE)])]
    );
}

/// Listed candidates answer the check themselves: nothing is looked up.
#[test]
fn listed_candidates_need_no_lookup() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED, UNRELATED], &[])],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &[])]),
    );

    assert_eq!(group.unlisted_exact_type_ids(), Vec::<GtsTypeId>::new());
    assert_eq!(
        group.unmatched_types(&[]),
        vec![(0, vec![pattern(UNRELATED)])]
    );
}

#[test]
fn more_than_256_subject_types_are_not_sent() {
    let subject_types: Vec<String> = (0..257)
        .map(|n| format!("gts.x.eb.joinscope.s{n:03}.v1~"))
        .collect();
    let first_256: Vec<&str> = subject_types[..256].iter().map(String::as_str).collect();
    let all_257: Vec<&str> = subject_types.iter().map(String::as_str).collect();

    let at_the_cap = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &first_256)]),
    );
    assert_eq!(
        sent(&at_the_cap.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [CREATED],
                    "event_subject_type_patterns": first_256,
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        }),
        "256 subject types"
    );

    let over_the_cap = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &all_257)]),
    );
    assert_eq!(
        sent(&over_the_cap.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [CREATED],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        }),
        "257 subject types"
    );
}

/// The single-type E for `SHIPPED` in the group of an interest in `CREATED`,
/// naming `subject_types` as `SHIPPED`'s own.
fn single_type_sent(subject_types: &[&str]) -> Value {
    let interest = interest(BarrierMode::Respect, &[CREATED], &[]);
    sent(&ConsumeCall::single_type(
        &GroupKey::new(&interest, CALLER_TENANT),
        BTreeSet::from([CREATED]),
        &GtsTypeId::new(SHIPPED),
        subject_types.iter().copied().collect(),
    ))
}

#[test]
fn the_single_type_e_request_names_one_type_and_its_subject_types() {
    assert_eq!(
        single_type_sent(&[ORDER, INVOICE]),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [SHIPPED],
                    "event_subject_type_patterns": [INVOICE, ORDER],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

/// No subject type, or more than 256: the request names none.
#[test]
fn a_single_type_e_request_sends_no_subject_types_when_there_are_none_or_too_many() {
    let subject_types: Vec<String> = (0..257)
        .map(|n| format!("gts.x.eb.joinscope.s{n:03}.v1~"))
        .collect();
    let all_257: Vec<&str> = subject_types.iter().map(String::as_str).collect();
    let without_subject_types = json!({
        "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
        "action": { "name": "consume" },
        "resource": {
            "type": TOPIC,
            "properties": {
                "topic_id": TOPIC,
                "owner_tenant_id": TENANT,
                "event_type_patterns": [CREATED],
                "event_types": [SHIPPED],
            },
        },
        "context": {
            "tenant_context": {
                "mode": "root_only",
                "root_id": TENANT,
                "barrier_mode": "respect",
            },
            "token_scopes": [],
            "require_constraints": true,
            "capabilities": [],
            "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
        },
    });

    assert_eq!(single_type_sent(&[]), without_subject_types, "none");
    assert_eq!(
        single_type_sent(&all_257),
        without_subject_types,
        "257 subject types"
    );
    assert_eq!(
        single_type_sent(&all_257[..256])["resource"]["properties"]["event_subject_type_patterns"],
        json!(all_257[..256]),
        "256 subject types"
    );
}

#[test]
fn an_empty_candidate_list_is_sent_as_empty() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(Vec::new()),
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [],
                    "event_subject_type_patterns": [],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

#[tokio::test]
async fn the_memo_entry_lives_exactly_the_configured_ttl() {
    let memo = Arc::new(ScopeMemo::new());
    let ttl = Duration::from_secs(60);
    let writer = ConsumeMemo::new(Arc::clone(&memo), ttl);
    let request = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![candidate(CREATED, &[ORDER])]),
    )
    .e_call()
    .evaluation_request(&caller(), &enforcer());
    let scope = Arc::new(
        CompiledScope::compile(
            &toolkit_security::AccessScope::for_tenant(TENANT),
            &authz_resolver_sdk::TenantMode::RootOnly,
            &TenantSetInterner::new(),
        )
        .expect("a tenant scope compiles"),
    );
    let now = Instant::now();

    writer.write(&request, MemoResult::Scope(Arc::clone(&scope)), now);

    let key = MemoKey::try_from(&request).expect("a request keys the memo");
    assert_eq!(
        memo.get(&key, now + ttl - Duration::from_millis(1)),
        Some(MemoResult::Scope(scope)),
        "one millisecond before the TTL ends"
    );
    assert_eq!(memo.get(&key, now + ttl), None, "when the TTL ends");
}

// -- post-check of event types --

/// `CREATED` as a selector matches the derived `EXPRESS_CREATED`, so a scope
/// that lists only the derived type still admits events the interest selects.
#[test]
fn a_scope_listing_only_a_derived_event_type_admits_the_exact_selector() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![
            candidate(CREATED, &[ORDER]),
            candidate(EXPRESS_CREATED, &[ORDER]),
        ]),
    );
    let scope = compiled(vec![vec![ids_in(EVENT_TYPE, &[EXPRESS_CREATED])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// `CREATED` as a selector matches itself and both types derived from it, so
/// E names all three; `SHIPPED` is a sibling, matched by no selector.
#[test]
fn a_base_selector_sends_its_derived_candidates_and_no_sibling() {
    let group = group(
        &[interest(BarrierMode::Respect, &[CREATED], &[])],
        EventTypeCandidates::Listed(vec![
            candidate(CREATED, &[ORDER]),
            candidate(EXPRESS_CREATED, &[INVOICE]),
            candidate(RUSH_CREATED, &[ORDER]),
            candidate(SHIPPED, &[AUDIT]),
        ]),
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [CREATED],
                    "event_types": [CREATED, EXPRESS_CREATED, RUSH_CREATED],
                    "event_subject_type_patterns": [INVOICE, ORDER],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

/// `EXPRESS_CREATED` as a selector matches only itself among the candidates:
/// not its base `CREATED`, not its sibling `RUSH_CREATED` and not `SHIPPED`,
/// so E names it alone, with only the subject types it allows.
#[test]
fn a_derived_selector_sends_only_its_own_candidate() {
    let group = group(
        &[interest(BarrierMode::Respect, &[EXPRESS_CREATED], &[])],
        EventTypeCandidates::Listed(vec![
            candidate(CREATED, &[ORDER]),
            candidate(EXPRESS_CREATED, &[INVOICE]),
            candidate(RUSH_CREATED, &[ORDER]),
            candidate(SHIPPED, &[AUDIT]),
        ]),
    );

    assert_eq!(
        sent(&group.e_call()),
        json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [EXPRESS_CREATED],
                    "event_types": [EXPRESS_CREATED],
                    "event_subject_type_patterns": [INVOICE],
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": true,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })
    );
}

/// The scope admits `CREATED` and `RUSH_CREATED` but not `EXPRESS_CREATED`.
/// The base selector still has admitted types, so the JOIN passes, and all
/// three stay candidates of the member scope: delivery then decides each from
/// the scope, skipping `EXPRESS_CREATED`, without asking again.
#[test]
fn a_scope_excluding_one_derived_type_passes_the_post_check_of_its_base() {
    let selector = interest(BarrierMode::Respect, &[CREATED], &[]);
    let group = group(
        std::slice::from_ref(&selector),
        EventTypeCandidates::Listed(vec![
            candidate(CREATED, &[ORDER]),
            candidate(EXPRESS_CREATED, &[ORDER]),
            candidate(RUSH_CREATED, &[ORDER]),
        ]),
    );
    let scope = Arc::new(compiled(vec![vec![ids_in(
        EVENT_TYPE,
        &[CREATED, RUSH_CREATED],
    )]]));
    let expires_at = Instant::now() + Duration::from_mins(1);

    let checked = group.post_check(&scope);
    let authorized = group.authorized(Arc::clone(&scope), expires_at);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
    assert_eq!(
        authorized,
        AuthorizedGroup {
            key: GroupKey::new(&selector, CALLER_TENANT),
            scope,
            candidates: Arc::new(
                [CREATED, EXPRESS_CREATED, RUSH_CREATED]
                    .into_iter()
                    .map(GtsTypeId::new)
                    .collect()
            ),
            candidates_stale: false,
            expires_at,
        }
    );
}

// -- post-check of subject types --

/// `ORDER` as a selector covers `SPECIAL_ORDER`, so a scope that lists only
/// the derived type still admits events the interest selects.
#[test]
fn a_scope_listing_only_a_derived_subject_type_admits_the_exact_selector() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[SPECIAL_ORDER])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// No candidate allows `AUDIT` or a type under it, so publish never stores an
/// event the selector matches and a refusal would tell the caller nothing.
#[test]
fn a_subject_type_no_candidate_allows_is_not_post_checked() {
    let group = created_group(&[AUDIT], &[INVOICE]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

#[test]
fn a_subject_type_the_scope_excludes_entirely_is_refused() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE, AUDIT])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// The allowed wildcard covers `ORDER`, so the candidate may carry events the
/// selector matches, and the scope excluding them is refused.
#[test]
fn a_wildcard_allowed_pattern_makes_the_selector_checked() {
    let group = created_group(&[ORDER], &["gts.x.*"]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// The candidate allows only `SPECIAL_ORDER`, which the `ORDER` selector
/// covers, so the selector is checked.
#[test]
fn an_exact_allowed_type_below_the_selector_makes_it_checked() {
    let group = created_group(&[ORDER], &[SPECIAL_ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

#[test]
fn a_constraint_without_a_subject_type_filter_admits_any_selector() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_TYPE, &[CREATED])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// The constraint's first filter covers `ORDER` through `SPECIAL_ORDER`, its
/// second does not, and an event must pass both, so it admits nothing the
/// selector matches.
#[test]
fn every_subject_type_filter_of_a_constraint_must_cover_the_selector() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![
        ids_in(EVENT_SUBJECT_TYPE, &[SPECIAL_ORDER]),
        ids_in(EVENT_SUBJECT_TYPE, &[INVOICE]),
    ]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

#[test]
fn a_wildcard_selector_is_never_post_checked() {
    let group = created_group(&["gts.x.eb.joinscope.*"], &[ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// An exact selector that names an instance is not a subject type, so it is
/// not checked, though the allowed `ORDER` covers it.
#[test]
fn an_exact_instance_subject_type_selector_is_not_post_checked() {
    let group = created_group(
        &["gts.x.eb.joinscope.order.v1~x.eb.joinscope.o1.v1"],
        &[ORDER],
    );
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// The candidate allows only types derived from `ORDER` under the `x`
/// vendor, all of which the `ORDER` selector covers, so the selector is
/// checked.
#[test]
fn an_allowed_wildcard_narrower_than_the_selector_makes_it_checked() {
    let group = created_group(&[ORDER], &["gts.x.eb.joinscope.order.v1~x.*"]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// The candidate allows any minor version of `ORDER` v1, which the `ORDER`
/// selector covers, so the selector is checked.
#[test]
fn an_allowed_minor_version_wildcard_makes_the_selector_checked() {
    let group = created_group(&[ORDER], &["gts.x.eb.joinscope.order.v1.*"]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// Each filter names a type under `ORDER`, but no type is named by both, and
/// an event must pass both, so the constraint admits nothing.
#[test]
fn two_subject_type_filters_naming_different_types_do_not_admit() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![
        ids_in(EVENT_SUBJECT_TYPE, &[SPECIAL_ORDER]),
        ids_in(EVENT_SUBJECT_TYPE, &[RUSH_ORDER]),
    ]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// The scope admits only `RUSH_ORDER`, which the candidate does not allow,
/// so publish never stores an event the scope lets through.
#[test]
fn a_scope_type_no_candidate_allows_does_not_admit() {
    let group = created_group(&[ORDER], &[SPECIAL_ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[RUSH_ORDER])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// The scope admits only `INVOICE`, which the candidate allows but the
/// `ORDER` selector does not cover.
#[test]
fn an_allowed_scope_type_outside_the_selector_does_not_admit() {
    let group = created_group(&[ORDER], &[ORDER, INVOICE]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[INVOICE])]]);

    let checked = group.post_check(&scope);

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = checked
    else {
        panic!("expected Forbidden, got {checked:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::SubjectTypeNotAuthorized,
            "calling principal lacks consume on subject type 'gts.x.eb.joinscope.order.v1~'",
            "gts.x.eb.joinscope.order.v1~",
        )
    );
}

/// `SPECIAL_ORDER` is named by both filters, allowed, and under `ORDER`, so
/// an event of it passes the constraint.
#[test]
fn two_filters_sharing_an_allowed_type_under_the_selector_admit() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![
        ids_in(EVENT_SUBJECT_TYPE, &[SPECIAL_ORDER, RUSH_ORDER]),
        ids_in(EVENT_SUBJECT_TYPE, &[SPECIAL_ORDER]),
    ]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}

/// `ORDER` allows and covers its minor version `ORDER_MINOR`.
#[test]
fn an_allowed_minor_version_of_the_selector_admits() {
    let group = created_group(&[ORDER], &[ORDER]);
    let scope = compiled(vec![vec![ids_in(EVENT_SUBJECT_TYPE, &[ORDER_MINOR])]]);

    let checked = group.post_check(&scope);

    assert!(matches!(checked, Ok(())), "expected Ok, got {checked:?}");
}
